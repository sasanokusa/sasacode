//! The `sasacode-fake-model` binary: the home it writes, the environment and exit code it passes on,
//! and the requests it records. The command is a shell script standing in for sasacode.

#[cfg(unix)]
mod unix {
    use std::process::Command;

    use serde_json::{Value, json};

    const BIN: &str = env!("CARGO_BIN_EXE_sasacode-fake-model");

    const CHILD: &str = r#"
cp "$SASACODE_HOME/config.json" "$1/config.json"
printf '%s|%s|%s|%s|%s|%s\n' "$HOME" "$USERPROFILE" "$SASACODE_HOME" "$SASACODE_LANG" "$NO_COLOR" "$(cat "$SASACODE_HOME/.env")"
url=$(sed -n 's/.*"baseUrl": "\(.*\)".*/\1/p' "$SASACODE_HOME/config.json")
command -v curl >/dev/null && curl -s -X POST -d '{"model":"m","n":1}' "$url/chat/completions"
read line && echo "stdin:$line"
exit 7
"#;

    #[test]
    fn it_runs_the_command_in_a_fake_home_and_exits_with_its_code() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("script.json");
        std::fs::write(&script, r#"[{"text":"hello"}]"#).unwrap();
        let requests = dir.path().join("requests.jsonl");
        let out = Command::new(BIN)
            .args([
                "--script",
                script.to_str().unwrap(),
                "--config",
                r#"{"permissions":{"mode":"edits"},"maxRetries":2}"#,
            ])
            .args([
                "--requests",
                requests.to_str().unwrap(),
                "--",
                "sh",
                "-c",
                CHILD,
                "sh",
                dir.path().to_str().unwrap(),
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(b"from the parent\n")?;
                child.wait_with_output()
            })
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            out.status.code(),
            Some(7),
            "{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let config: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("config.json")).unwrap()).unwrap();
        let url = config["providers"]["fake"]["baseUrl"].as_str().unwrap().to_string();
        assert!(url.starts_with("http://127.0.0.1:") && url.ends_with("/v1"), "{url}");
        assert_eq!(
            config,
            json!({
                "providers": { "fake": { "api": "openai-chat", "baseUrl": url } },
                "model": "fake/m",
                "maxRetries": 2,
                "permissions": { "mode": "edits" },
            })
        );
        let env: Vec<&str> = stdout.lines().next().unwrap().split('|').collect();
        assert_eq!(env[0], env[1], "HOME and USERPROFILE are the temporary root");
        assert_eq!(env[2], format!("{}/home", env[0]));
        assert_eq!(&env[3..], ["en", "1", ""]);
        assert!(stdout.contains("stdin:from the parent"), "{stdout}");
        // The temporary root is gone.
        assert!(!std::path::Path::new(env[0]).exists());

        if stdout.contains("data: ") {
            assert!(stdout.contains(r#""delta":{"content":"he"}"#), "{stdout}");
            assert_eq!(
                std::fs::read_to_string(&requests).unwrap(),
                "{\"model\":\"m\",\"n\":1}\n"
            );
        } else {
            eprintln!("curl is not on the PATH: the request part was not checked");
            assert_eq!(std::fs::read_to_string(&requests).unwrap(), "");
        }
    }

    #[test]
    fn usage_errors_exit_2_and_help_explains_the_script() {
        let out = Command::new(BIN).args(["--script", "x.json"]).output().unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("no command"));
        let out = Command::new(BIN).arg("--help").output().unwrap();
        assert_eq!(out.status.code(), Some(0));
        assert!(String::from_utf8_lossy(&out.stdout).contains("toolCallShape"));
        let out = Command::new(BIN)
            .args(["--script", "/nonexistent.json", "--", "true"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stderr).contains("could not read /nonexistent.json"));
    }
}
