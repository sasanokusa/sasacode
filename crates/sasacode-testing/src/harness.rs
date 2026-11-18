//! Runs a sasacode binary as a child process against a fake model server, in a throwaway home and
//! project (`packages/e2e/test/harness.ts`): config loading, the adapters over HTTP, tools on real
//! files, sessions on disk. The binary is the one `SASACODE_E2E_BIN` names.
//!
//! A test harness: a child that cannot start, hangs, or prints a line that is not JSON where JSON
//! is expected fails the test with a panic, as the TS threw.

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The variable naming the binary under test.
pub const E2E_BIN_ENV: &str = "SASACODE_E2E_BIN";

/// How long a run may take before it is killed and the test fails (bun test gave a test 5 s; this is
/// generous so a slow CI machine does not flake, and still ends a hang).
pub const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a [`Live`] waits for its next line.
pub const LINE_TIMEOUT: Duration = Duration::from_secs(60);

/// The binary `SASACODE_E2E_BIN` names, or `None` when it is unset or empty. A value with a path
/// separator is a path (a relative one is taken from the workspace root, since `cargo test` runs
/// each crate's tests in the crate's directory) and must exist; any other value is a command on the
/// PATH.
pub fn e2e_bin() -> Option<PathBuf> {
    let value = std::env::var_os(E2E_BIN_ENV).filter(|v| !v.is_empty())?;
    let path = PathBuf::from(&value);
    let has_separator = value.to_string_lossy().contains(['/', '\\']);
    if !has_separator {
        return Some(path);
    }
    let path = if path.is_absolute() {
        path
    } else {
        workspace_root().join(path)
    };
    assert!(
        path.is_file(),
        "{E2E_BIN_ENV}={} does not exist ({})",
        value.to_string_lossy(),
        path.display()
    );
    Some(path)
}

/// The workspace root (two levels above this crate).
pub fn workspace_root() -> PathBuf {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(crate_dir)
        .to_path_buf()
}

/// Config for one OpenAI-compatible server named "fake" (`fakeConfig`); `extra`'s top-level keys
/// replace these.
pub fn fake_config(url: &str, extra: Value) -> Value {
    let mut config = json!({
        "providers": { "fake": { "api": "openai-chat", "baseUrl": url } },
        "model": "fake/m",
        "maxRetries": 0,
    });
    if let (Value::Object(c), Value::Object(e)) = (&mut config, extra) {
        for (k, v) in e {
            c.insert(k, v);
        }
    }
    config
}

/// A sasacode home: `config.json` (indented by two, as `JSON.stringify(config, null, 2)`) and an empty
/// `.env`, so no keys from the developer's machine are loaded.
pub fn write_home(home: &Path, config: &Value) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    std::fs::write(home.join("config.json"), serde_json::to_string_pretty(config)?)?;
    std::fs::write(home.join(".env"), "")
}

