//! The stdio transport (`StdioClientTransport` of the TS SDK): the server is a child process
//! speaking newline-delimited JSON on stdin/stdout; its stderr is piped to a callback.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{mpsc, watch};

use crate::protocol::{Incoming, Transport};

/// A line longer than this without a newline closes the transport (`STDIO_DEFAULT_MAX_BUFFER_SIZE`).
const MAX_BUFFER: usize = 10 * 1024 * 1024;
/// How long close() gives the process after closing stdin, and again after SIGTERM.
const CLOSE_GRACE: Duration = Duration::from_millis(2_000);

/// Environment variables a server inherits (`DEFAULT_INHERITED_ENV_VARS`, a list inspired by the
/// default env inheritance of sudo). Everything else must be given in the server's `env`.
#[cfg(windows)]
pub const DEFAULT_INHERITED_ENV_VARS: &[&str] = &[
    "APPDATA",
    "HOMEDRIVE",
    "HOMEPATH",
    "LOCALAPPDATA",
    "PATH",
    "PROCESSOR_ARCHITECTURE",
    "SYSTEMDRIVE",
    "SYSTEMROOT",
    "TEMP",
    "USERNAME",
    "USERPROFILE",
    "PROGRAMFILES",
];
#[cfg(not(windows))]
pub const DEFAULT_INHERITED_ENV_VARS: &[&str] = &["HOME", "LOGNAME", "PATH", "SHELL", "TERM", "USER"];

/// `getDefaultEnvironment()`: the inherited variables that are set (and not hidden from children
/// by sasacode), skipping values that start with "()" (shell functions, a security risk).
pub fn default_environment() -> BTreeMap<String, String> {
    let all = sasacode_plugin_api::child_env(&[]);
    #[cfg(windows)]
    let get = |k: &str| {
        all.iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.clone())
    };
    #[cfg(not(windows))]
    let get = |k: &str| all.get(k).cloned();
    DEFAULT_INHERITED_ENV_VARS
        .iter()
        .filter_map(|k| get(k).filter(|v| !v.starts_with("()")).map(|v| (k.to_string(), v)))
        .collect()
}

pub(crate) struct StdioParams {
    pub command: String,
    pub args: Vec<String>,
    /// The whole environment of the child.
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
}

pub(crate) type OnStderr = Arc<dyn Fn(&str) + Send + Sync>;

struct Process {
    pid: Option<u32>,
    exited: watch::Receiver<bool>,
    kill: mpsc::UnboundedSender<()>,
}

pub(crate) struct StdioTransport {
    params: StdioParams,
    on_stderr: OnStderr,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    process: Mutex<Option<Process>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Node's text for a failed spawn: `spawn <command> ENOENT`.
fn spawn_error(command: &str, e: &std::io::Error) -> anyhow::Error {
    let code = match e.kind() {
        std::io::ErrorKind::NotFound => "ENOENT".to_string(),
        std::io::ErrorKind::PermissionDenied => "EACCES".to_string(),
        _ => e.to_string(),
    };
    anyhow::anyhow!("spawn {command} {code}")
}

impl StdioTransport {
    pub fn new(params: StdioParams, on_stderr: OnStderr) -> Self {
        StdioTransport {
            params,
            on_stderr,
            stdin: tokio::sync::Mutex::new(None),
            process: Mutex::new(None),
        }
    }

