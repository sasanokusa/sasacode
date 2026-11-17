//! `--control stdio`: the process that started this run answers approvals, sends messages and can
//! abort, over this process's stdin as JSON Lines (`packages/cli/src/control.ts`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::FutureExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::oneshot;

use sasacode_agent::agent::{Agent, ApprovalAnswer, ApprovalDecision, ApprovalRequest};
use sasacode_agent::tool_exec::ApproveFn;
use sasacode_ai::UserContent;
use sasacode_host::t;
use sasacode_plugin_api::InjectWhen;

const CLOSED: &str = "the controlling process closed stdin";

/// `--control`: only `stdio` exists, and only for a run a parent can read as JSON Lines (`-p` with
/// `--output jsonl`). Returns whether the control channel is on; anything else is an error.
pub fn control_mode(control: Option<&str>, print: Option<&str>, output: Option<&str>) -> anyhow::Result<bool> {
    let Some(control) = control else { return Ok(false) };
    if control != "stdio" {
        anyhow::bail!(t!("--control must be stdio"));
    }
    if print.is_none() || output != Some("jsonl") {
        anyhow::bail!(t!("--control stdio needs -p and --output jsonl"));
    }
    Ok(true)
}

pub type Emit = Arc<dyn Fn(Value) + Send + Sync>;

pub struct Control {
    stopped: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    reader: tokio::task::JoinHandle<()>,
}

impl Control {
    /// Stop reading. Call once the run is over.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        // Nobody reads any more: a later request is denied, not left waiting.
        self.closed.store(true, Ordering::SeqCst);
        self.reader.abort();
    }
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<ApprovalAnswer>>>>;

fn deny(feedback: Option<&str>) -> ApprovalAnswer {
    ApprovalAnswer {
        decision: ApprovalDecision::Deny,
        feedback: feedback.map(str::to_string),
    }
}

/// Lets the process that started this run answer for the user. It reads JSON Lines from `input`:
///   {"type":"approval","id":…,"decision":"allow"|"always"|"deny","feedback"?:…}  answers a request
///   {"type":"abort"}                                                             stops the run
///   {"type":"message","text":…}                                                  tells the model something mid-run
/// and asks for approval by writing `approval_request` through `emit` (one JSON object per line,
/// written by the caller's own writer so lines never interleave with the run's events). Anything
/// it cannot use becomes a `control_error` line. When `input` ends, every pending and later request
/// is denied; the run goes on.
pub fn start_control<R: AsyncRead + Unpin + Send + 'static>(agent: &Agent, input: R, emit: Emit) -> Control {
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let seq = Arc::new(AtomicU64::new(0));
    let closed = Arc::new(AtomicBool::new(false));
    let stopped = Arc::new(AtomicBool::new(false));

    let approve: ApproveFn = {
        let (pending, seq, closed, emit) = (pending.clone(), seq.clone(), closed.clone(), emit.clone());
        Arc::new(move |req: ApprovalRequest| {
            let (pending, seq, closed, emit) = (pending.clone(), seq.clone(), closed.clone(), emit.clone());
            async move {
                if closed.load(Ordering::SeqCst) {
                    return Ok(deny(Some(CLOSED)));
                }
                // Nobody was asked, so nobody is told it was cancelled.
                if req.signal.is_cancelled() {
                    return Ok(deny(None));
                }
                let id = format!("approval-{}", seq.fetch_add(1, Ordering::SeqCst) + 1);
                let (tx, rx) = oneshot::channel();
                pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), tx);
                let mut request = json!({
                    "type": "approval_request",
                    "id": id,
                    "callId": req.call.id,
                    "tool": req.tool.name(),
                    "args": req.args,
                    "reason": req.reason,
                    "cwd": req.cwd,
                });
                if req.reason.is_empty()
                    && let Some(o) = request.as_object_mut()
                {
                    // An empty reason was `undefined` in the TS, which JSON.stringify leaves out.
                    o.shift_remove("reason");
                }
                emit(request);
                // Told on its own task: an interrupted run may drop this future without polling it
                // again, and the parent still has to hear that the request is gone.
                let watcher = {
                    let (pending, emit, id, signal) = (pending.clone(), emit.clone(), id.clone(), req.signal.clone());
                    tokio::spawn(async move {
                        signal.cancelled().await;
                        cancel(&pending, &emit, &id);
                    })
                };
                let answer = rx.await.unwrap_or_else(|_| deny(Some(CLOSED)));
                watcher.abort();
                Ok(answer)
            }
            .boxed()
        })
    };
    agent.set_approve(Some(approve));

    let reader = {
        let (agent, pending, closed, stopped) = (agent.clone(), pending.clone(), closed.clone(), stopped.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(input);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match lines.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break, // a broken pipe ends the channel like EOF does
                    Ok(_) => {
                        if stopped.load(Ordering::SeqCst) {
                            return;
                        }
                        // A last line without its newline still counts.
                        let line = String::from_utf8_lossy(&buf);
                        let line = line.trim();
                        if !line.is_empty() {
                            handle(&agent, &pending, &emit, line);
                        }
                    }
                }
            }
            if stopped.load(Ordering::SeqCst) {
                return;
            }
            closed.store(true, Ordering::SeqCst);
            let waiting: Vec<oneshot::Sender<ApprovalAnswer>> = pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .drain()
                .map(|(_, tx)| tx)
                .collect();
            for tx in waiting {
                let _ = tx.send(deny(Some(CLOSED)));
            }
        })
    };
    Control {
        stopped,
        closed,
        reader,
    }
}

