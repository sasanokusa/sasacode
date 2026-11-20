//! `sasacode -p`: run one prompt to completion (`packages/cli/src/headless.ts`). Text mode streams
//! the answer to stdout; jsonl emits every event. With control (`--control stdio`, jsonl only), the
//! process that reads this output answers approvals, sends messages and can abort.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::AsyncRead;

use sasacode_agent::events::AgentEvent;
use sasacode_ai::{Message, StopReason, StreamEvent, fmt_tokens};
use sasacode_host::t;
use sasacode_plugin_api::{StopCause, UserContent};

use crate::catalog::untold;
use crate::control::start_control;
use crate::setup::Harness;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Jsonl,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HeadlessOptions {
    /// --slash: a prompt that starts with /name runs that slash command instead of going to the model.
    pub slash: bool,
}

enum Out {
    Stdout(String),
    Stderr(String),
    Flush(tokio::sync::oneshot::Sender<()>),
}

/// Writes to stdout and stderr in the order asked, on a thread of its own: the run never waits
/// on a slow reader, and [`Output::flushed`] waits until everything has gone out (#102). A reader
/// that went away (EPIPE) is ignored.
#[derive(Clone)]
pub struct Output {
    tx: std::sync::mpsc::Sender<Out>,
}

impl Default for Output {
    fn default() -> Self {
        Self::new()
    }
}

impl Output {
    pub fn new() -> Output {
        let (tx, rx) = std::sync::mpsc::channel::<Out>();
        std::thread::spawn(move || {
            let (mut stdout, mut stderr) = (std::io::stdout(), std::io::stderr());
            for msg in rx {
                match msg {
                    Out::Stdout(s) => {
                        let _ = stdout.write_all(s.as_bytes()).and_then(|_| stdout.flush());
                    }
                    Out::Stderr(s) => {
                        let _ = stderr.write_all(s.as_bytes()).and_then(|_| stderr.flush());
                    }
                    Out::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        Output { tx }
    }
    /// What goes out hides the values sasacode keeps out of its records (keys, tokens, headers).
    pub fn stdout(&self, s: impl Into<String>) {
        let _ = self
            .tx
            .send(Out::Stdout(sasacode_plugin_api::redact_secrets(&s.into()).into_owned()));
    }
    pub fn stderr(&self, s: impl Into<String>) {
        let _ = self
            .tx
            .send(Out::Stderr(sasacode_plugin_api::redact_secrets(&s.into()).into_owned()));
    }
    pub async fn flushed(&self) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self.tx.send(Out::Flush(tx)).is_ok() {
            let _ = rx.await;
        }
    }
}

/// ESC shows as ␛; other control characters but newline and tab are dropped.
pub fn plain(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\x1b' => Some('␛'),
            '\n' | '\t' => Some(c),
            '\x00'..='\x1f' | '\x7f'..='\u{9f}' => None,
            _ => Some(c),
        })
        .collect()
}

fn stop_hint(cause: StopCause) -> Option<&'static str> {
    match cause {
        StopCause::MaxTurns => Some("turn limit reached (--max-turns or maxTurns in config; 0 = no limit)"),
        StopCause::ContextLimit => Some("context window is full"),
        _ => None,
    }
}

/// `/^\/(\S+)\s*([\s\S]*)$/` on the prompt with its leading space trimmed: the command's name and
/// its arguments, trimmed.
fn slash_command(prompt: &str) -> Option<(String, String)> {
    let rest = prompt.trim_start().strip_prefix('/')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    (end > 0).then(|| (rest[..end].to_string(), rest[end..].trim().to_string()))
}

pub async fn run_headless<R: AsyncRead + Unpin + Send + 'static>(
    h: &Harness,
    prompt: String,
    output: OutputFormat,
    control: Option<R>,
    opts: HeadlessOptions,
) -> i32 {
    let out = Output::new();
    let code = run(h, prompt, output, control, &out, opts).await;
    // main ends with process::exit, which drops what a slow pipe reader has not taken yet (#102).
    out.flushed().await;
    code
}

