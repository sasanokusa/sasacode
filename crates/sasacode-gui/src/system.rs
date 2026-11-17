//! What the window asks of the system: showing a folder, a terminal or an editor there, saying
//! that something happened while the window is behind others, keeping the computer awake while a
//! run goes on, and opening another window. Each is a program the system has; where it has none,
//! nothing happens.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// Starts `program` and leaves it be: what it prints is not read, and the window does not wait.
fn spawn(mut command: Command) -> bool {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

fn command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    // On Windows, a console program started from a window would open a console of its own.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
}

/// `path` opened by the program the system opens it with.
pub fn open(path: &Path) -> bool {
    if cfg!(target_os = "macos") {
        spawn_with(command("open"), path)
    } else if cfg!(windows) {
        let mut c = command("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        spawn_with(c, path)
    } else {
        spawn_with(command("xdg-open"), path)
    }
}

/// The file manager, at `path`: a folder opened, a file shown in its folder.
pub fn reveal(path: &Path) -> bool {
    if cfg!(target_os = "macos") {
        let mut c = command("open");
        if path.is_file() {
            c.arg("-R");
        }
        spawn_with(c, path)
    } else if cfg!(windows) {
        let mut c = command("explorer");
        if path.is_file() {
            c.arg(format!("/select,{}", path.display()));
            return spawn(c);
        }
        spawn_with(c, path)
    } else {
        spawn_with(
            command("xdg-open"),
            if path.is_file() {
                path.parent().unwrap_or(path)
            } else {
                path
            },
        )
    }
}

fn spawn_with(mut c: Command, path: &Path) -> bool {
    c.arg(path);
    spawn(c)
}

/// A terminal in `dir`.
pub fn terminal(dir: &Path) -> bool {
    if cfg!(target_os = "macos") {
        let mut c = command("open");
        c.args(["-a", "Terminal"]).arg(dir);
        spawn(c)
    } else if cfg!(windows) {
        let mut c = command("cmd");
        c.args(["/C", "start", "cmd"]).current_dir(dir);
        spawn(c)
    } else {
        // Debian's alternatives name the user's terminal; else the usual ones, in turn.
        ["x-terminal-emulator", "gnome-terminal", "konsole", "xterm"]
            .iter()
            .any(|program| {
                let mut c = command(program);
                c.current_dir(dir);
                spawn(c)
            })
    }
}

/// A terminal in `dir` going on with conversation `id` in sasacode's TUI.
pub fn resume_in_terminal(dir: &Path, program: &Path, id: &str) -> bool {
    // Quoted for the shell: what is between single quotes is taken as it is.
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let line = format!(
        "cd {} && exec {} -r {}",
        quote(&dir.to_string_lossy()),
        quote(&program.to_string_lossy()),
        quote(id)
    );
    if cfg!(target_os = "macos") {
        // A `.command` file is run by Terminal when opened: no permission to script it is asked.
        let script = std::env::temp_dir().join(format!("sasacode-resume-{}.command", id.replace('/', "_")));
        let written = std::fs::write(&script, format!("#!/bin/sh\n{line}\n")).is_ok() && {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).is_ok()
            }
            #[cfg(not(unix))]
            true
        };
        let mut c = command("open");
        c.args(["-a", "Terminal"]).arg(&script);
        written && spawn(c)
    } else if cfg!(windows) {
        let mut c = command("cmd");
        c.args(["/C", "start", "cmd", "/K"])
            .arg(format!("{} -r {id}", program.display()))
            .current_dir(dir);
        spawn(c)
    } else {
        ["x-terminal-emulator", "gnome-terminal", "konsole", "xterm"]
            .iter()
            .any(|program| {
                let mut c = command(program);
                c.args(["-e", "sh", "-c", &line]).current_dir(dir);
                spawn(c)
            })
    }
}

