//! packages/cli/test/control.test.ts, the protocol: a real agent whose replies never come, so a run
//! waits until it is aborted (where the TS counted calls on a stand-in agent). The runs driven over
//! the channel are in tests/cli/control.rs, against the sasacode binary.

use std::pin::Pin;
use std::sync::atomic::AtomicUsize;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::io::{AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use sasacode_agent::agent::AgentOptions;
use sasacode_ai::{
    AssistantContent, AssistantMessage, JsonObject, Message, ModelInfo, Provider, Request, StopReason, StreamEvent,
    ToolCall,
};
use sasacode_plugin_api::{StopCause, ToolRef};

use super::*;

/// A provider that notes the messages it was sent, answers with the replies it was given, and after
/// them never answers: the run waits until it is aborted.
struct Parked {
    api: String,
    requests: Mutex<Vec<Vec<Message>>>,
    replies: Mutex<std::collections::VecDeque<String>>,
}

#[async_trait::async_trait]
impl Provider for Parked {
    fn api(&self) -> &str {
        &self.api
    }

    fn stream(&self, req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        self.requests.lock().unwrap().push(req.messages.clone());
        let start = StreamEvent::Start {
            partial: Arc::new(AssistantMessage::new(&req.model)),
        };
        if let Some(text) = self.replies.lock().unwrap().pop_front() {
            let mut message = AssistantMessage::new(&req.model);
            message.content = vec![AssistantContent::text(text)];
            return futures::stream::iter([Ok(start), Ok(StreamEvent::Done { message })]).boxed();
        }
        // What a real stream ends with when it is aborted.
        let mut message = AssistantMessage::new(&req.model);
        message.stop_reason = StopReason::Aborted;
        let signal = req.signal.clone();
        futures::stream::once(async move {
            signal.cancelled().await;
            Ok(StreamEvent::Done { message })
        })
        .boxed()
    }
}

fn obj(v: Value) -> JsonObject {
    v.as_object().cloned().unwrap_or_default()
}

fn bash() -> ToolRef {
    sasacode_tools::builtin_tools()
        .into_iter()
        .find(|t| t.name() == "bash")
        .expect("the bash tool")
}

fn answer(decision: ApprovalDecision, feedback: Option<&str>) -> ApprovalAnswer {
    ApprovalAnswer {
        decision,
        feedback: feedback.map(str::to_string),
        args: None,
    }
}

fn closed() -> ApprovalAnswer {
    answer(ApprovalDecision::Deny, Some("the controlling process closed stdin"))
}

/// Waits until `ready` holds, failing the test after 10 s.
async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Lets a line the test sent be read, for checks that something did not happen.
async fn tick() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// An agent with the channel on, `input` its stdin, and everything it writes in `out`.
struct Rig {
    agent: Agent,
    provider: Arc<Parked>,
    control: Control,
    out: Arc<Mutex<Vec<Value>>>,
}

fn agent() -> (Agent, Arc<Parked>) {
    static N: AtomicUsize = AtomicUsize::new(0);
    // The registry is process-wide and tests run in parallel: an api name of its own.
    let api = format!("control-test-{}", N.fetch_add(1, Ordering::SeqCst));
    let provider = Arc::new(Parked {
        api: api.clone(),
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(Default::default()),
    });
    sasacode_ai::register_api(&api, provider.clone());
    let model: ModelInfo = serde_json::from_value(json!({
        "id": "m", "provider": "test", "api": api, "contextWindow": 100_000, "maxOutput": 1000,
    }))
    .expect("a model");
    (Agent::new(AgentOptions::new(model, "/work", "")), provider)
}

fn rig_with<R: AsyncRead + Unpin + Send + 'static>(input: R) -> Rig {
    let (agent, provider) = agent();
    let out = Arc::new(Mutex::new(Vec::new()));
    let emit: Emit = {
        let out = out.clone();
        Arc::new(move |m| out.lock().unwrap().push(m))
    };
    let control = start_control(&agent, input, emit);
    Rig {
        agent,
        provider,
        control,
        out,
    }
}

/// A rig and the test's end of its stdin (dropped: EOF).
fn rig() -> (Rig, Option<DuplexStream>) {
    let (ours, theirs) = tokio::io::duplex(1 << 16);
    (rig_with(theirs), Some(ours))
}