    fn command(&self) -> Command {
        let p = &self.params;
        #[cfg(windows)]
        let program = {
            // No shell: find `npx` as `npx.cmd` on the child's PATH, as cross-spawn did.
            let path = p
                .env
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("PATH"))
                .map(|(_, v)| v.clone());
            which::which_in(&p.command, path, &p.cwd)
                .map(|f| f.into_os_string())
                .unwrap_or_else(|_| p.command.clone().into())
        };
        #[cfg(not(windows))]
        let program = &p.command;
        let mut cmd = Command::new(program);
        cmd.args(&p.args)
            .env_clear()
            .envs(&p.env)
            .current_dir(&p.cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(false);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn start(&self, sink: mpsc::UnboundedSender<Incoming>) -> anyhow::Result<()> {
        if lock(&self.process).is_some() {
            anyhow::bail!("StdioClientTransport already started!");
        }
        let mut child = self
            .command()
            .spawn()
            .map_err(|e| spawn_error(&self.params.command, &e))?;
        let (exited_tx, exited) = watch::channel(false);
        let (kill, mut kill_rx) = mpsc::unbounded_channel::<()>();
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        *self.stdin.lock().await = child.stdin.take();
        *lock(&self.process) = Some(Process {
            pid,
            exited: exited.clone(),
            kill,
        });

        // The process: wait for it, kill it when asked.
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = child.wait() => break,
                    Some(()) = kill_rx.recv() => { let _ = child.start_kill(); }
                }
            }
            let _ = exited_tx.send(true);
        });

        if let Some(mut stderr) = stderr {
            let on_stderr = self.on_stderr.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                while let Ok(n) = stderr.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    on_stderr(&String::from_utf8_lossy(&buf[..n]));
                }
            });
        }

        // stdout: one JSON-RPC message per line. The transport closes once stdout ends and the
        // process has exited (Node's 'close' event).
        let mut exited_wait = exited;
        let kill_on_overflow = lock(&self.process).as_ref().map(|p| p.kill.clone());
        tokio::spawn(async move {
            if let Some(mut out) = stdout {
                let mut pending: Vec<u8> = Vec::new();
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    let n = match out.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    pending.extend_from_slice(&buf[..n]);
                    while let Some(at) = pending.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = pending.drain(..=at).collect();
                        let line = String::from_utf8_lossy(&line[..line.len() - 1]);
                        let line = line.strip_suffix('\r').unwrap_or(&line);
                        // A line that is not JSON is reported and skipped (here: skipped).
                        if let Ok(v) = serde_json::from_str::<Value>(line) {
                            let _ = sink.send(Incoming::Message(v));
                        }
                    }
                    if pending.len() > MAX_BUFFER {
                        // ReadBuffer exceeded its maximum size: the SDK closes the transport.
                        if let Some(k) = &kill_on_overflow {
                            let _ = k.send(());
                        }
                        break;
                    }
                }
            }
            let _ = exited_wait.wait_for(|e| *e).await;
            let _ = sink.send(Incoming::Closed);
        });
        Ok(())
    }

    async fn send(&self, message: Value) -> anyhow::Result<()> {
        let mut stdin = self.stdin.lock().await;
        let Some(w) = stdin.as_mut() else {
            anyhow::bail!("Not connected");
        };
        let mut line = serde_json::to_string(&message)?;
        line.push('\n');
        // A write to a process that went away fails here, but the SDK only reported it: the
        // request then ends with the close ("Connection closed").
        let _ = w.write_all(line.as_bytes()).await;
        let _ = w.flush().await;
        Ok(())
    }

    async fn close(&self) {
        let Some(process) = lock(&self.process).take() else {
            return;
        };
        // Closing stdin asks the server to exit.
        drop(self.stdin.lock().await.take());
        let gone = |mut e: watch::Receiver<bool>| async move { e.wait_for(|x| *x).await.is_ok() };
        if tokio::time::timeout(CLOSE_GRACE, gone(process.exited.clone()))
            .await
            .is_ok()
        {
            return;
        }
        terminate(process.pid, &process.kill);
        if tokio::time::timeout(CLOSE_GRACE, gone(process.exited.clone()))
            .await
            .is_ok()
        {
            return;
        }
        let _ = process.kill.send(());
    }
}

/// SIGTERM on Unix; Windows has no such signal, so the process is killed (as Node's kill does there).
fn terminate(pid: Option<u32>, kill: &mpsc::UnboundedSender<()>) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill as send_signal};
        use nix::unistd::Pid;
        if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
            let _ = send_signal(Pid::from_raw(pid), Signal::SIGTERM);
            return;
        }
    }
    let _ = pid;
    let _ = kill.send(());
}