/// `path` in the editor the settings name (a command, with its arguments), else as [`editor`]
/// opens it.
pub fn editor_with(path: &Path, chosen: &str) -> bool {
    let mut words = chosen.split_whitespace();
    if let Some(program) = words.next() {
        let mut c = command(program);
        c.args(words);
        if spawn_with(c, path) {
            return true;
        }
    }
    editor(path)
}

/// `path` in an editor: VS Code when it is on the PATH, else what the system opens text with.
pub fn editor(path: &Path) -> bool {
    let mut code = command(if cfg!(windows) { "code.cmd" } else { "code" });
    code.arg(path);
    if spawn(code) {
        return true;
    }
    if cfg!(target_os = "macos") {
        let mut c = command("open");
        // A folder in Finder; a file in the text editor rather than whatever its kind opens with.
        if path.is_file() {
            c.arg("-t");
        }
        spawn_with(c, path)
    } else if cfg!(windows) {
        let mut c = command("cmd");
        c.args(["/C", "start", ""]).arg(path);
        spawn(c)
    } else {
        spawn_with(command("xdg-open"), path)
    }
}

/// A notification from the system, for when the window is behind others. None on Windows, where
/// the window asks for attention instead (its taskbar button flashes), as it does everywhere.
pub fn notify(title: &str, body: &str, sound: bool) {
    if cfg!(target_os = "macos") {
        // AppleScript's strings take `\` and `"` escaped.
        let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "display notification \"{}\" with title \"{}\"{}",
            quote(body),
            quote(title),
            if sound { " sound name \"Glass\"" } else { "" }
        );
        let mut c = command("osascript");
        c.args(["-e", &script]);
        spawn(c);
    } else if !cfg!(windows) {
        let mut c = command("notify-send");
        c.args(["--app-name=sasacode", title, body]);
        spawn(c);
    }
}

/// Keeps the computer from sleeping while it is held: macOS's `caffeinate`, else systemd's
/// inhibitor. Let go when dropped, and when the window's process ends however it ends.
pub struct Awake(Child);

impl Awake {
    pub fn hold() -> Option<Awake> {
        let pid = std::process::id().to_string();
        let mut c = if cfg!(target_os = "macos") {
            let mut c = command("caffeinate");
            // Idle sleep only: the display may still sleep. Ends with this process.
            c.args(["-i", "-w", &pid]);
            c
        } else if cfg!(windows) {
            return None;
        } else {
            let mut c = command("systemd-inhibit");
            c.args([
                "--what=idle:sleep",
                "--who=sasacode",
                "--why=a run is going on",
                "tail",
                "--pid",
                &pid,
                "-f",
                "/dev/null",
            ]);
            c
        };
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        c.spawn().ok().map(Awake)
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// How long reading the clipboard may take, as the TUI waits for it.
const PASTE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// What `c` prints when it ends well within `wait`; none when it fails or takes longer, when it is
/// stopped.
fn bounded(mut c: Command, wait: std::time::Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut child = c
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Read meanwhile: an image is more than a pipe holds, and the program would wait for room.
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = out.read_to_end(&mut bytes);
        bytes
    });
    let until = std::time::Instant::now() + wait;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() < until => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let bytes = reader.join().unwrap_or_default();
    status.filter(|s| s.success()).map(|_| bytes)
}

/// How long an image pasted from the clipboard is kept, for the message it went into to be sent.
const PASTED_KEPT: std::time::Duration = std::time::Duration::from_secs(7 * 86_400);

