//! `--control stdio`: the process that started this run answers approvals, sends messages and can
//! abort, over this process's stdin as JSON Lines (`packages/cli/src/control.ts`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::FutureExt;
use indexmap::IndexMap;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::oneshot;

use sasacode_agent::agent::{Agent, ApprovalAnswer, ApprovalDecision, ApprovalRequest};
use sasacode_agent::permission::{PermissionCheck, PermissionPolicy};
use sasacode_agent::plugins::{HeadlessUi, PluginHost};
use sasacode_agent::tool_exec::ApproveFn;
use sasacode_ai::UserContent;
use sasacode_bundled::agent_judge::Verdicts;
use sasacode_host::t;
use sasacode_plugin_api::{
    InjectWhen, InputOptions, NotifyLevel, SelectManyOptions, SelectOption, ShowTextOptions, TextFormat, UiBridge,
};

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

/// Why a value cannot answer a question, or None when it can.
type Check = Box<dyn Fn(&Value) -> Option<String> + Send + Sync>;

/// A plugin's question waiting for its `ui_response`.
struct Question {
    check: Check,
    /// `Value::Null`: dismissed, answered as a headless run answers.
    settle: oneshot::Sender<Value>,
}

/// What the channel, its reader and the plugins' bridge share.
struct Shared {
    agent: Agent,
    emit: Emit,
    pending: Mutex<HashMap<String, oneshot::Sender<ApprovalAnswer>>>,
    questions: Mutex<IndexMap<String, Question>>,
    asked: AtomicU64,
    /// No parent reads any more: later requests are denied, later questions dismissed unasked.
    closed: AtomicBool,
    /// The run is over: nothing more is written.
    stopped: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    /// Every open question answered as dismissed; `announce`: the parent is told (ui_cancelled).
    fn dismiss(&self, announce: bool) {
        let open: Vec<(String, Question)> = lock(&self.questions).drain(..).collect();
        for (id, q) in open {
            if announce {
                (self.emit)(json!({ "type": "ui_cancelled", "id": id }));
            }
            let _ = q.settle.send(Value::Null);
        }
    }

    /// The run is being interrupted: the request is withdrawn and the call does not run (like the TUI).
    fn cancel(&self, id: &str) {
        let tx = lock(&self.pending).remove(id);
        if let Some(tx) = tx {
            (self.emit)(json!({ "type": "approval_cancelled", "id": id }));
            let _ = tx.send(deny(None));
        }
    }

    /// Abort the run, and dismiss the plugins' questions still open (what SIGINT does).
    fn abort_run(&self) {
        // A tool waiting on a question would otherwise keep the run from ending.
        self.dismiss(true);
        // Withdrawn before the run unwinds, so the parent reads it before agent_end (the TS signal
        // listener ran at once, inside abort()).
        let ids: Vec<String> = lock(&self.pending).keys().cloned().collect();
        for id in ids {
            self.cancel(&id);
        }
        self.agent.abort();
    }

    /// Writes `ui_request` and waits for its answer; `dismissed` when closed, dismissed or dropped.
    async fn question(
        &self,
        kind: &'static str,
        fields: Value,
        fits: impl Fn(&Value) -> bool + Send + Sync + 'static,
        expected: &'static str,
    ) -> Option<Value> {
        if self.closed.load(Ordering::SeqCst) {
            return None;
        }
        let id = format!("ui-{}", self.asked.fetch_add(1, Ordering::SeqCst) + 1);
        let (tx, rx) = oneshot::channel();
        let check: Check = Box::new(move |v: &Value| {
            (!v.is_null() && !fits(v)).then(|| format!("ui_response to a {kind} needs \"value\": {expected}, or null"))
        });
        lock(&self.questions).insert(id.clone(), Question { check, settle: tx });
        let mut line = json!({ "type": "ui_request", "id": id, "kind": kind });
        if let (Some(o), Value::Object(f)) = (line.as_object_mut(), fields) {
            o.extend(f);
        }
        (self.emit)(line);
        rx.await.ok().filter(|v| !v.is_null())
    }
}

