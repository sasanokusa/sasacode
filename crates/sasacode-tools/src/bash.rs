//! The bash tool (`packages/tools/src/bash.ts`).
//!
//! Output limits count JS string length (UTF-16 units), as the TS did: the 30,000 of the tail and
//! the total in "[Output truncated: ... of N characters ...]".

use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Sleep;

use sasacode_plugin_api::{JsonObject, Tool, ToolContext, ToolKind, ToolResult, apply_child_env, save_output};

use crate::shell::{KillSignal, bash_path, kill_tree};
use crate::util::{fs_error, js_number_text, js_string_arg, js_trim_end, num_arg, str_arg, utf16_len};

const DEFAULT_TIMEOUT_S: f64 = 120.0;
/// 24 hours. Past 2^31-1 ms (24.8 days) a JS setTimeout fired at once: the command was killed as it
/// started (#90). Kept as the documented maximum.
const MAX_TIMEOUT_S: f64 = 86_400.0;
const MAX_OUTPUT: usize = 30_000;
/// After the shell exits, the call ends once both pipes close, or after this long if something it
/// started keeps them open (at 50 ms, a quick command's output once came back empty on Windows CI).
const EXIT_GRACE: Duration = Duration::from_millis(500);
/// SIGTERM first; SIGKILL this long after if the group is still there.
const ESCALATION: Duration = Duration::from_secs(2);

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> String {
        let mut d = format!(
            "Run a shell command with bash in the working directory. Returns combined stdout/stderr and the exit code. Output over {MAX_OUTPUT} characters is cut to the tail and saved to a file whose path is returned. Use ripgrep (rg) for searching."
        );
        if cfg!(windows) {
            d.push_str(" On Windows this is Git Bash: write paths as C:/x or /c/x and quote Windows paths with \\ in them; Windows programs (cmd /c, powershell -Command, *.exe) run from it too.");
        }
        d
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Command to run" },
                "timeout": {
                    "type": "number",
                    "maximum": MAX_TIMEOUT_S as i64,
                    "description": format!(
                        "Timeout in seconds (default {}, at most {})",
                        DEFAULT_TIMEOUT_S as i64,
                        MAX_TIMEOUT_S as i64
                    )
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Exec
    }
    fn always_load(&self) -> bool {
        true
    }
    fn match_target(&self, args: &JsonObject) -> Option<String> {
        Some(js_string_arg(args, "command"))
    }
    fn summary(&self, args: &JsonObject) -> Option<String> {
        Some(js_string_arg(args, "command"))
    }
    async fn execute(&self, args: JsonObject, ctx: ToolContext) -> anyhow::Result<ToolResult> {
        Ok(run(&args, &ctx).await)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KilledBy {
    Timeout,
    Abort,
}

impl KilledBy {
    fn as_str(self) -> &'static str {
        match self {
            KilledBy::Timeout => "timeout",
            KilledBy::Abort => "abort",
        }
    }
}

/// Memory holds the tail only; past MAX_OUTPUT everything goes to a file as it comes.
#[derive(Default)]
struct Output {
    tail: String,
    tail_units: usize,
    size: usize,
    file: Option<PathBuf>,
    /// Saving failed once: not tried again (the TS threw out of its data handler there).
    unsaved: bool,
}

impl Output {
    fn push(&mut self, s: &str) {
        let units = utf16_len(s);
        self.size += units;
        if let Some(file) = &self.file {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(file) {
                let _ = f.write_all(s.as_bytes());
            }
        } else if self.size > MAX_OUTPUT && !self.unsaved {
            match save_output("bash", &format!("{}{s}", self.tail)) {
                Ok(file) => self.file = Some(file),
                Err(_) => self.unsaved = true,
            }
        }
        self.tail.push_str(s);
        self.tail_units += units;
        // The last MAX_OUTPUT units, not half a surrogate pair (#88): whole characters only.
        if self.tail_units > MAX_OUTPUT {
            let mut cut = 0;
            for c in self.tail.chars() {
                if self.tail_units <= MAX_OUTPUT {
                    break;
                }
                self.tail_units -= c.len_utf16();
                cut += c.len_utf8();
            }
            self.tail.drain(..cut);
        }
    }
}

/// Read a pipe as UTF-8, decoded per stream so a character split across two reads stays whole.
/// The channel is small and `send` waits: a flood is throttled at the pipe, as the TS's synchronous
/// data handler throttled it, instead of queueing in memory while the consumer lags.
fn pump<R: AsyncRead + Unpin + Send + 'static>(mut r: R, tx: mpsc::Sender<String>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut decoder = encoding_rs::UTF_8.new_decoder_without_bom_handling();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = r.read(&mut buf).await.unwrap_or(0);
            let last = n == 0;
            let mut s = String::with_capacity(decoder.max_utf8_buffer_length(n).unwrap_or(n * 3 + 16));
            let _ = decoder.decode_to_string(&buf[..n], &mut s, last);
            if !s.is_empty() && tx.send(s).await.is_err() {
                break;
            }
            if last {
                break;
            }
        }
    })
}