async fn run<R: AsyncRead + Unpin + Send + 'static>(
    h: &Harness,
    prompt: String,
    output: OutputFormat,
    control: Option<R>,
    out: &Output,
    opts: HeadlessOptions,
) -> i32 {
    let agent = &h.agent;
    // Model and tool text reaching a terminal must not carry escape sequences it would act on.
    let tty = std::io::stdout().is_terminal();
    let write = {
        let out = out.clone();
        Arc::new(move |s: &str| out.stdout(if tty { plain(s) } else { s.to_string() }))
    };
    // Dim only where it will be shown as dim: a log file or CI output must keep no escape codes
    // (NO_COLOR too).
    let dim = std::io::stderr().is_terminal() && std::env::var("NO_COLOR").map(|v| v.is_empty()).unwrap_or(true);
    let status = {
        let out = out.clone();
        Arc::new(move |s: &str| {
            let s = plain(s);
            out.stderr(if dim {
                format!("\x1b[2m{s}\x1b[0m\n")
            } else {
                format!("{s}\n")
            })
        })
    };
    let at_line_start = Arc::new(AtomicBool::new(true));

    let listener = {
        let (write, status, out, at_line_start) = (write.clone(), status.clone(), out.clone(), at_line_start.clone());
        agent.events().on(move |e: &AgentEvent| {
            if output == OutputFormat::Jsonl {
                write(&format!("{}\n", sasacode_ai::json::stringify(&e.to_jsonl())));
                return;
            }
            match e {
                AgentEvent::MessageUpdate {
                    event: StreamEvent::TextDelta { delta, .. },
                    ..
                } => {
                    write(delta);
                    at_line_start.store(delta.ends_with('\n'), Ordering::SeqCst);
                }
                AgentEvent::MessageEnd {
                    message: Message::Assistant(m),
                } => {
                    if !m.text().is_empty() && !at_line_start.load(Ordering::SeqCst) {
                        write("\n");
                        at_line_start.store(true, Ordering::SeqCst);
                    }
                }
                AgentEvent::MessageDiscarded { message } if message.stop_reason == StopReason::Error => {
                    if !at_line_start.load(Ordering::SeqCst) {
                        write("\n");
                    }
                    at_line_start.store(true, Ordering::SeqCst);
                    status(&t!("the connection dropped mid-reply; asking again"));
                }
                AgentEvent::ToolStart { call, summary, .. } => status(&format!("● {}({summary})", call.name)),
                AgentEvent::ToolEnd { result, .. } if result.is_error == Some(true) => {
                    let text: String = result.content.iter().filter_map(UserContent::as_text).collect();
                    status(&format!("  ⎿ {}", text.split('\n').next_back().unwrap_or("")));
                }
                AgentEvent::Error { error } => out.stderr(format!("{}\n", t!("error: {error}", error = plain(error)))),
                AgentEvent::ToolRepaired { call, note, .. } => status(&format!(
                    "  {}",
                    t!("↻ repaired {name}: {note}", name = call.name, note = note)
                )),
                AgentEvent::PluginError { plugin, hook, error } => out.stderr(format!(
                    "{}\n",
                    t!(
                        "plugin {plugin} ({hook}): {error}",
                        plugin = plugin,
                        hook = hook,
                        error = error
                    )
                )),
                _ => {}
            }
        })
    };
    let session = agent.session();
    if output == OutputFormat::Jsonl {
        // The context window lets a window (the GUI) say how full the conversation is before it is full.
        let window = agent.model().context_window;
        let line = json!({
            "type": "session",
            "id": session.as_ref().map(|s| Value::from(s.id())).unwrap_or(Value::Null),
            "path": session.as_ref().map(|s| Value::from(s.path().to_string_lossy().into_owned())).unwrap_or(Value::Null),
            "contextWindow": if window > 0 { Value::from(window) } else { Value::Null },
        });
        write(&format!("{}\n", sasacode_ai::json::stringify(&line)));
    }
    // Through the writer above, so a control line never lands in the middle of an event.
    let controller = control.map(|input| {
        let write = write.clone();
        start_control(
            agent,
            input,
            Arc::new(move |m: Value| write(&format!("{}\n", sasacode_ai::json::stringify(&m)))),
        )
    });
    if let Some(c) = &controller {
        c.judge(&h.judged);
    }
    // With the channel on, the plugins ask the parent (ui_request). A slash command's notices must
    // be seen: they go to stderr at once, not into the queue the headless info notices wait in.
    let ui: Option<Arc<dyn sasacode_plugin_api::UiBridge>> = match &controller {
        Some(c) => Some(c.ui()),
        None if opts.slash => Some(Arc::new(sasacode_agent::plugins::HeadlessUi)),
        None => None,
    };
    // A local server knows the context the model really gets: asked while the plugins load, as the
    // TUI does at startup, and with the TUI's warning when Ollama will cut the input short (#61).
    // A custom server not told apart may be LM Studio added before sasacode knew it (#59).
    let provider = agent.model().provider;
    let local = h
        .providers
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&provider)
        .cloned();
    let ask = local
        .as_ref()
        .is_some_and(|p| p.ollama == Some(true) || p.lmstudio == Some(true) || p.llamacpp == Some(true))
        || untold(&provider, local.as_ref());
    let (listing, _) = tokio::join!(async { if ask { h.current_listing().await } else { None } }, async {
        h.load_plugins(ui).await;
        // Unlike the TUI, a one-shot run should start with MCP servers and similar tools connected.
        h.host.settle(std::time::Duration::from_secs(30)).await;
    });
    // MCP prompts become commands once their servers answer: the list the parent gets is the run's.
    if let Some(c) = &controller {
        c.watch(&h.host);
    }
    if let Some(listed) = listing
        && listed.server.as_deref() == Some("ollama")
        && listed.context_window.is_none_or(|c| c == 0)
        && let Some(max) = listed.max_context.filter(|m| *m > 0)
    {
        let warning = t!(
            "{spec} には num_ctx が設定されていません。Ollama を OLLAMA_CONTEXT_LENGTH で起動していなければ、Ollama の既定の長さ（数千トークン）を超えた入力は黙って切り詰められます。Modelfile に PARAMETER num_ctx 32768 などを書くか、OLLAMA_CONTEXT_LENGTH を設定してください（このモデルは最大 {max}）。",
            spec = listed.spec,
            max = fmt_tokens(max)
        );
        out.stderr(format!("{}\n", t!("warning: {warning}", warning = warning)));
    }
    let controller = controller.map(Arc::new);
    let sigint = {
        let (agent, controller) = (agent.clone(), controller.clone());
        tokio::spawn(async move {
            while tokio::signal::ctrl_c().await.is_ok() {
                match &controller {
                    Some(c) => c.abort(),
                    None => agent.abort(),
                }
            }
        })
    };
    let stopped_by: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let ended: Arc<Mutex<Option<StopCause>>> = Arc::default();
    let end_listener = {
        let (stopped_by, ended) = (stopped_by.clone(), ended.clone());
        agent.events().on(move |e| {
            if let AgentEvent::AgentEnd { cause, stopped, .. } = e {
                *ended.lock().unwrap_or_else(|e| e.into_inner()) = Some(*cause);
                if let Some(s) = stopped {
                    *stopped_by.lock().unwrap_or_else(|e| e.into_inner()) = format!("{}: {}", s.plugin, s.reason);
                }
            }
        })
    };
    let fail = |error: String| match output {
        OutputFormat::Jsonl => write(&format!(
            "{}\n",
            sasacode_ai::json::stringify(&json!({ "type": "error", "error": error }))
        )),
        OutputFormat::Text => out.stderr(format!("{}\n", t!("error: {error}", error = plain(&error)))),
    };
    let cause = match opts.slash.then(|| slash_command(&prompt)).flatten() {
        // What it says reaches the parent as plugin_notify / plugin_text lines; a run it starts ends
        // as any run does.
        Some((name, args)) => {
            let command = h.host.commands().into_iter().find(|c| c.command.name == name);
            let ran = match &command {
                None => Err(t!("unknown command: /{name}", name = name)),
                Some(c) => (c.command.run)(sasacode_plugin_api::CommandContext { args })
                    .await
                    .map_err(|e| t!("/{name} failed: {error}", name = name, error = format!("{e:#}"))),
            };
            match ran {
                Ok(()) => {
                    agent.wait_for_idle().await;
                    ended
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .unwrap_or(StopCause::Done)
                }
                Err(error) => {
                    fail(error);
                    StopCause::Error
                }
            }
        }
        None => match agent.prompt_text(&prompt).await {
            Ok(c) => c,
            Err(e) => {
                out.stderr(format!("{}\n", t!("error: {error}", error = plain(&format!("{e:#}")))));
                StopCause::Error
            }
        },
    };
    // Plugins may inject follow-ups (agent_end); wait for everything to settle.
    agent.wait_for_idle().await;
    sigint.abort();
    if let Some(c) = &controller {
        c.stop();
    }
    h.shutdown().await;
    end_listener.off();
    listener.off();
    let stopped_by = stopped_by.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let hint = if !stopped_by.is_empty() {
        format!(" — {stopped_by}")
    } else if let Some(h) = stop_hint(cause) {
        format!(" — {}", t(h, &[]))
    } else {
        String::new()
    };
    if output == OutputFormat::Text && cause != StopCause::Done {
        status(&t!("[stopped: {cause}{hint}]", cause = cause.as_str(), hint = hint));
    }
    // One-shot runs are how sasacode is driven without tmux: say how to continue this conversation.
    if output == OutputFormat::Text
        && let Some(s) = &session
        && s.path().exists()
    {
        status(&t!("session {id} · continue: sasacode -r {id} -p \"…\"", id = s.id()));
    }
    if cause == StopCause::Done { 0 } else { 1 }
}
