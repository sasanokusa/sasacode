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