pub struct Control {
    shared: Arc<Shared>,
    reader: tokio::task::JoinHandle<()>,
    unjudged: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Control {
    /// api.ui for the plugins: their questions go to the parent (ui_request), their notices and
    /// texts are lines too.
    pub fn ui(&self) -> Arc<dyn UiBridge> {
        Arc::new(ControlUi(self.shared.clone()))
    }

    /// Report the plugins' status items (plugin_status) and slash commands (commands) as they are
    /// now, then as they change.
    pub fn watch(&self, host: &Arc<PluginHost>) {
        let shared = self.shared.clone();
        let statuses: Mutex<IndexMap<String, String>> = Mutex::default();
        let listed: Mutex<String> = Mutex::default();
        let weak = Arc::downgrade(host);
        let report = Arc::new(move || {
            let Some(host) = weak.upgrade() else { return };
            if shared.stopped.load(Ordering::SeqCst) {
                return;
            }
            let now = host.status();
            let mut seen = lock(&statuses);
            for (key, text) in &now {
                if seen.get(key) != Some(text) {
                    seen.insert(key.clone(), text.clone());
                    (shared.emit)(json!({ "type": "plugin_status", "key": key, "text": text }));
                }
            }
            let gone: Vec<String> = seen.keys().filter(|k| !now.contains_key(*k)).cloned().collect();
            for key in gone {
                seen.shift_remove(&key);
                (shared.emit)(json!({ "type": "plugin_status", "key": key, "text": null }));
            }
            drop(seen);
            let commands: Vec<Value> = host
                .commands()
                .iter()
                .map(|c| {
                    let mut line = json!({ "name": c.command.name, "description": c.command.description });
                    if let (Some(o), Some(hint)) = (line.as_object_mut(), &c.command.argument_hint) {
                        o.insert("argumentHint".into(), json!(hint));
                    }
                    if let Some(o) = line.as_object_mut() {
                        o.insert("owner".into(), json!(c.owner));
                    }
                    line
                })
                .collect();
            let text = sasacode_ai::json::stringify(&Value::Array(commands.clone()));
            let mut last = lock(&listed);
            if *last == text {
                return;
            }
            *last = text;
            drop(last);
            (shared.emit)(json!({ "type": "commands", "commands": commands }));
        });
        let before = host.on_change();
        let r = report.clone();
        host.set_on_change(Some(Arc::new(move || {
            if let Some(b) = &before {
                b();
            }
            r();
        })));
        report();
    }

    /// The agent permission mode's judge, reported as `judged` lines.
    pub fn judge(&self, verdicts: &Verdicts) {
        let emit = self.shared.emit.clone();
        let off = verdicts.on(move |j| {
            emit(json!({
                "type": "judged", "callId": j.call_id, "tool": j.tool, "verdict": j.verdict, "reason": j.reason,
            }))
        });
        if let Some(previous) = lock(&self.unjudged).replace(off) {
            previous();
        }
    }

    /// Abort the run, and dismiss the plugins' questions still open (what SIGINT does).
    pub fn abort(&self) {
        self.shared.abort_run();
    }

    /// Stop reading. Call once the run is over.
    pub fn stop(&self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        // Nobody reads any more: a later request is denied, not left waiting.
        self.shared.closed.store(true, Ordering::SeqCst);
        self.shared.dismiss(true);
        if let Some(off) = lock(&self.unjudged).take() {
            off();
        }
        self.reader.abort();
    }
}

fn deny(feedback: Option<&str>) -> ApprovalAnswer {
    ApprovalAnswer {
        feedback: feedback.map(str::to_string),
        ..ApprovalAnswer::new(ApprovalDecision::Deny)
    }
}

/// The allow rules an "always" answer adds for this call: each one can be given back with --allow.
fn always_rules(req: &ApprovalRequest) -> Vec<String> {
    let check = PermissionCheck {
        tool: req.tool.clone(),
        args: req.args.clone(),
        cwd: req.cwd.clone(),
    };
    // A tool whose match target or paths panic: the call is asked about all the same.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| PermissionPolicy::rules_for(&check))).unwrap_or_default()
}

/// An option as the parent sees it: the plugin's may carry more.
fn shown(options: &[SelectOption]) -> Value {
    Value::Array(
        options
            .iter()
            .map(|o| {
                let mut v = json!({ "value": o.value, "label": o.label });
                if let (Some(m), Some(d)) = (v.as_object_mut(), &o.description) {
                    m.insert("description".into(), json!(d));
                }
                v
            })
            .collect(),
    )
}

/// `{ title, a, b }` with the keys whose value is None left out, as JSON.stringify leaves undefined.
fn fields(pairs: &[(&str, Option<Value>)]) -> Value {
    Value::Object(
        pairs
            .iter()
            .filter_map(|(k, v)| v.clone().map(|v| (k.to_string(), v)))
            .collect(),
    )
}

fn offered(options: &[SelectOption], v: &Value) -> bool {
    v.as_str().is_some_and(|s| options.iter().any(|o| o.value == s))
}

/// The plugins' front end while the channel is on.
struct ControlUi(Arc<Shared>);

#[async_trait::async_trait]
impl UiBridge for ControlUi {
    // A parent that has closed stdin answers nothing more: plugins see a headless run from then on.
    fn interactive(&self) -> bool {
        !self.0.closed.load(Ordering::SeqCst)
    }

    fn notify(&self, message: &str, level: NotifyLevel) {
        if self.0.stopped.load(Ordering::SeqCst) {
            return HeadlessUi.notify(message, level);
        }
        (self.0.emit)(json!({ "type": "plugin_notify", "level": level, "text": message }));
    }