/// The text of the final answer in a jsonl run.
pub fn answer(events: &[Value]) -> String {
    events
        .iter()
        .filter(|e| e["type"] == "message_end" && e["message"]["role"] == "assistant")
        .map(|e| {
            let content = e["message"]["content"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default();
            content
                .iter()
                .filter(|c| c["type"] == "text")
                .filter_map(|c| c["text"].as_str())
                .collect::<String>()
        })
        .rfind(|t| !t.is_empty())
        .unwrap_or_default()
}

/// The roles of the messages the model was sent in one request.
pub fn roles(request: &Value) -> Vec<String> {
    let messages = request["messages"].as_array().map(Vec::as_slice).unwrap_or_default();
    messages
        .iter()
        .map(|m| m["role"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// `JSON.stringify(value)`.
pub fn stringify(value: &Value) -> String {
    value.to_string()
}

/// What a finished run printed.
#[derive(Debug, Clone)]
pub struct RunOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// What a `-p <prompt> --output jsonl` run printed, its events parsed.
#[derive(Debug, Clone)]
pub struct JsonlOutput {
    pub code: i32,
    pub events: Vec<Value>,
    pub stderr: String,
}

/// Options of [`Sandbox::run_with`].
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Written to its stdin, which is then closed. Without it stdin is empty (`/dev/null`).
    pub stdin: Option<String>,
    pub env: Vec<(String, String)>,
}

/// A throwaway root holding `home` (SASACODE_HOME) and `project` (the cwd of every run). Removed
/// when dropped.
pub struct Sandbox {
    root: tempfile::TempDir,
    pub home: PathBuf,
    pub project: PathBuf,
    program: PathBuf,
}

impl Sandbox {
    /// A sandbox running the binary `SASACODE_E2E_BIN` names. Panics when it is unset: check
    /// [`e2e_bin`] first and skip.
    pub fn new(config: &Value) -> Sandbox {
        let Some(program) = e2e_bin() else {
            panic!("{E2E_BIN_ENV} is not set")
        };
        Self::with_program(config, program)
    }

    pub fn with_program(config: &Value, program: impl Into<PathBuf>) -> Sandbox {
        let root = tempfile::Builder::new()
            .prefix("sasacode-e2e-")
            .tempdir()
            .expect("a temporary directory");
        let home = root.path().join("home");
        let project = root.path().join("project");
        write_home(&home, config).expect("the sandbox home");
        std::fs::create_dir_all(&project).expect("the sandbox project");
        Sandbox {
            root,
            home,
            project,
            program: program.into(),
        }
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// The whole environment of a run: nothing else from this process comes through.
    pub fn env(&self) -> Vec<(OsString, OsString)> {
        let root = self.root.path().as_os_str();
        let mut env: Vec<(OsString, OsString)> = vec![
            ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
            ("HOME".into(), root.into()),
            ("USERPROFILE".into(), root.into()),
            ("SASACODE_HOME".into(), self.home.as_os_str().into()),
            ("SASACODE_LANG".into(), "en".into()),
            ("NO_COLOR".into(), "1".into()),
        ];
        // Windows programs need it (sockets, crypto) once the environment is cleared. The TS harness
        // passed only the variables above.
        if cfg!(windows)
            && let Some(system_root) = std::env::var_os("SystemRoot")
        {
            env.push(("SystemRoot".into(), system_root));
        }
        env
    }

    fn command<I, S>(&self, args: I, extra_env: &[(String, String)]) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut cmd = Command::new(&self.program);
        cmd.args(args).current_dir(&self.project).env_clear().envs(self.env());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        cmd
    }

    /// `sasacode <args>`; returns when it exits.
    pub fn run<I, S>(&self, args: I) -> RunOutput
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_with(args, RunOptions::default())
    }

    pub fn run_with<I, S>(&self, args: I, opts: RunOptions) -> RunOutput
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut cmd = self.command(args, &opts.env);
        cmd.stdin(if opts.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let mut child = self.spawn(&mut cmd);
        let writer = opts.stdin.zip(child.stdin.take()).map(|(input, mut stdin)| {
            std::thread::spawn(move || {
                let _ = stdin.write_all(input.as_bytes());
            })
        });
        let stdout = read_all(child.stdout.take());
        let stderr = read_all(child.stderr.take());
        let status = wait(&mut child, RUN_TIMEOUT);
        if let Some(w) = writer {
            let _ = w.join();
        }
        RunOutput {
            code: exit_code(status),
            stdout: joined(stdout),
            stderr: joined(stderr),
        }
    }

    /// `sasacode <args>` with its stdin and stdout left open, for a parent that answers while it runs.
    pub fn start<I, S>(&self, args: I) -> Live
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.start_with(args, &[])
    }

    pub fn start_with<I, S>(&self, args: I, env: &[(String, String)]) -> Live
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut cmd = self.command(args, env);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = self.spawn(&mut cmd);
        let stdin = child.stdin.take();
        let (tx, lines) = mpsc::channel();
        let stdout = child.stdout.take();
        std::thread::spawn(move || {
            let Some(stdout) = stdout else { return };
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    // A last line without its newline is never complete: like the TS, it is dropped.
                    Ok(0) | Err(_) => return,
                    Ok(_) if buf.last() != Some(&b'\n') => return,
                    Ok(_) => {
                        buf.pop();
                        if tx.send(String::from_utf8_lossy(&buf).into_owned()).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        let stderr = read_all(child.stderr.take());
        Live {
            child,
            stdin,
            lines,
            stderr: Some(stderr),
            stderr_text: None,
            status: None,
        }
    }

    /// `sasacode -p <prompt> --output jsonl <extra>`: the events, parsed.
    pub fn jsonl(&self, prompt: &str, extra: &[&str]) -> JsonlOutput {
        let mut args = vec!["-p", prompt, "--output", "jsonl"];
        args.extend_from_slice(extra);
        let r = self.run(args);
        JsonlOutput {
            code: r.code,
            events: parse_lines(&r.stdout),
            stderr: r.stderr,
        }
    }

    fn spawn(&self, cmd: &mut Command) -> Child {
        cmd.spawn()
            .unwrap_or_else(|e| panic!("could not start {}: {e}", self.program.display()))
    }
}

/// The non-blank lines of `text`, each parsed as JSON.
pub fn parse_lines(text: &str) -> Vec<Value> {
    text.split('\n')
        .filter(|l| !l.trim().is_empty())
        .map(parse_line)
        .collect()
}

fn parse_line(line: &str) -> Value {
    serde_json::from_str(line).unwrap_or_else(|e| panic!("not a JSON line ({e}): {line}"))
}

/// A sasacode child process that keeps running: read its stdout line by line, write to its stdin.
/// Dropping it kills the process.
pub struct Live {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    stderr_text: Option<String>,
    status: Option<ExitStatus>,
}

/// Its stdout lines, parsed: [`Iterator::next`] is the next one, `None` once stdout has closed.
impl Iterator for Live {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        self.next_within(LINE_TIMEOUT)
            .unwrap_or_else(|TimedOut| panic!("no line from sasacode in {} s", LINE_TIMEOUT.as_secs()))
    }
}

/// No line came within the time [`Live::next_within`] was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedOut;

impl Live {
    /// The next stdout line, parsed, if one comes within `timeout`; `Ok(None)` once stdout has closed.
    /// A line that comes later is not lost: the next call returns it.
    pub fn next_within(&mut self, timeout: Duration) -> Result<Option<Value>, TimedOut> {
        let deadline = Instant::now() + timeout;
        loop {
            match self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => return Ok(Some(parse_line(&line))),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
                Err(mpsc::RecvTimeoutError::Timeout) => return Err(TimedOut),
            }
        }
    }

    /// Lines up to and including the first of this type.
    pub fn until(&mut self, ty: &str) -> Vec<Value> {
        let mut seen = Vec::new();
        for e in self.by_ref() {
            let last = e["type"] == ty;
            seen.push(e);
            if last {
                break;
            }
        }
        seen
    }

    /// Writes `JSON.stringify(message)` and a newline. A process that has stopped reading is not an
    /// error here; what it did shows in its output and exit code.
    pub fn send(&mut self, message: &Value) {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = stdin
                .write_all(format!("{message}\n").as_bytes())
                .and_then(|_| stdin.flush());
        }
    }

    /// Close its stdin.
    pub fn end(&mut self) {
        self.stdin = None;
    }

    /// Waits for it to exit; its exit code.
    pub fn exited(&mut self) -> i32 {
        if self.status.is_none() {
            self.status = Some(wait(&mut self.child, RUN_TIMEOUT));
        }
        self.status.map(exit_code).unwrap_or(-1)
    }

    /// All it wrote to stderr (waits for stderr to close).
    pub fn stderr(&mut self) -> String {
        if let Some(handle) = self.stderr.take() {
            self.stderr_text = Some(joined(handle));
        }
        self.stderr_text.clone().unwrap_or_default()
    }

    pub fn kill(&mut self) {
        if self.status.is_none() {
            let _ = self.child.kill();
            self.status = self.child.wait().ok();
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.kill();
    }
}

fn read_all<R: Read + Send + 'static>(pipe: Option<R>) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut out);
        }
        out
    })
}

fn joined(handle: JoinHandle<Vec<u8>>) -> String {
    String::from_utf8_lossy(&handle.join().unwrap_or_default()).into_owned()
}

/// Waits for the child, killing it and failing the test after `timeout`.
fn wait(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("sasacode did not exit within {} s", timeout.as_secs());
            }
            Err(e) => panic!("could not wait for sasacode: {e}"),
        }
    }
}

/// The exit code; a process ended by a signal counts as 128 + the signal, as a shell reports it.
pub fn exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    -1
}