async fn raw(input: &mut Option<DuplexStream>, bytes: &[u8]) {
    let w = input.as_mut().expect("stdin is open");
    w.write_all(bytes).await.expect("written");
    w.flush().await.expect("flushed");
}

/// One line: `message` as it is when a string, else as JSON.
async fn send(input: &mut Option<DuplexStream>, message: Value) {
    let line = match message {
        Value::String(s) => s,
        other => other.to_string(),
    };
    raw(input, format!("{line}\n").as_bytes()).await;
}

impl Rig {
    fn out(&self) -> Vec<Value> {
        self.out.lock().unwrap().clone()
    }

    /// What was written, once there are at least `n` lines.
    async fn emitted(&self, n: usize) -> Vec<Value> {
        until(&format!("{n} lines"), || self.out.lock().unwrap().len() >= n).await;
        self.out()
    }

    /// Asks for approval the way the agent does (`call_1`, bash `ls` in /work), changed by `over`.
    /// Started at once: the answer is awaited through the handle.
    fn ask(&self, over: impl FnOnce(&mut ApprovalRequest)) -> JoinHandle<ApprovalAnswer> {
        let approve = self.agent.approve().expect("start_control sets the approval");
        let mut req = ApprovalRequest {
            tool: bash(),
            args: obj(json!({ "command": "ls" })),
            cwd: "/work".into(),
            signal: CancellationToken::new(),
            call: ToolCall {
                id: "call_1".into(),
                name: "bash".into(),
                input: JsonObject::new(),
                raw_input: None,
            },
            reason: "needs approval".into(),
        };
        over(&mut req);
        tokio::spawn(async move { approve(req).await.expect("an answer") })
    }

    /// A run waiting for its reply: an abort ends it as aborted.
    async fn parked_run(&self) -> JoinHandle<StopCause> {
        let asked = self.provider.requests.lock().unwrap().len();
        let agent = self.agent.clone();
        let run = tokio::spawn(async move { agent.prompt_text("go").await.expect("the run") });
        until("the request", || self.provider.requests.lock().unwrap().len() > asked).await;
        run
    }
}