/// The image on the system clipboard, written as a PNG file in `dir`, as the TUI reads it: macOS
/// through AppleScript, Windows through PowerShell, elsewhere `wl-paste` or `xclip`. None when the
/// clipboard has no image. Files pasted before and older than a week are removed.
pub fn clipboard_image(dir: &Path) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let now = std::time::SystemTime::now();
    if let Ok(old) = std::fs::read_dir(dir) {
        for f in old.filter_map(Result::ok) {
            let stale = f
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|at| now.duration_since(at).unwrap_or_default() > PASTED_KEPT);
            if stale {
                let _ = std::fs::remove_file(f.path());
            }
        }
    }
    let millis = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_millis();
    let file = dir.join(format!("pasted-{}-{millis:03}.png", stamp(now)));
    let quiet = |c: Command| bounded(c, PASTE_WAIT);
    if cfg!(target_os = "macos") {
        let path = file.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
        let open = format!("set fh to open for access POSIX file \"{path}\" with write permission");
        let mut c = command("osascript");
        for line in [
            "try",
            "set d to the clipboard as «class PNGf»",
            "on error",
            "error \"no image\"",
            "end try",
            &open,
            "set eof of fh to 0",
            "write d to fh",
            "close access fh",
        ] {
            c.args(["-e", line]);
        }
        quiet(c)?;
    } else if cfg!(windows) {
        // The clipboard needs a single-threaded apartment; '' is a ' inside a PowerShell string.
        let script = [
            "Add-Type -AssemblyName System.Windows.Forms, System.Drawing".to_owned(),
            "$i = [System.Windows.Forms.Clipboard]::GetImage()".to_owned(),
            "if ($i -eq $null) { exit 1 }".to_owned(),
            format!(
                "$i.Save('{}', [System.Drawing.Imaging.ImageFormat]::Png)",
                file.to_string_lossy().replace('\'', "''")
            ),
        ]
        .join("; ");
        let mut c = command("powershell.exe");
        c.args(["-NoProfile", "-NonInteractive", "-STA", "-Command", &script]);
        quiet(c)?;
    } else {
        let png = [
            ("wl-paste", &["--type", "image/png"][..]),
            ("xclip", &["-selection", "clipboard", "-t", "image/png", "-o"]),
        ]
        .iter()
        .find_map(|(program, args)| {
            let mut c = command(program);
            c.args(*args);
            quiet(c).filter(|png| !png.is_empty())
        })?;
        std::fs::write(&file, png).ok()?;
    }
    if std::fs::metadata(&file).is_ok_and(|m| m.len() > 0) {
        Some(file)
    } else {
        let _ = std::fs::remove_file(&file);
        None
    }
}

/// Another window of this program, on `cwd`, open on `session` if given, with its top left corner
/// at `at` (in the system's points) if given.
pub fn new_window(program: &Path, cwd: &Path, session: Option<&str>, at: Option<iced::Point>) -> bool {
    let Ok(me) = std::env::current_exe() else {
        return false;
    };
    let mut c = Command::new(me);
    c.arg("--sasacode").arg(program);
    if let Some(id) = session {
        c.args(["--session", id]);
    }
    if let Some(at) = at {
        c.arg("--at").arg(format!("{},{}", at.x.round(), at.y.round()));
    }
    c.arg(cwd);
    spawn(c)
}

/// A git worktree of the repository `cwd` is in, on a new branch from what is checked out, under
/// sasacode's home: where the same folder of it is, to be opened in a window of its own.
pub fn make_worktree(cwd: &Path, home: &Path) -> Result<PathBuf, String> {
    let git = |args: &[&std::ffi::OsStr]| -> Result<String, String> {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    };
    let top = PathBuf::from(git(&["rev-parse".as_ref(), "--show-toplevel".as_ref()])?);
    let name = top
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stamp = stamp(std::time::SystemTime::now());
    let dir = home.join("worktrees").join(format!("{name}-{stamp}"));
    let branch = format!("sasacode/{stamp}");
    git(&[
        "worktree".as_ref(),
        "add".as_ref(),
        "-b".as_ref(),
        branch.as_ref(),
        dir.as_os_str(),
    ])?;
    // The same folder inside it, where the window was opened on a folder inside the repository.
    let inside = cwd.canonicalize().ok().and_then(|c| {
        let top = top.canonicalize().ok()?;
        c.strip_prefix(top).ok().map(Path::to_path_buf)
    });
    Ok(inside.map(|rel| dir.join(rel)).filter(|d| d.is_dir()).unwrap_or(dir))
}

