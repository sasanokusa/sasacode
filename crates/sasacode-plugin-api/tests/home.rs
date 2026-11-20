//! Where sasacode keeps its files, with and without a home directory. The environment is process-wide
//! and changing it needs `unsafe` (edition 2024), so each case runs this test binary again with the
//! environment it names, and the child prints what the functions answered.

use std::process::Command;

const CHILD: &str = "SASACODE_HOME_TEST_CHILD";

/// In the child: print `sasacode_home()` and `home_dir()`.
fn answer() {
    println!("home={}", sasacode_plugin_api::sasacode_home().display());
    println!("dir={:?}", sasacode_plugin_api::home_dir());
}

/// The child's (exit code, stdout, stderr) with exactly `env` set.
fn run(env: &[(&str, &str)]) -> (Option<i32>, String, String) {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["child_answers", "--exact", "--nocapture", "--test-threads=1"])
        .env_clear()
        .env(CHILD, "1")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn child_answers() {
    if std::env::var_os(CHILD).is_some() {
        answer();
    }
}

#[test]
fn without_any_home_it_exits_and_says_to_set_sasacode_home() {
    for env in [
        &[][..],
        &[("HOME", "")],
        &[("HOME", ""), ("USERPROFILE", ""), ("SASACODE_HOME", "")],
    ] {
        let (code, out, err) = run(env);
        assert_eq!(code, Some(1), "{env:?}: {out}{err}");
        assert!(err.contains("set SASACODE_HOME"), "{env:?}: {err}");
        assert!(!out.contains("home="), "{env:?} went on with {out}");
    }
}

#[test]
fn sasacode_home_alone_is_enough_and_a_home_directory_is_not_invented() {
    let (code, out, _) = run(&[("SASACODE_HOME", "/s")]);
    assert_eq!(code, Some(0));
    assert!(out.contains("home=/s\n") && out.contains("dir=None"), "{out}");
    // An empty SASACODE_HOME is no value: the home directory decides.
    let (_, out, _) = run(&[("SASACODE_HOME", ""), ("HOME", "/h")]);
    assert!(
        out.contains("home=/h/.sasacode\n") && out.contains("dir=Some(\"/h\")"),
        "{out}"
    );
}

#[test]
fn the_home_directory_is_home_then_userprofile() {
    let (_, out, _) = run(&[("HOME", "/h"), ("USERPROFILE", "/u")]);
    assert!(out.contains("home=/h/.sasacode\n"), "{out}");
    let (_, out, _) = run(&[("HOME", ""), ("USERPROFILE", "/u")]);
    assert!(out.contains("home=/u/.sasacode\n"), "{out}");
    let (_, out, _) = run(&[("SASACODE_HOME", "/s"), ("HOME", "/h")]);
    assert!(out.contains("home=/s\n"), "{out}");
}
