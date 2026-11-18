//! `packages/agent/test/pipeline.test.ts`: the hooks between the model's output and what is stored:
//! tool_call_raw repairs, stream_delta stops, assistant_message rewrites and retries.

mod support;

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::events::AgentEvent;
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_agent::session::SessionFile;
use sasacode_ai::{
    AssistantContent, AssistantMessage, JsonObject, Message, SamplingOptions, StopReason, ThinkingContent, ToolCall,
};
use sasacode_plugin_api::{
    AssistantMessageResult, BeforeRequestResult, DeltaKind, Lang, PluginInit, StopCause, Stopped, StreamDeltaResult,
    ToolCallRawResult, hook,
};
use sasacode_testing::ReplayProvider;

use support::*;

/// pipeline.test.ts `reply`: provider "t", empty usage, a timestamp from a running clock.
fn reply(content: Vec<AssistantContent>) -> AssistantMessage {
    static CLOCK: AtomicI64 = AtomicI64::new(1);
    let mut m = support::reply(content, "t");
    m.timestamp = CLOCK.fetch_add(1, Ordering::Relaxed);
    m
}

fn raw_call(id: &str, name: &str, raw: &str) -> AssistantContent {
    AssistantContent::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        input: JsonObject::new(),
        raw_input: Some(raw.into()),
    })
}

struct Setup {
    agent: Agent,
    provider: Arc<ReplayProvider>,
    events: Arc<Mutex<Vec<AgentEvent>>>,
    _dir: tempfile::TempDir,
}

/// pipeline.test.ts `setup`: `a.txt` holds "hello\n"; mode auto; the built-in tools, then `plugins`.
async fn setup(
    script: Vec<AssistantMessage>,
    plugins: Vec<(&str, PluginInit)>,
    session: Option<Arc<SessionFile>>,
    dir: Option<tempfile::TempDir>,
) -> Setup {
    let dir = dir.unwrap_or_else(|| temp_dir("sasacode-pipeline-"));
    std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
    let api = api_name();
    let provider = register(&api, script, None);
    let mut o = AgentOptions::new(model(&api, "t", 100_000, 4000), path_str(dir.path()), "s");
    o.permissions = Some(policy(sasacode_plugin_api::PermissionMode::Auto, json!({})));
    o.session = session;
    let agent = Agent::new(o);
    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd: path_str(dir.path()),
        settings: None,
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: Lang::En,
    });
    assert!(host.load("builtin-tools", builtin_tools).await);
    for (n, p) in plugins {
        host.load(n, p).await;
    }
    let events = record(&agent);
    Setup {
        agent,
        provider,
        events,
        _dir: dir,
    }
}

