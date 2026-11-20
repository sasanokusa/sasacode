//! Where sasacode keeps its files, with and without a home directory. The environment is process-wide
//! and changing it needs `unsafe` (edition 2024), so each case runs this test binary again with the
//! environment it names, and the child prints what the functions answered.

#![cfg(unix)]

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

/// In the child: a week-old folder in tmp/ (as the MCP servers' own folders are) survives the prune
/// that `save_output` makes of the old files there.
fn prune_keeps_folders() {
    let tmp = std::path::PathBuf::from(std::env::var_os("SASACODE_HOME").unwrap()).join("tmp");
    let dir = tmp.join("mcp").join("server");
    let old_file = tmp.join("old.log");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&old_file, "x").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86_400);
    for p in [&dir, &old_file, &tmp.join("mcp")] {
        std::fs::File::open(p).unwrap().set_modified(old).unwrap();
    }
    sasacode_plugin_api::save_output("t", "y").unwrap();
    assert!(!old_file.exists(), "the old file is pruned");
    assert!(dir.is_dir(), "the folder is kept");
}

#[test]
fn child_prunes() {
    if std::env::var_os(CHILD).is_some() {
        prune_keeps_folders();
    }
}

#[test]
fn pruning_old_files_in_tmp_leaves_folders_alone() {
    let home = std::env::temp_dir().join(format!("sasacode-home-test-{}", std::process::id()));
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["child_prunes", "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .env("SASACODE_HOME", &home)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&home);
    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && text.contains("1 passed"), "{text}");
}
