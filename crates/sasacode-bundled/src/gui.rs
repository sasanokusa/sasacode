//! `/gui` (`packages/bundled/src/gui.ts`): the desktop window (sasacode-gui, docs/gui.md) on this
//! folder, opened on this conversation. The window runs sasacode itself for each message, so from
//! then on the conversation goes on in one of the two: neither reads what the other adds.
//!
//! settings: { command?: string }

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::future::BoxFuture;

use sasacode_host::t;
use sasacode_plugin_api::{Command, NotifyLevel, PluginApi, apply_child_env, detach_session, sasacode_home};

use crate::net_util::setting_str;

/// How to get the window's program: it is built on this machine, not released with sasacode (docs/gui.md).
const INSTALL: &str = "sasacode gui install";
/// The version the window should be: sasacode's own (one workspace version for every crate).
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where `sasacode gui install` builds into (cargo's `--root`): under sasacode's home, off PATH.
pub fn gui_root_in(home: &Path) -> PathBuf {
    home.join("gui")
}

/// The window's program that `sasacode gui install` built, under a sasacode home.
pub fn managed_gui_in(home: &Path) -> PathBuf {
    gui_root_in(home)
        .join("bin")
        .join(format!("sasacode-gui{}", std::env::consts::EXE_SUFFIX))
}

/// [`managed_gui_in`] for this sasacode home.
pub fn managed_gui() -> PathBuf {
    managed_gui_in(&sasacode_home())
}

/// Which window to start: the one plugins.settings.gui.command names, else the one `sasacode gui
/// install` built (an older one installed by hand on PATH would hide every rebuild), else
/// sasacode-gui on PATH. The bool: found here, not named by the user (only those are asked their
/// version: a command the user names may be a launcher that opens a window on any argument).
pub fn pick_gui(
    configured: Option<String>,
    managed: &Path,
    on_path: impl FnOnce() -> Option<PathBuf>,
) -> Option<(String, bool)> {
    if let Some(c) = configured.filter(|c| !c.is_empty()) {
        return Some((c, false));
    }
    if managed.is_file() {
        return Some((managed.display().to_string(), true));
    }
    on_path().map(|p| (p.display().to_string(), true))
}

/// The version in what `sasacode-gui --version` prints ("sasacode-gui 0.20.0").
pub fn version_of(printed: &str) -> Option<String> {
    let v = printed.trim();
    let v = v.strip_prefix("sasacode-gui").map_or(v, str::trim_start);
    (!v.is_empty() && !v.contains(char::is_whitespace)).then(|| v.to_string())
}

/// What the window at `program` says its version is, within 5 s; None when it says nothing usable.
pub async fn gui_version(program: &str) -> Option<String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    apply_child_env(&mut cmd, &[]);
    let out = tokio::time::timeout(Duration::from_secs(5), cmd.output())
        .await
        .ok()?
        .ok()?;
    out.status
        .success()
        .then(|| version_of(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

/// A window that is not this sasacode's version: what to tell the user (the window opens all the same).
pub fn stale_note(gui: &str) -> String {
    t!(
        "sasacode-guiは{gui}、sasacodeは{cli}です。`{install}`で、この版のGUIを作り直せます。",
        gui = gui,
        cli = VERSION,
        install = INSTALL
    )
}

/// The window that was started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub program: String,
    /// Its version, when it is not sasacode's (only asked of a window found here, not one the user named).
    pub stale: Option<String>,
}
/// How long a window has to stay up to count as opened: time for one to turn its arguments down. A
/// guess: a program started for the first time can take longer, and is then taken for opened.
const GRACE: Duration = Duration::from_millis(600);

/// The sasacode a window opened from this one runs: this program, unless SASACODE_BIN says which
/// one to use (then the window finds it as it does by itself).
pub fn own_program() -> Option<String> {
    let exe = std::env::current_exe().ok()?.display().to_string();
    own_program_for(&exe, std::env::var("SASACODE_BIN").ok().as_deref())
}

/// [`own_program`] for a given program path and SASACODE_BIN.
pub fn own_program_for(exe: &str, sasacode_bin: Option<&str>) -> Option<String> {
    if sasacode_bin.is_some_and(|b| !b.is_empty()) {
        return None;
    }
    Some(exe.to_string())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuiTarget {
    /// The folder to work in.
    pub cwd: String,
    /// The conversation to open on (`sasacode-gui --session`).
    pub session: Option<String>,
    /// The sasacode the window runs (`sasacode-gui --sasacode`).
    pub program: Option<String>,
}

/// The window's arguments (`sasacode-gui --help`).
pub fn gui_args(o: &GuiTarget) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(program) = o.program.as_ref().filter(|p| !p.is_empty()) {
        args.extend(["--sasacode".to_string(), program.clone()]);
    }
    if let Some(session) = o.session.as_ref().filter(|s| !s.is_empty()) {
        args.extend(["--session".to_string(), session.clone()]);
    }
    args.push(o.cwd.clone());
    args
}

/// A signal's name as Node reports it (`SIGTERM`).
#[cfg(unix)]
fn signal_name(n: i32) -> String {
    let name = match n {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        10 if cfg!(target_os = "linux") => "SIGUSR1",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => return n.to_string(),
    };
    name.to_string()
}

fn exit_status(status: std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return code.to_string();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return signal_name(signal);
        }
    }
    "?".to_string()
}