async fn run(args: &JsonObject, ctx: &ToolContext) -> ToolResult {
    if ctx.signal.is_cancelled() {
        return ToolResult::error("[Command was interrupted before execution]");
    }
    let timeout_s = num_arg(args, "timeout")
        .filter(|t| *t > 0.0)
        .unwrap_or(DEFAULT_TIMEOUT_S)
        .min(MAX_TIMEOUT_S);
    let bash = match bash_path() {
        Ok(b) => b,
        Err(e) => return ToolResult::error(e.to_string()),
    };
    let mut cmd = tokio::process::Command::new(&bash);
    cmd.arg("-c").arg(str_arg(args, "command")).current_dir(&ctx.cwd);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // Without the keys sasacode read from ~/.sasacode/.env for itself.
    let extra =
        [("PAGER", "cat"), ("GIT_PAGER", "cat"), ("SASACODE", "1")].map(|(k, v)| (k.to_string(), v.to_string()));
    apply_child_env(&mut cmd, &extra);
    // Its own session and process group (Node's `detached`), so everything the command starts can
    // be stopped together and nothing waits on the terminal (Windows has no process groups:
    // kill_tree ends the process tree instead).
    sasacode_plugin_api::detach_session(&mut cmd);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            // Worded as Bun reported it: "Error: ENOENT: no such file or directory, posix_spawn 'bash'".
            let tail = format!("Error: {}", fs_error(&e, "posix_spawn", &bash));
            return finish(
                Output {
                    tail,
                    ..Default::default()
                },
                None,
                None,
                None,
                timeout_s,
            );
        }
    };
    // Stopped with the rest when the command is killed; left alone when it ends by itself.
    #[cfg(windows)]
    let job = crate::shell::job_for(&child);
    let pid = child.id();
    let signal_group = |s: KillSignal| {
        if let Some(pid) = pid {
            kill_tree(pid, s);
        }
    };
    let (tx, mut rx) = mpsc::channel::<String>(16);
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        readers.push(pump(out, tx.clone()));
    }
    if let Some(err) = child.stderr.take() {
        readers.push(pump(err, tx.clone()));
    }
    drop(tx);

    let mut output = Output::default();
    let timer = tokio::time::sleep(Duration::from_secs_f64(timeout_s));
    tokio::pin!(timer);
    let mut killed_by: Option<KilledBy> = None;
    let mut escalation: Option<Pin<Box<Sleep>>> = None;
    let mut grace: Option<Pin<Box<Sleep>>> = None;
    let mut status: Option<std::io::Result<std::process::ExitStatus>> = None;
    let mut stream_open = true;
    while status.is_none() || stream_open {
        tokio::select! {
            chunk = rx.recv(), if stream_open => match chunk {
                Some(s) => {
                    output.push(&s);
                    ctx.update(s);
                }
                None => stream_open = false,
            },
            st = child.wait(), if status.is_none() => {
                status = Some(st);
                grace = Some(Box::pin(tokio::time::sleep(EXIT_GRACE)));
            }
            _ = &mut timer, if status.is_none() && killed_by.is_none() => {
                killed_by = Some(KilledBy::Timeout);
                signal_group(KillSignal::Term);
                escalation = Some(Box::pin(tokio::time::sleep(ESCALATION)));
            }
            _ = ctx.signal.cancelled(), if status.is_none() && killed_by.is_none() => {
                killed_by = Some(KilledBy::Abort);
                signal_group(KillSignal::Term);
                escalation = Some(Box::pin(tokio::time::sleep(ESCALATION)));
            }
            _ = async { if let Some(e) = escalation.as_mut() { e.await } }, if escalation.is_some() => {
                escalation = None;
                signal_group(KillSignal::Kill);
            }
            _ = async { if let Some(g) = grace.as_mut() { g.await } }, if grace.is_some() => break,
        }
    }
    if killed_by.is_some() {
        signal_group(KillSignal::Kill);
        #[cfg(windows)]
        crate::shell::end_job(job);
    }
    // Background jobs may keep the pipes open; stop listening once the shell itself is gone. What
    // was read already still counts.
    for r in &readers {
        r.abort();
    }
    while let Ok(s) = rx.try_recv() {
        output.push(&s);
        ctx.update(s);
    }
    let (code, sig) = match status {
        Some(Ok(st)) => exit_parts(&st),
        Some(Err(e)) => {
            output.tail.push_str(&format!("Error: {e}"));
            (None, None)
        }
        None => (None, None),
    };
    finish(output, code, sig, killed_by, timeout_s)
}