/// The run is being interrupted: the request is withdrawn and the call does not run (like the TUI).
fn cancel(pending: &Pending, emit: &Emit, id: &str) {
    let tx = pending.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
    if let Some(tx) = tx {
        emit(json!({ "type": "approval_cancelled", "id": id }));
        let _ = tx.send(deny(None));
    }
}

/// `JSON.stringify(v) ?? "(none)"` for a value that may be missing.
fn shown(v: Option<&Value>) -> String {
    v.map(sasacode_ai::json::stringify)
        .unwrap_or_else(|| "(none)".to_string())
}

fn handle(agent: &Agent, pending: &Pending, emit: &Emit, line: &str) {
    let fail = |error: String| emit(json!({ "type": "control_error", "error": error }));
    let Ok(msg) = sasacode_agent::session::parse_json_lenient::<Value>(line) else {
        return fail("not valid JSON".into());
    };
    let Some(msg) = msg.as_object() else {
        return fail("a control message is a JSON object".into());
    };
    let kind = msg.get("type");
    if kind.and_then(Value::as_str) == Some("abort") {
        // Withdrawn before the run unwinds, so the parent reads it before agent_end (the TS
        // signal listener ran at once, inside abort()).
        let ids: Vec<String> = pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        for id in ids {
            cancel(pending, emit, &id);
        }
        return agent.abort();
    }
    if kind.and_then(Value::as_str) == Some("message") {
        let Some(text) = msg.get("text").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) else {
            return fail("message needs a non-empty string \"text\"".into());
        };
        // Like a message typed while the TUI is busy: the model sees it at its next turn.
        return agent.inject(vec![UserContent::text(text)], InjectWhen::Next);
    }
    if kind.and_then(Value::as_str) != Some("approval") {
        return fail(format!("unknown message type {}", shown(kind)));
    }
    let Some(id) = msg.get("id").and_then(Value::as_str) else {
        return fail("approval needs a string \"id\"".into());
    };
    let decision = match msg.get("decision").and_then(Value::as_str) {
        Some("allow") => ApprovalDecision::Allow,
        Some("always") => ApprovalDecision::Always,
        Some("deny") => ApprovalDecision::Deny,
        _ => return fail("approval \"decision\" must be allow, always or deny".into()),
    };
    let feedback = match msg.get("feedback") {
        None => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return fail("approval \"feedback\" must be a string".into()),
    };
    let Some(tx) = pending.lock().unwrap_or_else(|e| e.into_inner()).remove(id) else {
        return fail(format!(
            "no pending approval with id {} (unknown, already answered or cancelled)",
            sasacode_ai::json::stringify(&id)
        ));
    };
    let _ = tx.send(ApprovalAnswer { decision, feedback });
}