// ── the protocol ─────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_request_is_one_json_line_the_answer_with_its_id_settles_it() {
    let (r, mut ch) = rig();
    let first = r.ask(|_| {});
    r.emitted(1).await;
    let second = r.ask(|q| {
        q.call.id = "call_2".into();
        q.args = obj(json!({ "command": "pwd" }));
    });
    let out = r.emitted(2).await;
    assert_eq!(out.len(), 2);
    assert_eq!(
        out[0],
        json!({
            "type": "approval_request", "id": out[0]["id"], "callId": "call_1", "tool": "bash",
            "args": { "command": "ls" }, "reason": "needs approval", "cwd": "/work",
            // What "always" would allow.
            "rules": ["bash(ls)"],
        })
    );
    assert_eq!(out[1]["callId"], "call_2");
    assert_ne!(out[0]["id"], out[1]["id"]);
    // Answered out of order: each answer reaches its own request.
    send(
        &mut ch,
        json!({ "type": "approval", "id": out[1]["id"], "decision": "deny", "feedback": "no pwd" }),
    )
    .await;
    send(
        &mut ch,
        json!({ "type": "approval", "id": out[0]["id"], "decision": "allow" }),
    )
    .await;
    assert_eq!(second.await.unwrap(), answer(ApprovalDecision::Deny, Some("no pwd")));
    assert_eq!(first.await.unwrap(), answer(ApprovalDecision::Allow, None));
    assert_eq!(r.out().len(), 2); // nothing else was written
    send(
        &mut ch,
        json!({ "type": "approval", "id": out[0]["id"], "decision": "always" }),
    )
    .await; // a second answer to the same id
    let out = r.emitted(3).await;
    assert_eq!(out[2]["type"], "control_error");
    assert!(
        out[2]["error"].as_str().unwrap().contains("no pending approval"),
        "{}",
        out[2]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn always_and_an_empty_feedback_are_passed_on_as_sent() {
    let (r, mut ch) = rig();
    let p = r.ask(|_| {});
    let out = r.emitted(1).await;
    send(
        &mut ch,
        json!({ "type": "approval", "id": out[0]["id"], "decision": "always", "feedback": "" }),
    )
    .await;
    assert_eq!(p.await.unwrap(), answer(ApprovalDecision::Always, Some("")));
}

#[tokio::test(flavor = "multi_thread")]
async fn abort_calls_agent_abort() {
    let (r, mut ch) = rig();
    let run = r.parked_run().await;
    send(&mut ch, json!({ "type": "abort" })).await;
    assert_eq!(run.await.unwrap(), StopCause::Aborted);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_is_injected_for_the_next_turn() {
    let (r, mut ch) = rig();
    send(&mut ch, json!({ "type": "message", "text": "also check the tests" })).await;
    send(&mut ch, json!({ "type": "message", "text": "  " })).await;
    send(&mut ch, json!({ "type": "message" })).await;
    // Lines are handled in order: the first one, before these two errors, wrote nothing.
    let error = json!({ "type": "control_error", "error": "message needs a non-empty string \"text\"" });
    assert_eq!(r.emitted(2).await, [error.clone(), error]);
    // The model sees it at its next turn, once; the empty ones were not injected.
    r.provider.replies.lock().unwrap().push_back("ok".into());
    let run = r.parked_run().await;
    until("the next turn", || r.provider.requests.lock().unwrap().len() == 2).await;
    let sent: Vec<String> = r
        .provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|m| serde_json::to_string(m).unwrap())
        .collect();
    assert_eq!(sent[0].matches("also check the tests").count(), 0);
    assert_eq!(sent[1].matches("also check the tests").count(), 1);
    let next = r.provider.requests.lock().unwrap()[1].clone();
    assert_eq!(
        next.iter().map(Message::role).collect::<Vec<_>>(),
        ["user", "assistant", "user"]
    );
    r.agent.abort();
    assert_eq!(run.await.unwrap(), StopCause::Aborted);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_line_the_channel_cannot_use_is_a_control_error_and_the_channel_goes_on() {
    let (r, mut ch) = rig();
    let p = r.ask(|_| {});
    let out = r.emitted(1).await;
    let id = out[0]["id"].as_str().unwrap().to_string();
    let bad = [
        "not json".to_string(),
        "[1,2]".into(),
        "null".into(),
        r#"{"type":"nope"}"#.into(),
        r#"{"id":"x"}"#.into(),
        r#"{"type":"approval","decision":"allow"}"#.into(),
        r#"{"type":"approval","id":"x","decision":"maybe"}"#.into(),
        format!(
            r#"{{"type":"approval","id":{},"decision":"allow","feedback":3}}"#,
            json!(id)
        ),
        r#"{"type":"approval","id":"nobody","decision":"allow"}"#.into(),
    ];
    for line in &bad {
        send(&mut ch, json!(line)).await;
    }
    send(&mut ch, json!("   ")).await; // blank lines are not messages
    let out = r.emitted(1 + bad.len()).await;
    let errors = &out[1..];
    assert_eq!(errors.len(), bad.len());
    for e in errors {
        assert_eq!(e["type"], "control_error");
        assert!(e["error"].is_string(), "{e}");
        assert_eq!(e.as_object().map(|o| o.len()), Some(2), "{e}");
    }
    assert_eq!(errors[0]["error"], "not valid JSON");
    assert_eq!(errors[3]["error"], "unknown message type \"nope\"");
    assert!(errors[8]["error"].as_str().unwrap().contains("\"nobody\""));
    // The request is still open, and the channel still works.
    send(&mut ch, json!({ "type": "approval", "id": id, "decision": "allow" })).await;
    assert_eq!(p.await.unwrap(), answer(ApprovalDecision::Allow, None));
    let run = r.parked_run().await;
    send(&mut ch, json!({ "type": "abort" })).await;
    assert_eq!(run.await.unwrap(), StopCause::Aborted);
    assert_eq!(r.out().len(), 1 + bad.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn lines_are_cut_from_chunks_split_anywhere_crlf_multi_byte_text_a_last_line_without_a_newline() {
    let (r, mut ch) = rig();
    let p = r.ask(|_| {});
    let out = r.emitted(1).await;
    let run = r.parked_run().await;
    let line = format!(
        "{}\r\n",
        json!({ "type": "approval", "id": out[0]["id"], "decision": "deny", "feedback": "だめ" })
    );
    let bytes = line.as_bytes();
    let cut = line.find('だ').unwrap() + 1; // in the middle of a character
    raw(&mut ch, &bytes[..cut]).await;
    tick().await;
    assert_eq!(r.out().len(), 1); // half a line is not a line
    assert!(!run.is_finished());
    raw(&mut ch, &bytes[cut..]).await;
    assert_eq!(p.await.unwrap(), answer(ApprovalDecision::Deny, Some("だめ")));
    raw(&mut ch, br#"{"type":"abort"}"#).await; // no newline, then EOF
    drop(ch.take());
    assert_eq!(run.await.unwrap(), StopCause::Aborted);
}

#[tokio::test(flavor = "multi_thread")]
async fn when_the_requests_signal_aborts_first_approval_cancelled_a_deny_and_the_id_is_gone() {
    let (r, mut ch) = rig();
    let signal = CancellationToken::new();
    let p = r.ask(|q| q.signal = signal.clone());
    let out = r.emitted(1).await;
    let id = out[0]["id"].clone();
    signal.cancel();
    // What the TUI answers when its dialog is cancelled.
    assert_eq!(p.await.unwrap(), answer(ApprovalDecision::Deny, None));
    assert_eq!(r.out()[1], json!({ "type": "approval_cancelled", "id": id }));
    send(&mut ch, json!({ "type": "approval", "id": id, "decision": "allow" })).await;
    assert_eq!(r.emitted(3).await[2]["type"], "control_error");
    // Already aborted when asked: nobody was asked, so there is nothing to cancel.
    let done = CancellationToken::new();
    done.cancel();
    assert_eq!(
        r.ask(|q| q.signal = done).await.unwrap(),
        answer(ApprovalDecision::Deny, None)
    );
    assert_eq!(r.out().len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_removes_the_abort_listener_abort_after_it_writes_nothing() {
    let (r, mut ch) = rig();
    let signal = CancellationToken::new();
    let p = r.ask(|q| q.signal = signal.clone());
    let out = r.emitted(1).await;
    send(
        &mut ch,
        json!({ "type": "approval", "id": out[0]["id"], "decision": "allow" }),
    )
    .await;
    p.await.unwrap();
    signal.cancel();
    tick().await;
    assert_eq!(r.out().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn stdin_closing_denies_every_pending_request_and_every_later_one_and_does_not_abort() {
    let (r, mut ch) = rig();
    let run = r.parked_run().await;
    let waiting = r.ask(|_| {});
    r.emitted(1).await;
    let also_waiting = r.ask(|_| {});
    r.emitted(2).await;
    drop(ch.take());
    assert_eq!(waiting.await.unwrap(), closed());
    assert_eq!(also_waiting.await.unwrap(), closed());
    assert_eq!(r.ask(|_| {}).await.unwrap(), closed());
    // The later one was not even announced.
    let types: Vec<Value> = r.out().iter().map(|m| m["type"].clone()).collect();
    assert_eq!(types, ["approval_request", "approval_request"]);
    assert!(!run.is_finished());
    r.agent.abort();
    assert_eq!(run.await.unwrap(), StopCause::Aborted);
}

/// A stdin whose read fails.
struct Failing;

impl AsyncRead for Failing {
    fn poll_read(self: Pin<&mut Self>, _cx: &mut Context<'_>, _buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "EPIPE")))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_that_fails_ends_the_channel_like_eof() {
    let r = rig_with(Failing);
    // Asked with an aborted signal, nothing is written either way: denied with no reason before the
    // channel ends, as closed after.
    let approve = r.agent.approve().unwrap();
    let probe = || {
        let approve = approve.clone();
        async move {
            let signal = CancellationToken::new();
            signal.cancel();
            let req = ApprovalRequest {
                tool: bash(),
                args: JsonObject::new(),
                cwd: "/".into(),
                signal,
                call: ToolCall {
                    id: "c".into(),
                    name: "bash".into(),
                    input: JsonObject::new(),
                    raw_input: None,
                },
                reason: "r".into(),
            };
            approve(req).await.unwrap()
        }
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while probe().await != closed() {
        assert!(Instant::now() < deadline, "the channel did not end");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let a = r.ask(|q| {
        q.args = JsonObject::new();
        q.cwd = "/".into();
        q.call.id = "c".into();
        q.reason = "r".into();
    });
    assert_eq!(a.await.unwrap().decision, ApprovalDecision::Deny);
    assert_eq!(r.out(), Vec::<Value>::new());
    r.control.stop();
}

#[test]
fn control_is_stdio_only_and_only_with_p_and_output_jsonl() {
    sasacode_host::set_lang(sasacode_plugin_api::Lang::En); // these assertions quote the English strings
    assert!(!control_mode(None, Some("hi"), Some("text")).unwrap());
    assert!(!control_mode(None, None, Some("text")).unwrap());
    assert!(control_mode(Some("stdio"), Some("hi"), Some("jsonl")).unwrap());
    let needs = "--control stdio needs -p and --output jsonl";
    let only = "--control must be stdio";
    for (control, print, output, error) in [
        ("stdio", Some("hi"), "text", needs),
        ("stdio", None, "jsonl", needs),
        ("stdio", None, "text", needs),
        ("tcp", Some("hi"), "jsonl", only),
        ("", Some("hi"), "jsonl", only),
    ] {
        let e = control_mode(Some(control), print, Some(output)).unwrap_err();
        assert!(e.to_string().contains(error), "{control} {print:?} {output}: {e}");
    }
}

// ── additions for the GUI: edited args, one call's abort, the plugins' ui, status and commands ──

fn option(value: &str, label: &str) -> SelectOption {
    SelectOption::new(value, label)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_may_carry_edited_args_with_allow_or_always_with_deny_or_not_an_object_it_is_a_control_error() {
    let (r, mut ch) = rig();
    let p = r.ask(|_| {});
    let out = r.emitted(1).await;
    let id = out[0]["id"].clone();
    for bad in [
        json!({ "type": "approval", "id": id, "decision": "deny", "args": { "command": "pwd" } }),
        json!({ "type": "approval", "id": id, "decision": "allow", "args": ["pwd"] }),
        json!({ "type": "approval", "id": id, "decision": "allow", "args": null }),
    ] {
        send(&mut ch, bad).await;
    }
    let out = r.emitted(4).await;
    let error =
        json!({ "type": "control_error", "error": "approval \"args\" must be an object, sent with allow or always" });
    assert_eq!(out[1..], [error.clone(), error.clone(), error]);
    send(
        &mut ch,
        json!({ "type": "approval", "id": id, "decision": "always", "args": { "command": "pwd" } }),
    )
    .await;
    let mut expected = answer(ApprovalDecision::Always, None);
    expected.args = Some(obj(json!({ "command": "pwd" })));
    assert_eq!(p.await.unwrap(), expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn abort_with_a_call_id_not_running_or_not_a_string_is_a_control_error_and_the_run_goes_on() {
    // Stopping a running call alone: tests/agent.rs (abort_call) and tests/cli/control.rs.
    let (r, mut ch) = rig();
    let run = r.parked_run().await;
    send(&mut ch, json!({ "type": "abort", "callId": "call_9" })).await;
    send(&mut ch, json!({ "type": "abort", "callId": 3 })).await;
    assert_eq!(
        r.emitted(2).await,
        [
            json!({ "type": "control_error", "error": "no running tool call with callId \"call_9\"" }),
            json!({ "type": "control_error", "error": "abort \"callId\" must be a string" }),
        ]
    );
    tick().await;
    assert!(!run.is_finished(), "the run was not stopped");
    send(&mut ch, json!({ "type": "abort" })).await;
    assert_eq!(run.await.unwrap(), StopCause::Aborted);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugins_question_is_a_ui_request_the_ui_response_with_its_id_answers_it_null_dismisses_it() {
    let (r, mut ch) = rig();
    let ui = r.control.ui();
    assert!(ui.interactive());
    let asking = {
        let ui = ui.clone();
        tokio::spawn(async move {
            let confirmed = ui.confirm("Delete it?", Some("for good"));
            let options = [option("a", "A").describe("first"), option("b", "B")];
            let picked = ui.select("Which?", &options);
            let typed_options = InputOptions {
                placeholder: Some("x".into()),
                initial: Some("y".into()),
                ..Default::default()
            };
            let typed = ui.input("Name?", &typed_options);
            let many_options = SelectManyOptions {
                selected: Some(vec!["b".into()]),
            };
            let both = [option("a", "A"), option("b", "B")];
            let many = ui.select_many("Which ones?", &both, &many_options);
            let dismissed = ui.confirm("Again?", None);
            // Asked in this order, all open at once.
            tokio::join!(confirmed, picked, typed, many, dismissed)
        })
    };
    let out = r.emitted(5).await;
    let mut asked = out.clone();
    asked.sort_by_key(|l| l["id"].as_str().unwrap_or_default().to_string());
    assert_eq!(
        asked,
        [
            json!({ "type": "ui_request", "id": "ui-1", "kind": "confirm", "title": "Delete it?", "message": "for good" }),
            json!({ "type": "ui_request", "id": "ui-2", "kind": "select", "title": "Which?",
                "options": [{ "value": "a", "label": "A", "description": "first" }, { "value": "b", "label": "B" }] }),
            json!({ "type": "ui_request", "id": "ui-3", "kind": "input", "title": "Name?", "placeholder": "x", "initial": "y" }),
            json!({ "type": "ui_request", "id": "ui-4", "kind": "select_many", "title": "Which ones?",
                "options": [{ "value": "a", "label": "A" }, { "value": "b", "label": "B" }], "selected": ["b"] }),
            json!({ "type": "ui_request", "id": "ui-5", "kind": "confirm", "title": "Again?" }),
        ]
    );
    send(&mut ch, json!({ "type": "ui_response", "id": "ui-1", "value": true })).await;
    send(&mut ch, json!({ "type": "ui_response", "id": "ui-2", "value": "b" })).await;
    send(&mut ch, json!({ "type": "ui_response", "id": "ui-3", "value": "sasa" })).await;
    send(
        &mut ch,
        json!({ "type": "ui_response", "id": "ui-4", "value": ["a", "b"] }),
    )
    .await;
    send(&mut ch, json!({ "type": "ui_response", "id": "ui-5", "value": null })).await;
    let answers = asking.await.unwrap();
    assert_eq!(
        answers,
        (
            true,
            Some("b".to_string()),
            Some("sasa".to_string()),
            Some(vec!["a".to_string(), "b".to_string()]),
            false
        )
    );
    assert_eq!(r.out().len(), 5); // nothing else was written
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ui_response_that_cannot_answer_its_question_is_a_control_error_and_the_question_stays_open() {
    let (r, mut ch) = rig();
    let ui = r.control.ui();
    let picked = {
        let ui = ui.clone();
        tokio::spawn(async move { ui.select("Which?", &[option("a", "A")]).await })
    };
    r.emitted(1).await;
    let sure = {
        let ui = ui.clone();
        tokio::spawn(async move { ui.confirm("Sure?", None).await })
    };
    r.emitted(2).await;
    for bad in [
        json!({ "type": "ui_response", "id": "ui-1", "value": "z" }), // not offered
        json!({ "type": "ui_response", "id": "ui-2", "value": "yes" }),
        json!({ "type": "ui_response", "id": "ui-2" }),
        json!({ "type": "ui_response", "value": true }),
        json!({ "type": "ui_response", "id": "ui-9", "value": true }),
    ] {
        send(&mut ch, bad).await;
    }
    let out = r.emitted(7).await;
    let errors: Vec<&str> = out[2..]
        .iter()
        .map(|e| e["error"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        errors,
        [
            "ui_response to a select needs \"value\": one of the options' values, or null",
            "ui_response to a confirm needs \"value\": true or false, or null",
            "ui_response needs \"value\" (null dismisses the question)",
            "ui_response needs a string \"id\"",
            "no pending ui_request with id \"ui-9\" (unknown, already answered or cancelled)",
        ]
    );
    send(&mut ch, json!({ "type": "ui_response", "id": "ui-1", "value": "a" })).await;
    send(&mut ch, json!({ "type": "ui_response", "id": "ui-2", "value": false })).await;
    assert_eq!(picked.await.unwrap().as_deref(), Some("a"));
    assert!(!sure.await.unwrap());
    send(&mut ch, json!({ "type": "ui_response", "id": "ui-1", "value": "a" })).await; // answered already
    let out = r.emitted(8).await;
    assert!(
        out[7]["error"].as_str().unwrap_or_default().contains("\"ui-1\""),
        "{}",
        out[7]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn abort_dismisses_the_open_questions_with_ui_cancelled_once_stdin_closes_every_question_is_dismissed_unannounced()
 {
    let (r, mut ch) = rig();
    let ui = r.control.ui();
    let run = r.parked_run().await;
    let open = {
        let ui = ui.clone();
        tokio::spawn(async move { ui.select("Which?", &[option("a", "A")]).await })
    };
    r.emitted(1).await;
    send(&mut ch, json!({ "type": "abort" })).await;
    assert_eq!(open.await.unwrap(), None);
    assert_eq!(run.await.unwrap(), StopCause::Aborted);
    let out = r.out();
    assert_eq!(out[0]["type"], "ui_request");
    assert_eq!(out[1..], [json!({ "type": "ui_cancelled", "id": "ui-1" })]);
    let waiting = {
        let ui = ui.clone();
        tokio::spawn(async move { ui.input("Name?", &InputOptions::default()).await })
    };
    r.emitted(3).await;
    ch.take(); // EOF
    assert_eq!(waiting.await.unwrap(), None);
    assert!(!ui.interactive());
    assert!(!ui.confirm("Later?", None).await); // not even asked
    let types: Vec<Value> = r.out().iter().map(|e| e["type"].clone()).collect();
    assert_eq!(types, ["ui_request", "ui_cancelled", "ui_request"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn notices_and_shown_text_are_lines_status_items_and_commands_are_reported_now_and_as_they_change() {
    let (r, _ch) = rig();
    let ui = r.control.ui();
    ui.notify("hello", NotifyLevel::Warning);
    ui.show_text(ShowTextOptions {
        title: "Usage".into(),
        text: "12 tokens".into(),
        format: TextFormat::Markdown,
    })
    .await;
    let host = PluginHost::new(sasacode_agent::plugins::PluginHostOptions {
        agent: r.agent.clone(),
        cwd: "/work".into(),
        settings: None,
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: sasacode_plugin_api::Lang::En,
    });
    let (goal, mcp) = (host.api("goal"), host.api("mcp"));
    goal.ui().set_status("goal", Some("writing tests".into()));
    goal.register_command(sasacode_plugin_api::Command::new("goal", "set a goal", |_| async { Ok(()) }).hint("<text>"));
    r.control.watch(&host);
    goal.ui().set_status("goal", Some("done".into()));
    mcp.ui().set_status("servers", Some("MCP 1/1".into()));
    host.on_change().expect("watched")(); // nothing changed: nothing written
    goal.ui().set_status("goal", None);
    mcp.register_command(sasacode_plugin_api::Command::new("mcp", "servers", |_| async {
        Ok(())
    }));
    let goal_line = json!({ "name": "goal", "description": "set a goal", "argumentHint": "<text>", "owner": "goal" });
    assert_eq!(
        r.out(),
        [
            json!({ "type": "plugin_notify", "level": "warning", "text": "hello" }),
            json!({ "type": "plugin_text", "title": "Usage", "text": "12 tokens", "format": "markdown" }),
            json!({ "type": "plugin_status", "key": "goal:goal", "text": "writing tests" }),
            json!({ "type": "commands", "commands": [goal_line] }),
            json!({ "type": "plugin_status", "key": "goal:goal", "text": "done" }),
            json!({ "type": "plugin_status", "key": "mcp:servers", "text": "MCP 1/1" }),
            json!({ "type": "plugin_status", "key": "goal:goal", "text": null }),
            json!({ "type": "commands", "commands": [goal_line, { "name": "mcp", "description": "servers", "owner": "mcp" }] }),
        ]
    );
    // Keys in the TS order (the GUI reads them by name; the order is what a reader sees).
    let out = r.out();
    let keys: Vec<&String> = out[3]["commands"][0]
        .as_object()
        .map(|o| o.keys().collect())
        .unwrap_or_default();
    assert_eq!(keys, ["name", "description", "argumentHint", "owner"]);
    r.control.stop();
    mcp.ui().set_status("late", Some("after the run".into()));
    assert_eq!(r.out().len(), 8); // after stop, nothing more is written
}