/// Starts the window on its own: it stays when this sasacode ends. `command` is the program to start
/// (default: [`pick_gui`]'s, looked up now, so one installed a moment ago is found). Returns the
/// program once it has stayed up for `grace` (600 ms). One that leaves at once with an error (an
/// older sasacode-gui that does not know an option) is an error: nothing the window says is seen
/// from here.
pub async fn open_gui(target: GuiTarget, command: Option<String>, grace: Option<Duration>) -> anyhow::Result<Opened> {
    // PATH as it is now: `which` reads it on every call.
    let picked = pick_gui(command, &managed_gui(), || which::which("sasacode-gui").ok());
    let Some((command, found_here)) = picked else {
        anyhow::bail!(t!(
            "sasacode-guiが見つかりません。ターミナルで次を実行すると、このマシンでビルドして入ります（Rustが要ります）。\n  {install}\n別の場所にある場合はplugins.settings.gui.commandを指定してください。",
            install = INSTALL
        ));
    };
    let stale = if found_here {
        gui_version(&command).await.filter(|v| v != VERSION)
    } else {
        None
    };
    let failed = |reason: String| {
        anyhow::anyhow!(t!(
            "sasacode-guiを起動できませんでした（{command}）: {reason}",
            command = command,
            reason = reason
        ))
    };
    // Detached, as a background job is: a child that is not is ended with sasacode on Windows too.
    // Nothing is read from it: a pipe nobody reads would stop it, or end it with us.
    let mut cmd = tokio::process::Command::new(&command);
    cmd.args(gui_args(&target))
        .current_dir(&target.cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    apply_child_env(&mut cmd, &[]);
    detach_session(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| failed(e.to_string()))?;
    let left = match tokio::time::timeout(grace.unwrap_or(GRACE), child.wait()).await {
        Err(_) => None,
        Ok(Err(e)) => return Err(failed(e.to_string())),
        Ok(Ok(status)) => Some(status),
    };
    // A launcher that starts the window and leaves with 0 has done its work.
    if let Some(status) = left
        && !status.success()
    {
        anyhow::bail!(t!(
            "sasacode-guiがすぐに終了しました（{command}、終了コード{status}）。このsasacodeより古い版かもしれません。入れ直すには次を実行してください。\n  {install}",
            command = command,
            status = exit_status(status),
            install = INSTALL
        ));
    }
    Ok(Opened {
        program: command,
        stale,
    })
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let run_api = api.clone();
        api.register_command(Command::new("gui", &t!("デスクトップGUIでこの会話を開く"), move |_ctx| {
            let api = run_api.clone();
            async move {
                // A conversation nobody has said anything in has an id and no log yet: nothing to open on.
                let session = if api.session().messages().is_empty() { None } else { api.session().id() };
                let target = GuiTarget {
                    cwd: api.cwd().display().to_string(),
                    session: session.clone(),
                    program: own_program(),
                };
                let opened = match open_gui(target, setting_str(&api.settings(), "command"), None).await {
                    Ok(opened) => opened,
                    Err(e) => {
                        api.ui().notify(&e.to_string(), NotifyLevel::Error);
                        return Ok(());
                    }
                };
                if let Some(gui) = &opened.stale {
                    api.ui().notify(&stale_note(gui), NotifyLevel::Warning);
                }
                let message = if session.is_some() {
                    t!("GUIでこの会話を開きました。この画面とGUIは互いの発言を読み込みません。続きはどちらか一方で進めてください。")
                } else {
                    t!("GUIを開きました。")
                };
                api.ui().notify(&message, NotifyLevel::Info);
                Ok(())
            }
        }));
        Ok(())
    })
}