/// `20261117-093012`, in UTC: a name that sorts by when it was made.
fn stamp(at: std::time::SystemTime) -> String {
    let (date, time, _) = civil(at);
    format!("{}-{}", date.replace('-', ""), time.replace(':', ""))
}

/// `2026-11-17T09:30:12.345Z`, as JavaScript's `toISOString` writes it.
pub fn iso(at: std::time::SystemTime) -> String {
    let (date, time, millis) = civil(at);
    format!("{date}T{time}.{millis:03}Z")
}

/// The date (`2026-11-17`), the time (`09:30:12`) and the milliseconds of `at`, in UTC.
fn civil(at: std::time::SystemTime) -> (String, String, u32) {
    let since = at.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let (secs, millis) = (since.as_secs(), since.subsec_millis());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Howard Hinnant's days-to-civil.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (
        format!("{y:04}-{m:02}-{d:02}"),
        format!("{:02}:{:02}:{:02}", rest / 3600, rest % 3600 / 60, rest % 60),
        millis,
    )
}

/// The folders sasacode has kept conversations for, the most recently used first: the first
/// entry of the newest log in each of the home's session folders says where it ran.
pub fn projects(home: &Path) -> Vec<PathBuf> {
    let Ok(dirs) = std::fs::read_dir(home.join("sessions")) else {
        return Vec::new();
    };
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = dirs
        .filter_map(Result::ok)
        .filter_map(|dir| {
            let newest = std::fs::read_dir(dir.path())
                .ok()?
                .filter_map(Result::ok)
                .filter(|f| f.path().extension().is_some_and(|x| x == "jsonl"))
                .filter_map(|f| Some((f.metadata().ok()?.modified().ok()?, f.path())))
                .max_by_key(|(when, _)| *when)?;
            let first = std::fs::read_to_string(&newest.1).ok()?;
            let head: serde_json::Value = serde_json::from_str(first.lines().next()?).ok()?;
            let cwd = PathBuf::from(head["cwd"].as_str()?);
            cwd.is_dir().then_some((newest.0, cwd))
        })
        .collect();
    found.sort_by_key(|a| std::cmp::Reverse(a.0));
    found.dedup_by(|a, b| a.1 == b.1);
    found.into_iter().map(|(_, cwd)| cwd).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_is_the_date_and_time_in_utc() {
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_794_907_812);
        assert_eq!(stamp(at), "20261117-093012");
        assert_eq!(stamp(std::time::UNIX_EPOCH), "19700101-000000");
        let at = at + std::time::Duration::from_millis(7);
        assert_eq!(iso(at), "2026-11-17T09:30:12.007Z");
    }

    #[cfg(unix)]
    #[test]
    fn a_program_that_takes_too_long_is_stopped() {
        let start = std::time::Instant::now();
        let mut slow = Command::new("sleep");
        slow.arg("5");
        assert_eq!(bounded(slow, std::time::Duration::from_millis(200)), None);
        assert!(start.elapsed() < std::time::Duration::from_secs(3));
        let mut quick = Command::new("printf");
        quick.arg("abc");
        assert_eq!(bounded(quick, PASTE_WAIT).as_deref(), Some(&b"abc"[..]));
        assert_eq!(bounded(Command::new("false"), PASTE_WAIT), None);
    }

    #[test]
    fn a_paste_leaves_a_file_only_when_the_clipboard_had_an_image() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-pasted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // One pasted long ago is let go.
        let old = dir.join("pasted-old.png");
        std::fs::write(&old, b"x").unwrap();
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        // What the clipboard holds is the machine's; either way, no empty file is left.
        let left: Vec<_> = match clipboard_image(&dir) {
            Some(file) => {
                assert!(std::fs::metadata(&file).unwrap().len() > 0);
                vec![file]
            }
            None => Vec::new(),
        };
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|f| f.unwrap().path()).collect();
        assert_eq!(files, left);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_folders_with_conversations_are_listed_newest_first() {
        let home = std::env::temp_dir().join(format!("sasacode-gui-projects-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let (old, new) = (home.join("old"), home.join("new"));
        for (n, cwd) in [(0, &old), (1, &new)] {
            std::fs::create_dir_all(cwd).unwrap();
            let dir = home.join("sessions").join(format!("d{n}"));
            std::fs::create_dir_all(&dir).unwrap();
            let log = dir.join("x.jsonl");
            let head = serde_json::json!({ "type": "session", "id": "x", "cwd": cwd });
            std::fs::write(&log, format!("{head}\n")).unwrap();
            let then = std::time::SystemTime::now() - std::time::Duration::from_secs(600 * (1 - n));
            std::fs::File::options()
                .write(true)
                .open(&log)
                .unwrap()
                .set_modified(then)
                .unwrap();
        }
        // One whose folder is gone is left out.
        let gone = home.join("sessions").join("d2");
        std::fs::create_dir_all(&gone).unwrap();
        std::fs::write(
            gone.join("y.jsonl"),
            "{\"type\":\"session\",\"cwd\":\"/no/such/folder\"}\n",
        )
        .unwrap();
        assert_eq!(projects(&home), vec![new, old]);
        std::fs::remove_dir_all(home).unwrap();
    }
}

/// What sets the environment apart around a login shell's: not the user's choices.
const SHELL_OWN: [&str; 5] = ["PWD", "OLDPWD", "SHLVL", "_", "TERM"];

/// The environment `env -0` printed between two `mark`s: name and value, the shell's own left
/// out.
pub fn parse_env(out: &str, mark: &str) -> Vec<(String, String)> {
    let Some(start) = out.find(mark) else {
        return Vec::new();
    };
    let rest = &out[start + mark.len()..];
    let Some(end) = rest.rfind(mark) else {
        return Vec::new();
    };
    rest[..end]
        .split('\0')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(name, _)| !name.is_empty() && !SHELL_OWN.contains(name))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// The environment the user's login shell sets up, as a terminal of theirs has it. A window
/// started from the Finder, the Dock or a desktop's menu gets the system's bare one, where
/// neither `sasacode` nor `git` may be on the PATH. Nothing when the shell does not answer
/// within a few seconds.
#[cfg(unix)]
pub fn login_env() -> Vec<(String, String)> {
    use std::io::Read;
    const MARK: &str = "__SASACODE_GUI_ENV__";
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    let Ok(mut child) = Command::new(&shell)
        .args(["-i", "-l", "-c", &format!("printf {MARK}; env -0; printf {MARK}")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let Some(mut out) = child.stdout.take() else {
        return Vec::new();
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut all = Vec::new();
        let _ = out.read_to_end(&mut all);
        let _ = tx.send(all);
    });
    let all = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap_or_default();
    let _ = child.kill();
    let _ = child.wait();
    parse_env(&String::from_utf8_lossy(&all), MARK)
}

#[cfg(test)]
mod env_tests {
    use super::*;

    #[test]
    fn the_login_shells_environment_is_read_between_the_marks_without_the_shells_own() {
        let out = "Last login: today\nMARKPATH=/opt/homebrew/bin:/usr/bin\0HOME=/Users/me\0SHLVL=2\0EQ=a=b\0MARK";
        let env = parse_env(out, "MARK");
        assert_eq!(
            env,
            [
                ("PATH".to_owned(), "/opt/homebrew/bin:/usr/bin".to_owned()),
                ("HOME".to_owned(), "/Users/me".to_owned()),
                ("EQ".to_owned(), "a=b".to_owned()),
            ]
        );
        assert!(parse_env("no marks here", "MARK").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_login_shell_answers_with_a_path() {
        let env = login_env();
        // A machine whose shell cannot start says nothing; one that can gives a PATH.
        if !env.is_empty() {
            assert!(env.iter().any(|(k, v)| k == "PATH" && !v.is_empty()));
        }
    }
}