/// The exit code, or the name of the signal that ended the shell.
fn exit_parts(st: &std::process::ExitStatus) -> (Option<i32>, Option<String>) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        (st.code(), st.signal().map(crate::shell::signal_name))
    }
    #[cfg(not(unix))]
    {
        (st.code(), None)
    }
}

fn finish(
    output: Output,
    code: Option<i32>,
    sig: Option<String>,
    killed_by: Option<KilledBy>,
    timeout_s: f64,
) -> ToolResult {
    let mut notes = Vec::new();
    if let Some(file) = &output.file {
        notes.push(format!(
            "[Output truncated: showing the last {MAX_OUTPUT} of {} characters. Full output saved to {}]",
            output.size,
            file.display()
        ));
    }
    match killed_by {
        Some(KilledBy::Timeout) => notes.push(format!(
            "[Command timed out after {}s and was killed]",
            js_number_text(timeout_s)
        )),
        Some(KilledBy::Abort) => notes.push("[Command was interrupted by the user]".into()),
        None => {}
    }
    notes.push(match code {
        Some(c) => format!("[exit code {c}]"),
        None => format!("[terminated by {}]", sig.as_deref().unwrap_or("null")),
    });
    let mut parts = vec![js_trim_end(&output.tail).to_string()];
    parts.extend(notes);
    parts.retain(|p| !p.is_empty());
    let mut details = JsonObject::new();
    details.insert("exitCode".into(), code.map_or(Value::Null, Value::from));
    if let Some(k) = killed_by {
        details.insert("killedBy".into(), json!(k.as_str()));
    }
    ToolResult {
        content: vec![sasacode_plugin_api::text(parts.join("\n"))],
        is_error: Some(code != Some(0) || killed_by.is_some()),
        details: Some(details),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_keeps_whole_characters_in_the_tail() {
        let mut o = Output::default();
        o.push(&"x".repeat(MAX_OUTPUT - 1));
        o.unsaved = true; // no file in this unit test
        o.push("😀");
        // 30,001 units: the x at the front goes, the emoji stays whole.
        assert_eq!(o.tail_units, MAX_OUTPUT);
        assert!(o.tail.ends_with('😀'));
        o.push("😀");
        assert_eq!(o.size, MAX_OUTPUT + 3);
        assert_eq!(utf16_len(&o.tail), o.tail_units);
        assert!(o.tail_units <= MAX_OUTPUT);
    }
}
