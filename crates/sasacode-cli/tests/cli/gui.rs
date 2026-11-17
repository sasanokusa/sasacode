//! packages/cli/test/gui.test.ts. `sasacode gui [folder]`: starts sasacode-gui on the folder and
//! leaves it running on its own.

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
#[cfg(unix)]
use std::time::{Duration, Instant};

use crate::support::bin;

/// A place to run from: a home, a project, and a folder for stand-in programs.
struct Place {
    _root: tempfile::TempDir,
    home: PathBuf,
    proj: PathBuf,
    bin: PathBuf,
}

fn place() -> Place {
    let root = tempfile::Builder::new().prefix("sasacode-gui-cli-").tempdir().unwrap();
    // As the program sees its folder (`pwd`): symbolic links resolved.
    let real = root.path().canonicalize().unwrap();
    let (home, proj, bin) = (real.join("home"), real.join("proj"), real.join("bin"));
    for d in [&home, &proj.join(".sasacode"), &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(home.join(".env"), "").unwrap();
    Place {
        _root: root,
        home,
        proj,
        bin,
    }
}

/// A stand-in for sasacode-gui that stays for a while, as a window does: it writes where it was
/// started and what it was given, and `<out>.stayed` when it leaves.
#[cfg(unix)]
fn fake_gui(dir: &Path, name: &str, out: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let command = dir.join(name);
    let out = out.display();
    let script = format!(
        "#!/bin/sh\n\
         {{ pwd; for a in \"$@\"; do printf '%s\\n' \"$a\"; done; }} > \"{out}.part\"\n\
         mv \"{out}.part\" \"{out}\"\n\
         sleep 2\n\
         : > \"{out}.stayed\"\n"
    );
    std::fs::write(&command, script).unwrap();
    std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755)).unwrap();
    command
}

#[cfg(unix)]
fn appears(path: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    path.exists()
}

#[cfg(unix)]
fn stayed(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.stayed", path.display()))
}

#[cfg(unix)]
fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .trim_end()
        .split('\n')
        .map(str::to_string)
        .collect()
}

struct Ran {
    #[cfg_attr(not(unix), allow(dead_code))] // read by the Unix-only tests
    out: String,
    err: String,
    code: Option<i32>,
}

fn sasacode(args: &[&str], at: &Place, env: &[(&str, &str)]) -> Ran {
    let out = Command::new(bin())
        .args(args)
        .current_dir(&at.proj)
        .env_clear()
        // Only the stand-ins and the system's own tools: a sasacode-gui installed on this machine is not found.
        .env("PATH", format!("{}:/usr/bin:/bin", at.bin.display()))
        .env("HOME", at.home.parent().unwrap())
        .env("USERPROFILE", at.home.parent().unwrap())
        .env("SASACODE_HOME", &at.home)
        .env("SASACODE_LANG", "en")
        .env("NO_COLOR", "1")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    Ran {
        out: String::from_utf8_lossy(&out.stdout).into_owned(),
        err: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code(),
    }
}

#[cfg(unix)]
/// What the window is given: `--sasacode` and this sasacode, unless it runs from the npm launcher
/// (SASACODE_BIN), then the folder.
fn given_args(folder: &Path, own: bool) -> Vec<String> {
    let folder = folder.display().to_string();
    let mut args = vec![folder.clone()]; // pwd
    if own {
        args.extend(["--sasacode".to_string(), bin().to_string()]);
    }
    args.push(folder);
    args
}

// The stand-ins are shell scripts: there is no such thing on Windows.
#[cfg(unix)]
#[test]
fn gui_the_window_is_started_on_the_folder_and_is_still_there_after_sasacode_has_left() {
    let at = place();
    let given = at.bin.join("given");
    fake_gui(&at.bin, "sasacode-gui", &given);
    let other = at.proj.join("pkg");
    std::fs::create_dir(&other).unwrap();

    let here = sasacode(&["gui"], &at, &[]);
    assert_eq!((here.out.as_str(), here.err.as_str(), here.code), ("", "", Some(0)));
    assert!(appears(&given));
    // The Rust sasacode is a program of its own: the window is told which one to run. (From TS source
    // there was none to name, and the window found its own.)
    assert_eq!(lines(&given), given_args(&at.proj, true));
    // sasacode is gone, and the window had not left yet: it leaves later, by itself.
    assert!(!stayed(&given).exists());
    assert!(appears(&stayed(&given)));

    std::fs::remove_file(&given).unwrap();
    assert_eq!(sasacode(&["gui", "pkg"], &at, &[]).code, Some(0));
    assert!(appears(&given));
    assert_eq!(lines(&given), given_args(&other, true));

    // Run from the npm launcher, the launcher is the program to name, not this binary: none is named.
    std::fs::remove_file(&given).unwrap();
    assert_eq!(
        sasacode(&["gui"], &at, &[("SASACODE_BIN", "/elsewhere/sasacode")]).code,
        Some(0)
    );
    assert!(appears(&given));
    assert_eq!(lines(&given), given_args(&at.proj, false));
}

#[cfg(unix)]
#[test]
fn gui_the_program_is_the_one_the_users_config_names_never_one_a_projects_config_names() {
    let at = place();
    let mine = fake_gui(&at.bin, "my-gui", &at.bin.join("mine"));
    let theirs = fake_gui(&at.bin, "their-gui", &at.bin.join("theirs"));
    let project = serde_json::json!({ "plugins": { "settings": { "gui": { "command": theirs } } } });
    std::fs::write(at.proj.join(".sasacode").join("config.json"), project.to_string()).unwrap();

    // Nothing of the user's says where it is, and none is on PATH: the project's is not started.
    let none = sasacode(&["gui"], &at, &[]);
    assert_eq!(none.code, Some(1));
    assert!(none.err.contains("sasacode-gui not found"), "{}", none.err);
    assert!(none.err.contains("plugins.settings.gui.command"), "{}", none.err);

    let user = serde_json::json!({ "plugins": { "settings": { "gui": { "command": mine } } } });
    std::fs::write(at.home.join("config.json"), user.to_string()).unwrap();
    assert_eq!(sasacode(&["gui"], &at, &[]).code, Some(0));
    assert!(appears(&at.bin.join("mine")));
    assert!(!at.bin.join("theirs").exists());
}

#[test]
fn gui_what_is_not_a_folder_and_more_than_a_folder_are_turned_down_before_anything_starts() {
    let at = place();
    let missing = sasacode(&["gui", "nowhere"], &at, &[]);
    assert_eq!(missing.code, Some(1));
    assert!(missing.err.contains("nowhere is not a folder"), "{}", missing.err);
    let many = sasacode(&["gui", ".", "--session"], &at, &[]);
    assert_eq!(many.code, Some(1));
    assert!(many.err.contains("usage: sasacode gui [folder]"), "{}", many.err);
}
