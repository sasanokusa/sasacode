//! `/gui` (`packages/bundled/src/gui.ts`): the desktop window (sasacode-gui, docs/gui.md) on this
//! folder, opened on this conversation. The window runs sasacode itself for each message, so from
//! then on the conversation goes on in one of the two: neither reads what the other adds.
//!
//! settings: { command?: string }

use std::time::Duration;

use futures::future::BoxFuture;

use sasacode_host::t;
use sasacode_plugin_api::{Command, NotifyLevel, PluginApi, apply_child_env, detach_session};

use crate::net_util::setting_str;

/// How to get the window's program while it is not released with sasacode (docs/gui.md). `--locked`: the versions CI builds with.
const INSTALL: &str = "cargo install --locked --path crates/sasacode-gui";
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
/// (default: sasacode-gui on PATH, looked up now, so one installed a moment ago is found). Returns
/// the program once it has stayed up for `grace` (600 ms). One that leaves at once with an error
/// (an older sasacode-gui that does not know an option) is an error: nothing the window says is
/// seen from here.
pub async fn open_gui(target: GuiTarget, command: Option<String>, grace: Option<Duration>) -> anyhow::Result<String> {
    // PATH as it is now: `which` reads it on every call.
    let command = command
        .filter(|c| !c.is_empty())
        .or_else(|| which::which("sasacode-gui").ok().map(|p| p.display().to_string()));
    let Some(command) = command else {
        anyhow::bail!(t!(
            "sasacode-guiが見つかりません。sasacodeのリポジトリで次を実行すると入ります。\n  {install}\n別の場所にある場合はplugins.settings.gui.commandを指定してください。",
            install = INSTALL
        ));
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
    Ok(command)
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
                if let Err(e) = open_gui(target, setting_str(&api.settings(), "command"), None).await {
                    api.ui().notify(&e.to_string(), NotifyLevel::Error);
                    return Ok(());
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