    async fn show_text(&self, options: ShowTextOptions) {
        if self.0.stopped.load(Ordering::SeqCst) {
            return HeadlessUi.notify(&format!("{}\n{}", options.title, options.text), NotifyLevel::Info);
        }
        let format = match options.format {
            TextFormat::Text => "text",
            TextFormat::Markdown => "markdown",
        };
        (self.0.emit)(json!({ "type": "plugin_text", "title": options.title, "text": options.text, "format": format }));
    }

    async fn confirm(&self, title: &str, message: Option<&str>) -> bool {
        let f = fields(&[("title", Some(json!(title))), ("message", message.map(|m| json!(m)))]);
        let v = self.0.question("confirm", f, Value::is_boolean, "true or false").await;
        v.and_then(|v| v.as_bool()).unwrap_or(false)
    }

    async fn select(&self, title: &str, options: &[SelectOption]) -> Option<String> {
        let f = fields(&[("title", Some(json!(title))), ("options", Some(shown(options)))]);
        let own = options.to_vec();
        let fits = move |v: &Value| offered(&own, v);
        let v = self.0.question("select", f, fits, "one of the options' values").await;
        v.and_then(|v| v.as_str().map(str::to_string))
    }

    async fn input(&self, title: &str, o: &InputOptions) -> Option<String> {
        // Named one by one: a plugin's options carrying more must not change the line's own keys.
        let f = fields(&[
            ("title", Some(json!(title))),
            ("message", o.message.as_ref().map(|m| json!(m))),
            ("placeholder", o.placeholder.as_ref().map(|m| json!(m))),
            ("initial", o.initial.as_ref().map(|m| json!(m))),
        ]);
        let v = self.0.question("input", f, Value::is_string, "a string").await;
        v.and_then(|v| v.as_str().map(str::to_string))
    }

    async fn select_many(&self, title: &str, options: &[SelectOption], o: &SelectManyOptions) -> Option<Vec<String>> {
        let f = fields(&[
            ("title", Some(json!(title))),
            ("options", Some(shown(options))),
            ("selected", o.selected.as_ref().map(|s| json!(s))),
        ]);
        let own = options.to_vec();
        let fits = move |v: &Value| v.as_array().is_some_and(|a| a.iter().all(|x| offered(&own, x)));
        let v = self
            .0
            .question("select_many", f, fits, "an array of the options' values")
            .await;
        v.and_then(|v| {
            v.as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        })
    }
}

/// Lets the process that started this run answer for the user. It reads JSON Lines from `input`:
///   {"type":"approval","id":…,"decision":"allow"|"always"|"deny","feedback"?:…,"args"?:{…}}  answers a request
///   {"type":"abort","callId"?:…}                                stops the run, or with callId that tool call alone
///   {"type":"message","text":…}                                 tells the model something mid-run
///   {"type":"ui_response","id":…,"value":…}                     answers a plugin's question (null dismisses it)
/// and asks for approval by writing `approval_request` through `emit` (one JSON object per line,
/// written by the caller's own writer so lines never interleave with the run's events). The
/// plugins reach the parent through [`Control::ui`] (ui_request, plugin_notify, plugin_text) and
/// [`Control::watch`] (plugin_status, commands); the agent permission mode's judge is reported as
/// `judged` ([`Control::judge`]). Anything it cannot use becomes a `control_error` line. When
/// `input` ends, every pending and later request is denied, and every question dismissed; the run
/// goes on.
pub fn start_control<R: AsyncRead + Unpin + Send + 'static>(agent: &Agent, input: R, emit: Emit) -> Control {
    let shared = Arc::new(Shared {
        agent: agent.clone(),
        emit,
        pending: Mutex::default(),
        questions: Mutex::default(),
        asked: AtomicU64::new(0),
        closed: AtomicBool::new(false),
        stopped: AtomicBool::new(false),
    });
    let seq = Arc::new(AtomicU64::new(0));

