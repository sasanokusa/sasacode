//! The shell the bash tool runs, and stopping a process tree (`packages/tools/src/shell.ts`).
//!
//! The bash tool runs bash everywhere. On Windows that is Git for Windows' bash: WSL's bash.exe
//! (System32, WindowsApps) runs in another filesystem and would not see the Windows paths the
//! model works with, so it is never picked.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Git for Windows' bash.exe candidates, most specific first, from the environment `env` reads.
pub fn git_bash_candidates_with(env: impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = env("SASACODE_GIT_BASH_PATH").filter(|p| !p.is_empty()) {
        out.push(PathBuf::from(p));
    }
    let delimiter = if cfg!(windows) { ';' } else { ':' };
    let path = env("PATH").or_else(|| env("Path")).unwrap_or_default();
    // git.exe sits in <Git>\cmd or <Git>\mingw64\bin; bash.exe in <Git>\bin.
    for dir in path.split(delimiter).filter(|d| !d.is_empty()) {
        let dir = Path::new(dir);
        if !dir.join("git.exe").exists() {
            continue;
        }
        let parent = dir.parent().unwrap_or(dir);
        let grandparent = parent.parent().unwrap_or(parent);
        out.push(parent.join("bin").join("bash.exe"));
        out.push(grandparent.join("bin").join("bash.exe"));
    }
    let roots = [
        env("ProgramFiles"),
        env("ProgramFiles(x86)"),
        env("ProgramW6432"),
        env("LOCALAPPDATA")
            .filter(|l| !l.is_empty())
            .map(|l| Path::new(&l).join("Programs").display().to_string()),
    ];
    for root in roots.into_iter().flatten().filter(|r| !r.is_empty()) {
        out.push(Path::new(&root).join("Git").join("bin").join("bash.exe"));
    }
    out
}

/// `gitBashCandidates()` from this process's environment.
pub fn git_bash_candidates() -> Vec<PathBuf> {
    git_bash_candidates_with(|k| std::env::var(k).ok())
}

/// The bash to run commands with: `bash` on macOS and Linux, Git Bash's full path on Windows.
pub fn bash_path() -> anyhow::Result<String> {
    if !cfg!(windows) {
        return Ok("bash".into());
    }
    static CACHED: OnceLock<String> = OnceLock::new();
    if let Some(found) = CACHED.get() {
        return Ok(found.clone());
    }
    let Some(found) = git_bash_candidates().into_iter().find(|p| p.exists()) else {
        anyhow::bail!(
            "bash was not found: sasacode on Windows needs Git for Windows (https://git-scm.com/download/win). \
             If it is installed somewhere unusual, set SASACODE_GIT_BASH_PATH to its bash.exe."
        );
    };
    Ok(CACHED.get_or_init(|| found.display().to_string()).clone())
}

/// The signals [`kill_tree`] sends. Windows has none: the tree is ended either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSignal {
    Term,
    Kill,
}

/// Windows: a job holding `child` and all it starts. After `taskkill /T /F`, a process the command
/// started was seen still running and holding its output; [`end_job`] ends what is left.
#[cfg(windows)]
pub(crate) fn job_for(child: &tokio::process::Child) -> Option<win32job::Job> {
    let job = win32job::Job::create().ok()?;
    job.assign_process(child.raw_handle()? as isize).ok()?;
    Some(job)
}

/// Ends every process left in `job`: its only handle is closed with kill-on-close set.
#[cfg(windows)]
pub(crate) fn end_job(job: Option<win32job::Job>) {
    if let Some(job) = job {
        let _ = job.set_extended_limit_info(win32job::ExtendedLimitInfo::new().limit_kill_on_job_close());
    }
}

/// Stop a process and everything it started. On macOS and Linux the process must have been spawned
/// in its own process group (`process_group(0)`) and the group gets `signal`; Windows has no
/// signals or process groups, so the whole tree is ended at once with taskkill.
pub fn kill_tree(pid: u32, signal: KillSignal) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = signal;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("taskkill")
            .args(["/pid", &pid.to_string(), "/T", "/F"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;
        let Ok(pgid) = i32::try_from(pid) else { return false };
        // 0 would be our own process group.
        if pgid <= 0 {
            return false;
        }
        let sig = match signal {
            KillSignal::Term => Signal::SIGTERM,
            KillSignal::Kill => Signal::SIGKILL,
        };
        killpg(Pid::from_raw(pgid), sig).is_ok()
    }
}

/// A signal number by its name, as Node reports it (`SIGTERM`).
#[cfg(unix)]
pub(crate) fn signal_name(sig: i32) -> String {
    nix::sys::signal::Signal::try_from(sig)
        .map(|s| s.as_str().to_string())
        .unwrap_or_else(|_| sig.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn git_bash_candidates_follow_git_exe_and_program_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let cmd = tmp.path().join("Git").join("cmd");
        std::fs::create_dir_all(&cmd).unwrap();
        std::fs::write(cmd.join("git.exe"), "").unwrap();
        let delimiter = if cfg!(windows) { ";" } else { ":" };
        let env: HashMap<&str, String> = HashMap::from([
            ("SASACODE_GIT_BASH_PATH", "/custom/bash.exe".to_string()),
            ("PATH", format!("/nowhere{delimiter}{}{delimiter}", cmd.display())),
            ("ProgramFiles", "/pf".to_string()),
            ("LOCALAPPDATA", "/la".to_string()),
        ]);
        let got = git_bash_candidates_with(|k| env.get(k).cloned());
        let git = tmp.path().join("Git");
        assert_eq!(
            got,
            [
                PathBuf::from("/custom/bash.exe"),
                git.join("bin").join("bash.exe"),
                tmp.path().join("bin").join("bash.exe"),
                Path::new("/pf").join("Git").join("bin").join("bash.exe"),
                Path::new("/la")
                    .join("Programs")
                    .join("Git")
                    .join("bin")
                    .join("bash.exe"),
            ]
        );
    }

    #[test]
    fn bash_is_plain_bash_off_windows() {
        if !cfg!(windows) {
            assert_eq!(bash_path().unwrap(), "bash");
        }
    }
}
