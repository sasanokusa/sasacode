//! packages/cli/test/commands.test.ts: flags in any position, piped input that arrives late, and the
//! keys of ~/.sasacode/.env. Its three `sasacode plugin` and plugin-folder tests are dropped with the
//! plugin installer (rust/DECISIONS.md ADR-002).

use std::io::Write;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use sasacode_cli::endpoints::endpoint_command_in;
use sasacode_testing::{FakeServer, Reply};

use crate::support::{Bin, chunk, fake_config, provider, sse, sse_body};
use crate::test_server::{Res, TestServer};

#[tokio::test(flavor = "multi_thread")]
async fn endpoint_flags_may_come_before_the_arguments_key_env_takes_its_value() {
    sasacode_host::set_lang(sasacode_plugin_api::Lang::En); // these assertions quote the English strings
    let server = TestServer::start(|_| {
        Res::json(json!({ "object": "list", "data": [{ "id": "m", "object": "model", "max_model_len": 32768 }] }))
    });
    let root = tempfile::tempdir().unwrap();
    let (home, proj) = (root.path().join("home"), root.path().join("proj"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&proj).unwrap();
    let url = server.url();
    let args: Vec<String> = ["add", "--key-env", "LAN_KEY", "lan", &url]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = endpoint_command_in(&args, &proj, &home, &mut out, &mut err)
        .await
        .unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out), String::from_utf8_lossy(&err));
    assert_eq!(code, 0, "{said}");
    assert!(said.contains("(32768 ctx)"), "{said}"); // vLLM's max_model_len
    let config: Value = serde_json::from_str(&std::fs::read_to_string(home.join("config.json")).unwrap()).unwrap();
    let lan = &config["providers"]["lan"];
    assert_eq!(
        (&lan["apiKeyEnv"], &lan["api"]),
        (&json!("LAN_KEY"), &json!("openai-chat")),
        "{lan}"
    );
}

/// The text of the user message in a chat request, as the model got it.
fn prompt_sent(request: &Value) -> String {
    let user = request["messages"]
        .as_array()
        .and_then(|m| m.iter().find(|m| m["role"] == "user"))
        .cloned()
        .unwrap_or_default();
    match &user["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect(),
        _ => String::new(),
    }
}

#[test]
fn piped_input_that_arrives_after_a_second_is_still_read_a_file_is_read_whole() {
    let server = FakeServer::start(vec![Reply::text("ok"), Reply::text("ok")]).expect("the fake server");
    let bin = Bin::new(&fake_config(server.url(), json!({})));
    let file = bin.sb.root().join("in.txt");
    std::fs::write(&file, "from a file").unwrap();
    let out = bin
        .command(["-p", "say", "--no-session"])
        .stdin(std::fs::File::open(&file).unwrap())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(prompt_sent(&server.requests()[0]), "say\n\nfrom a file");
    // A pipe that stays silent for a second, then writes.
    let mut child = bin
        .command(["-p", "say", "--no-session"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        stdin.write_all(b"LATE\n").unwrap();
    });
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(prompt_sent(&server.requests()[1]), "say\n\nLATE\n");
}

/// One streamed Chat Completions reply: a call to `name` with `args`.
fn tool_call(name: &str, args: Value) -> String {
    let function = json!({ "name": name, "arguments": args.to_string() });
    let call = json!({ "tool_calls": [{ "index": 0, "id": "call_1", "type": "function", "function": function }] });
    sse_body(&[chunk("m", call, None), chunk("m", json!({}), Some("tool_calls"))])
}

/// The chat requests a server was sent: the Authorization header and the body.
type Seen = Arc<Mutex<Vec<(Option<String>, Value)>>>;

#[test]
fn keys_read_from_the_sasacode_env_file_stay_out_of_commands_the_agent_runs() {
    // The model runs a command that prints the key and PATH; sasacode itself sends the key.
    let requests: Seen = Arc::new(Mutex::new(Vec::new()));
    let server = {
        let requests = requests.clone();
        TestServer::start(move |req| {
            if req.path != "/v1/chat/completions" {
                return Res::text(404, "");
            }
            let mut seen = requests.lock().unwrap();
            seen.push((req.header("authorization").map(str::to_string), req.json()));
            if seen.len() == 1 {
                let command = r#"printf 'secret=[%s] path=[%s]' "$SASACODE_TEST_SECRET" "$PATH""#;
                sse(tool_call("bash", json!({ "command": command })))
            } else {
                sse(crate::support::sse_answer("m", "done"))
            }
        })
    };
    let mut fake = provider("openai-chat", &format!("{}/v1", server.url()));
    fake["apiKeyEnv"] = json!("SASACODE_TEST_SECRET");
    let bin = Bin::new(&json!({
        "providers": { "fake": fake },
        "model": "fake/m",
        "maxRetries": 0,
        "permissions": { "mode": "auto" },
        "plugins": { "disabled": ["openai-codex"] },
    }));
    std::fs::write(bin.sb.home.join(".env"), "SASACODE_TEST_SECRET=abc\n").unwrap();
    let r = bin.run(["-p", "show me", "--no-session"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let seen = requests.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    // sasacode has the key: it sent it.
    assert_eq!(seen[0].0.as_deref(), Some("Bearer abc"));
    // The command did not get it, and got PATH as sasacode was given it.
    let result = seen[1].1["messages"]
        .as_array()
        .and_then(|m| m.last())
        .map(Value::to_string)
        .unwrap_or_default();
    assert!(result.contains("secret=[]"), "{result}");
    // On Windows bash is MSYS's, which writes PATH its own way (/c/...:/c/...).
    if cfg!(unix) {
        assert!(result.contains(&format!("path=[{}]", bin.var("PATH"))), "{result}");
    }
    // Variables passed on explicitly still go through, and PATH as this process has it, under the
    // name the OS spells it (Windows says Path, and matches it without case).
    let env = sasacode_plugin_api::child_env(&[("X".into(), "1".into())]);
    assert_eq!(env.get("X").map(String::as_str), Some("1"));
    let path = env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("PATH"))
        .map(|(_, v)| v.clone());
    assert_eq!(path, std::env::var("PATH").ok());
}