/// A repair plugin that fixes one known mistake, standing in for the bundled tool-repair.
fn fixer() -> PluginInit {
    plug(|api| {
        api.on::<hook::ToolCallRaw, _, _>(|ev, _| {
            let r = if ev.name == "raed" {
                Some(ToolCallRawResult {
                    name: Some("read".into()),
                    note: Some(r#"tool "raed" → "read""#.into()),
                    input: None,
                })
            } else if ev.raw_input.as_deref() == Some("{path: 'a.txt'}") {
                Some(ToolCallRawResult {
                    input: Some(obj(json!({ "path": "a.txt" }))),
                    note: Some("JSON: quoted keys".into()),
                    name: None,
                })
            } else {
                ev.input.get("file").map(|file| ToolCallRawResult {
                    input: Some(obj(json!({ "path": file }))),
                    note: Some(r#"argument "file" → "path""#.into()),
                    name: None,
                })
            };
            async move { Ok(r) }
        })
    })
}

/// pipeline.test.ts: "tool_call_raw repairs run before validation; history shows the repaired call; the model is told"
#[tokio::test]
async fn tool_call_raw_repairs_run_before_validation_history_shows_the_repaired_call_the_model_is_told() {
    let dir = temp_dir("sasacode-pipeline-");
    let session = SessionFile::create(&dir.path().join("s1"), &path_str(dir.path()), Vec::new()).unwrap();
    let s = setup(
        vec![
            reply(vec![
                call("1", "raed", json!({ "path": "a.txt" })),
                raw_call("2", "read", "{path: 'a.txt'}"),
                call("3", "read", json!({ "file": "a.txt" })),
            ]),
            reply(vec![say("ok")]),
        ],
        vec![("fixer", fixer())],
        Some(session.clone()),
        Some(dir),
    )
    .await;
    s.agent.prompt_text("go").await.unwrap();
    let requests = s.provider.requests();
    let sent = &requests[1].messages;
    let calls = &sent[1].as_assistant().expect("an assistant message").content;
    assert_eq!(
        *calls,
        vec![
            call("1", "read", json!({ "path": "a.txt" })),
            call("2", "read", json!({ "path": "a.txt" })),
            call("3", "read", json!({ "path": "a.txt" })),
        ]
    );
    let results = &sent[2..];
    assert!(
        results.iter().all(|r| matches!(r, Message::Tool(t) if !t.is_error)),
        "{}",
        json(results)
    );
    assert!(json(&results[0]).contains(r#"[harness repaired this call: tool \"raed\" → \"read\"]"#));
    assert!(json(&results[2]).contains("hello"));
    let log = std::fs::read_to_string(session.path()).unwrap();
    assert!(log.contains(r#""type":"tool_repair""#));
    assert!(log.contains(r#""rawInput":"{path: 'a.txt'}""#));
    assert_eq!(count(&s.events, "tool_repaired"), 3);
}

/// pipeline.test.ts: "calls with signed reasoning are repaired for execution but not rewritten; truncated calls are never repaired"
#[tokio::test]
async fn calls_with_signed_reasoning_are_repaired_for_execution_but_not_rewritten_truncated_calls_are_never_repaired() {
    let thinking = AssistantContent::Thinking(ThinkingContent {
        thinking: "t".into(),
        signature: Some("sig".into()),
        redacted: None,
        reasoning_field: None,
    });
    let mut truncated = reply(vec![call("2", "raed", json!({ "path": "a.txt" }))]);
    truncated.stop_reason = StopReason::MaxTokens;
    let s = setup(
        vec![
            reply(vec![thinking, call("1", "raed", json!({ "path": "a.txt" }))]),
            truncated,
            reply(vec![say("done")]),
        ],
        vec![("fixer", fixer())],
        None,
        None,
    )
    .await;
    s.agent.prompt_text("go").await.unwrap();
    let requests = s.provider.requests();
    let first = &requests[1].messages;
    match &first[1].as_assistant().expect("an assistant message").content[1] {
        AssistantContent::ToolCall(c) => assert_eq!(c.name, "raed"),
        other => panic!("not a tool call: {other:?}"),
    }
    match &first[2] {
        Message::Tool(t) => {
            assert_eq!(t.tool_name, "read");
            assert!(!t.is_error);
        }
        other => panic!("not a tool result: {other:?}"),
    }
    let second = requests[2].messages.last().unwrap();
    assert!(json(second).contains("hit max_tokens"), "{}", json(second));
}

fn kind(k: DeltaKind) -> &'static str {
    match k {
        DeltaKind::Text => "text",
        DeltaKind::Thinking => "thinking",
        DeltaKind::Toolcall => "toolcall",
    }
}

/// pipeline.test.ts: "stream_delta can stop a response; assistant_message sees who stopped it and can continue the loop"
#[tokio::test]
async fn stream_delta_can_stop_a_response_assistant_message_sees_who_stopped_it_and_can_continue_the_loop() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    // What the assistant_message handler saw: asserted below, where a failure counts.
    let stopped_seen: Arc<Mutex<Vec<(Stopped, StopReason)>>> = Arc::default();
    let (sn, st) = (seen.clone(), stopped_seen.clone());
    let guard = plug(move |api| {
        api.on_sync::<hook::StreamDelta, _>(move |ev, _| {
            sn.lock().unwrap().push(format!("{}:{}", kind(ev.kind), ev.text));
            Ok(ev.text.contains("loop loop").then(|| StreamDeltaResult {
                stop: Some("repeating".into()),
            }))
        });
        api.on::<hook::AssistantMessage, _, _>(move |ev, _| {
            let r = ev.stopped.map(|stopped| {
                st.lock().unwrap().push((stopped, ev.message.stop_reason));
                AssistantMessageResult {
                    message: Some(AssistantMessage {
                        content: vec![say("loop")],
                        ..ev.message
                    }),
                    inject: Some("do not repeat".into()),
                    retry: false,
                }
            });
            async move { Ok(r) }
        });
    });
    let s = setup(
        vec![
            reply(vec![say("loop loop loop"), say("never streamed")]),
            reply(vec![say("fine now")]),
        ],
        vec![("guard", guard)],
        None,
        None,
    )
    .await;
    assert_eq!(s.agent.prompt_text("go").await.unwrap(), StopCause::Done);
    assert_eq!(
        *stopped_seen.lock().unwrap(),
        vec![(
            Stopped {
                plugin: "guard".into(),
                reason: "repeating".into()
            },
            StopReason::Stopped
        )]
    );
    // The second block of the stopped response is never streamed; the next response is watched again.
    assert_eq!(*seen.lock().unwrap(), vec!["text:loop loop loop", "text:fine now"]);
    let requests = s.provider.requests();
    let sent = &requests[1].messages;
    let a = sent[1].as_assistant().expect("an assistant message");
    assert_eq!(a.content.len(), 1);
    assert_eq!(a.content[0].as_text(), Some("loop"));
    assert_eq!(a.stop_reason, StopReason::Stopped);
    // Not "user interrupted".
    match &sent[2] {
        Message::User(u) => {
            assert_eq!(u.content.len(), 1);
            assert_eq!(u.content[0].as_text(), Some("do not repeat"));
        }
        other => panic!("not a user message: {other:?}"),
    }
}

/// pipeline.test.ts: "assistant_message retry discards a response and asks again (bounded)"
#[tokio::test]
async fn assistant_message_retry_discards_a_response_and_asks_again_bounded() {
    let n = Arc::new(AtomicUsize::new(0));
    let picky = plug(move |api| {
        api.on::<hook::AssistantMessage, _, _>(move |_ev, _| {
            let retry = n.fetch_add(1, Ordering::SeqCst) < 5;
            async move {
                Ok(retry.then(|| AssistantMessageResult {
                    retry: true,
                    ..Default::default()
                }))
            }
        })
    });
    let s = setup(
        vec![
            reply(vec![say("bad")]),
            reply(vec![say("bad")]),
            reply(vec![say("bad")]),
            reply(vec![say("good")]),
        ],
        vec![("picky", picky)],
        None,
        None,
    )
    .await;
    s.agent.prompt_text("go").await.unwrap();
    // First try + 2 retries, then the response is kept.
    assert_eq!(s.provider.requests().len(), 3);
    assert_eq!(count(&s.events, "message_discarded"), 2);
    assert_eq!(s.agent.messages().iter().filter(|m| m.role() == "assistant").count(), 1);
}

/// pipeline.test.ts: "stream_delta handlers must be synchronous; before_request can set sampling"
#[tokio::test]
async fn stream_delta_handlers_must_be_synchronous_before_request_can_set_sampling() {
    let slow = plug(|api| {
        api.on::<hook::StreamDelta, _, _>(|_ev, _| async { Ok(Some(StreamDeltaResult { stop: Some("x".into()) })) })
    });
    let sampler = plug(|api| {
        api.on::<hook::BeforeRequest, _, _>(|_ev, _| async {
            Ok(Some(BeforeRequestResult {
                sampling: Some(SamplingOptions {
                    temperature: Some(0.2),
                    extra_body: Some(obj(json!({ "repetition_penalty": 1.1 }))),
                    ..Default::default()
                }),
                ..Default::default()
            }))
        })
    });
    let s = setup(
        vec![reply(vec![say("hi")])],
        vec![("slow", slow), ("sampler", sampler)],
        None,
        None,
    )
    .await;
    s.agent.prompt_text("go").await.unwrap();
    match s.agent.messages().last() {
        Some(Message::Assistant(a)) => assert_eq!(a.stop_reason, StopReason::Stop),
        other => panic!("not an assistant message: {other:?}"),
    }
    assert!(s.events.lock().unwrap().iter().any(|e| matches!(
        e,
        AgentEvent::PluginError { error, .. } if error.contains("must be synchronous")
    )));
    assert_eq!(
        json(&s.provider.requests()[0].sampling),
        json(&json!({ "temperature": 0.2, "extraBody": { "repetition_penalty": 1.1 } }))
    );
}