    let approve: ApproveFn = {
        let shared = shared.clone();
        Arc::new(move |req: ApprovalRequest| {
            let (shared, seq) = (shared.clone(), seq.clone());
            async move {
                if shared.closed.load(Ordering::SeqCst) {
                    return Ok(deny(Some(CLOSED)));
                }
                // Nobody was asked, so nobody is told it was cancelled.
                if req.signal.is_cancelled() {
                    return Ok(deny(None));
                }
                let id = format!("approval-{}", seq.fetch_add(1, Ordering::SeqCst) + 1);
                let (tx, rx) = oneshot::channel();
                lock(&shared.pending).insert(id.clone(), tx);
                let mut request = json!({
                    "type": "approval_request",
                    "id": id,
                    "callId": req.call.id,
                    "tool": req.tool.name(),
                    "args": req.args,
                    "reason": req.reason,
                    "cwd": req.cwd,
                    "rules": always_rules(&req),
                });
                if req.reason.is_empty()
                    && let Some(o) = request.as_object_mut()
                {
                    // An empty reason was `undefined` in the TS, which JSON.stringify leaves out.
                    o.shift_remove("reason");
                }
                (shared.emit)(request);
                // Told on its own task: an interrupted run may drop this future without polling it
                // again, and the parent still has to hear that the request is gone.
                let watcher = {
                    let (shared, id, signal) = (shared.clone(), id.clone(), req.signal.clone());
                    tokio::spawn(async move {
                        signal.cancelled().await;
                        shared.cancel(&id);
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
        let shared = shared.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(input);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match lines.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break, // a broken pipe ends the channel like EOF does
                    Ok(_) => {
                        if shared.stopped.load(Ordering::SeqCst) {
                            return;
                        }
                        // A last line without its newline still counts.
                        let line = String::from_utf8_lossy(&buf);
                        let line = line.trim();
                        if !line.is_empty() {
                            handle(&shared, line);
                        }
                    }
                }
            }
            if shared.stopped.load(Ordering::SeqCst) {
                return;
            }
            shared.closed.store(true, Ordering::SeqCst);
            let waiting: Vec<oneshot::Sender<ApprovalAnswer>> =
                lock(&shared.pending).drain().map(|(_, tx)| tx).collect();
            for tx in waiting {
                let _ = tx.send(deny(Some(CLOSED)));
            }
            shared.dismiss(false); // nobody is there to be told
        })
    };
    Control {
        shared,
        reader,
        unjudged: Mutex::default(),
    }
}

/// `JSON.stringify(v) ?? "(none)"` for a value that may be missing.
fn shown_value(v: Option<&Value>) -> String {
    v.map(sasacode_ai::json::stringify)
        .unwrap_or_else(|| "(none)".to_string())
}

fn handle(shared: &Shared, line: &str) {
    let fail = |error: String| (shared.emit)(json!({ "type": "control_error", "error": error }));
    let Ok(msg) = sasacode_agent::session::parse_json_lenient::<Value>(line) else {
        return fail("not valid JSON".into());
    };
    let Some(msg) = msg.as_object() else {
        return fail("a control message is a JSON object".into());
    };
    let kind = msg.get("type");
    if kind.and_then(Value::as_str) == Some("abort") {
        return match msg.get("callId") {
            None => shared.abort_run(),
            Some(Value::String(id)) => {
                // That call alone: its result is an error, and the run goes on.
                if !shared.agent.abort_call(id) {
                    fail(format!(
                        "no running tool call with callId {}",
                        sasacode_ai::json::stringify(&json!(id))
                    ));
                }
            }
            Some(_) => fail("abort \"callId\" must be a string".into()),
        };
    }
    if kind.and_then(Value::as_str) == Some("message") {
        let Some(text) = msg.get("text").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) else {
            return fail("message needs a non-empty string \"text\"".into());
        };
        // Like a message typed while the TUI is busy: the model sees it at its next turn.
        return shared.agent.inject(vec![UserContent::text(text)], InjectWhen::Next);
    }
    if kind.and_then(Value::as_str) == Some("ui_response") {
        let Some(id) = msg.get("id").and_then(Value::as_str) else {
            return fail("ui_response needs a string \"id\"".into());
        };
        let mut questions = lock(&shared.questions);
        let Some(q) = questions.get(id) else {
            drop(questions);
            return fail(format!(
                "no pending ui_request with id {} (unknown, already answered or cancelled)",
                sasacode_ai::json::stringify(&json!(id))
            ));
        };
        let Some(value) = msg.get("value") else {
            drop(questions);
            return fail("ui_response needs \"value\" (null dismisses the question)".into());
        };
        if let Some(problem) = (q.check)(value) {
            drop(questions);
            return fail(problem);
        }
        if let Some(q) = questions.shift_remove(id) {
            let _ = q.settle.send(value.clone());
        }
        return;
    }
    if kind.and_then(Value::as_str) != Some("approval") {
        return fail(format!("unknown message type {}", shown_value(kind)));
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
    let args = match msg.get("args") {
        None => None,
        Some(Value::Object(a)) if decision != ApprovalDecision::Deny => Some(a.clone()),
        Some(_) => return fail("approval \"args\" must be an object, sent with allow or always".into()),
    };
    let Some(tx) = lock(&shared.pending).remove(id) else {
        return fail(format!(
            "no pending approval with id {} (unknown, already answered or cancelled)",
            sasacode_ai::json::stringify(&json!(id))
        ));
    };
    let _ = tx.send(ApprovalAnswer {
        decision,
        feedback,
        args,
    });
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
