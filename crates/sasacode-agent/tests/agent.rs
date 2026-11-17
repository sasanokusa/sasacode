//! `packages/agent/test/agent.test.ts`: the loop end to end against a scripted model.

mod support;

use std::collections::HashSet;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use sasacode_agent::agent::{
    Agent, AgentOptions, ApprovalAnswer, ApprovalDecision, content_chars, reply_budget, response_room,
};
use sasacode_agent::events::AgentEvent;
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_agent::session::{SessionEntry, SessionFile, restore};
use sasacode_ai::{
    AssistantContent, AssistantMessage, INCOMPLETE_STREAM, ImageContent, Message, ModelInfo, Provider, Request,
    StopReason, StreamEvent, ThinkingContent, Usage, UserContent, register_api,
};
use sasacode_plugin_api::{
    AssistantMessageResult, CompleteOptions, HookContext, Lang, PermissionMode, SpendSource, StopCause,
    SubagentOptions, ToolDef, ToolKind, ToolResult, hook,
};
use sasacode_testing::ReplayProvider;
use sasacode_tools::ReadTool;

use support::*;

const P: &str = "test";

/// agent.test.ts `reply`: usage input 100, output 10.
fn reply(content: Vec<AssistantContent>) -> AssistantMessage {
    let mut m = support::reply(content, P);
    m.usage = usage(100, 10);
    m
}
fn reply_with(content: Vec<AssistantContent>, f: impl FnOnce(&mut AssistantMessage)) -> AssistantMessage {
    let mut m = reply(content);
    f(&mut m);
    m
}

/// `{ id: "m", provider: "test", api: "replay", contextWindow: 100_000, maxOutput: 4000 }`.
fn base_model(api: &str) -> ModelInfo {
    model(api, P, 100_000, 4000)
}
fn sized(api: &str, context_window: u64, max_output: u64) -> ModelInfo {
    model(api, P, context_window, max_output)
}

struct Setup {
    agent: Agent,
    provider: Arc<ReplayProvider>,
    events: Arc<Mutex<Vec<AgentEvent>>>,
    api: String,
    dir: tempfile::TempDir,
}

impl Setup {
    fn requests(&self) -> Vec<Request> {
        self.provider.requests()
    }
    fn file(&self, name: &str) -> std::path::PathBuf {
        self.dir.path().join(name)
    }
}

/// `setup(script, opts)`: system prompt "sys", the four built-in tools, every event recorded. `f`
/// gets the options with the model already on this test's api.
fn setup(script: Vec<AssistantMessage>, f: impl FnOnce(&mut AgentOptions)) -> Setup {
    let dir = temp_dir("sasacode-agent-");
    let api = api_name();
    let provider = register(&api, script, None);
    let mut o = AgentOptions::new(base_model(&api), path_str(dir.path()), "sys");
    o.tools = tools();
    f(&mut o);
    let agent = Agent::new(o);
    let events = record(&agent);
    Setup {
        agent,
        provider,
        events,
        api,
        dir,
    }
}

fn last(req: &Request) -> &Message {
    req.messages.last().expect("a message")
}

fn tool_results(messages: &[Message]) -> Vec<&sasacode_ai::ToolResultMessage> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::Tool(t) => Some(t),
            _ => None,
        })
        .collect()
}

fn image(data: &str) -> UserContent {
    UserContent::Image(ImageContent {
        media_type: "image/png".into(),
        data: data.into(),
    })
}

/// agent.test.ts: "fixes a bug end to end: read, edit, verify with bash, then stop"
#[tokio::test]
async fn fixes_a_bug_end_to_end_read_edit_verify_with_bash_then_stop() {
    // The TS verified with `bun -e "import {add} from './add.js'; console.log(add(2,3))"`. The Rust
    // build needs no bun (ADR-009), so bash evaluates the operator the edited file now holds.
    let check = r#"echo $((2 $(sed -n 's/.*=> a \(.\) b;.*/\1/p' add.js) 3))"#;
    let s = setup(
        vec![
            reply(vec![
                say("Let me look."),
                call("1", "read", json!({ "path": "add.js" })),
            ]),
            reply(vec![call(
                "2",
                "edit",
                json!({ "path": "add.js", "old_string": "a - b", "new_string": "a + b" }),
            )]),
            reply(vec![call("3", "bash", json!({ "command": check }))]),
            reply(vec![say("Fixed: add now returns 5 for (2,3).")]),
        ],
        |o| o.permissions = Some(policy(PermissionMode::Auto, json!({}))),
    );
    std::fs::write(s.file("add.js"), "export const add = (a, b) => a - b;\n").unwrap();
    assert_eq!(s.agent.prompt_text("add is broken").await.unwrap(), StopCause::Done);
    assert!(std::fs::read_to_string(s.file("add.js")).unwrap().contains("a + b"));
    let requests = s.requests();
    assert_eq!(requests.len(), 4);
    let last = last(&requests[3]);
    assert_eq!(last.role(), "tool");
    assert!(json(last).contains("5\\n[exit code 0]"), "{}", json(last));
}

/// agent.test.ts: "tool errors, unknown tools and bad arguments go back to the model"
#[tokio::test]
async fn tool_errors_unknown_tools_and_bad_arguments_go_back_to_the_model() {
    let s = setup(
        vec![
            reply(vec![
                call("1", "nope", json!({})),
                call("2", "read", json!({ "path": 42 })),
                call("3", "read", json!({ "path": "missing.txt" })),
            ]),
            reply(vec![say("ok")]),
        ],
        |_| {},
    );
    s.agent.prompt_text("go").await.unwrap();
    let requests = s.requests();
    let results = tool_results(&requests[1].messages);
    assert_eq!(
        results.iter().map(|r| r.is_error).collect::<Vec<_>>(),
        vec![true, true, true]
    );
    assert!(json(&results).contains("Unknown tool"));
    assert!(json(&results).contains("input.path must be string"));
}

/// agent.test.ts: "approval: deny with feedback, always-allow adds a rule, headless denies"
#[tokio::test]
async fn approval_deny_with_feedback_always_allow_adds_a_rule_headless_denies() {
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let a = asked.clone();
    let s = setup(
        vec![
            reply(vec![call("1", "bash", json!({ "command": "echo a" }))]),
            reply(vec![call("2", "bash", json!({ "command": "echo b" }))]),
            reply(vec![call("3", "bash", json!({ "command": "echo b" }))]),
            reply(vec![say("done")]),
        ],
        |o| {
            o.approve = Some(Arc::new(move |r| {
                let command = r
                    .args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                a.lock().unwrap().push(command.clone());
                Box::pin(async move {
                    Ok(if command == "echo a" {
                        ApprovalAnswer {
                            decision: ApprovalDecision::Deny,
                            feedback: Some("use printf".into()),
                        }
                    } else {
                        ApprovalAnswer::new(ApprovalDecision::Always)
                    })
                })
            }))
        },
    );
    s.agent.prompt_text("go").await.unwrap();
    // The third call is covered by the always rule.
    assert_eq!(*asked.lock().unwrap(), vec!["echo a", "echo b"]);
    assert!(json(last(&s.requests()[1])).contains("User feedback: use printf"));

    let headless = setup(
        vec![
            reply(vec![call("1", "bash", json!({ "command": "echo x" }))]),
            reply(vec![say("k")]),
        ],
        |_| {},
    );
    headless.agent.prompt_text("go").await.unwrap();
    assert!(json(last(&headless.requests()[1])).contains("no user is available"));
}

/// agent.test.ts: "a call is approved right before it runs: its approver sees the previous call's result"
#[tokio::test]
async fn a_call_is_approved_right_before_it_runs_its_approver_sees_the_previous_calls_result() {
    let order: Arc<Mutex<Vec<String>>> = Arc::default();
    let saw_previous_result = Arc::new(AtomicBool::new(false));
    let s = setup(
        vec![
            reply(vec![
                call("1", "bash", json!({ "command": "echo first" })),
                call("2", "bash", json!({ "command": "echo second" })),
            ]),
            reply(vec![say("ok")]),
        ],
        |o| o.permissions = Some(policy(PermissionMode::Ask, json!({}))),
    );
    let (o, saw, agent) = (order.clone(), saw_previous_result.clone(), s.agent.clone());
    s.agent.set_approve(Some(Arc::new(move |r| {
        let command = r
            .args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        o.lock().unwrap().push(format!("ask:{command}"));
        if command == "echo second" {
            let last = agent.messages().last().map(json).unwrap_or_default();
            saw.store(last.contains("first"), Ordering::SeqCst);
        }
        Box::pin(async { Ok(ApprovalAnswer::new(ApprovalDecision::Allow)) })
    })));
    let o = order.clone();
    let _ = s.agent.events().on(move |e| {
        if let AgentEvent::ToolStart { call, .. } = e {
            let command = call.input.get("command").and_then(|v| v.as_str()).unwrap_or_default();
            o.lock().unwrap().push(format!("run:{command}"));
        }
    });
    s.agent.prompt_text("go").await.unwrap();
    // Not "ask, ask, run, run": each call is approved after the one before it has finished.
    assert_eq!(
        *order.lock().unwrap(),
        vec!["ask:echo first", "run:echo first", "ask:echo second", "run:echo second"]
    );
    assert!(saw_previous_result.load(Ordering::SeqCst));
}

/// agent.test.ts: "interrupting a running tool reports it and tells the model on the next prompt"
#[tokio::test]
async fn interrupting_a_running_tool_reports_it_and_tells_the_model_on_the_next_prompt() {
    let s = setup(
        vec![
            reply(vec![call("1", "bash", json!({ "command": "sleep 20" }))]),
            reply(vec![say("ok")]),
        ],
        |o| o.permissions = Some(policy(PermissionMode::Auto, json!({}))),
    );
    let agent = s.agent.clone();
    let _ = s.agent.events().on(move |e| {
        if matches!(e, AgentEvent::ToolStart { .. }) {
            let agent = agent.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                agent.abort();
            });
        }
    });
    assert_eq!(s.agent.prompt_text("sleep").await.unwrap(), StopCause::Aborted);
    assert!(json(s.agent.messages().last().unwrap()).contains("interrupted by the user"));
    s.agent.prompt_text("next").await.unwrap();
    let last_user = json(last(&s.requests()[1]));
    assert!(last_user.contains("interrupted the previous response"), "{last_user}");
}

/// agent.test.ts: "messages sent while running are delivered next turn"
#[tokio::test]
async fn messages_sent_while_running_are_delivered_next_turn() {
    let s = setup(
        vec![
            reply(vec![call("1", "read", json!({ "path": "add.js" }))]),
            reply(vec![say("saw it")]),
        ],
        |_| {},
    );
    std::fs::write(s.file("add.js"), "export const add = (a, b) => a + b;\n").unwrap();
    let sent: Arc<Mutex<Vec<tokio::task::JoinHandle<anyhow::Result<StopCause>>>>> = Arc::default();
    let (agent, h) = (s.agent.clone(), sent.clone());
    let _ = s.agent.events().on(move |e| {
        if matches!(e, AgentEvent::ToolStart { .. }) {
            let agent = agent.clone();
            // `void agent.prompt("also check tests")`
            h.lock()
                .unwrap()
                .push(start(async move { agent.prompt_text("also check tests").await }));
        }
    });
    s.agent.prompt_text("look").await.unwrap();
    let handles = std::mem::take(&mut *sent.lock().unwrap());
    for h in handles {
        h.await.unwrap().unwrap();
    }
    s.agent.wait_for_idle().await;
    let requests = s.requests();
    match last(&requests[1]) {
        Message::User(u) => {
            assert_eq!(u.content.len(), 1);
            assert_eq!(u.content[0].as_text(), Some("also check tests"));
        }
        other => panic!("not a user message: {other:?}"),
    }
}

/// agent.test.ts: "read-only tools in one turn run in parallel"
#[tokio::test]
async fn read_only_tools_in_one_turn_run_in_parallel() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (a, p) = (active.clone(), peak.clone());
    // `{ ...readTool, name: "slow", execute }`: read's definition and flags, another body.
    let read = ReadTool;
    use sasacode_plugin_api::Tool;
    let slow = ToolDef::new(
        "slow",
        &read.description(),
        read.parameters(),
        read.kind(),
        move |_args, _ctx| {
            let (a, p) = (a.clone(), p.clone());
            async move {
                let now = a.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                a.fetch_sub(1, Ordering::SeqCst);
                Ok(ToolResult::text("x"))
            }
        },
    )
    .concurrent(read.concurrent())
    .always_load(read.always_load())
    .into_ref();
    let s = setup(
        vec![
            reply(vec![
                call("1", "slow", json!({ "path": "a" })),
                call("2", "slow", json!({ "path": "b" })),
                call("3", "slow", json!({ "path": "c" })),
            ]),
            reply(vec![say("k")]),
        ],
        |o| {
            o.tools = vec![slow];
            o.permissions = Some(policy(PermissionMode::Auto, json!({})));
        },
    );
    s.agent.prompt_text("go").await.unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), 3);
}

/// agent.test.ts: "context limit stops the loop with an event"
#[tokio::test]
async fn context_limit_stops_the_loop_with_an_event() {
    let big = reply_with(vec![call("1", "read", json!({ "path": "add.js" }))], |m| {
        m.usage = usage(99_000, 10)
    });
    let s = setup(vec![big, reply(vec![say("never")])], |_| {});
    assert_eq!(s.agent.prompt_text("go").await.unwrap(), StopCause::ContextLimit);
    assert!(count(&s.events, "context_limit") > 0);
}

/// agent.test.ts: "a small window is not full at once: the room kept for the reply scales with it"
#[tokio::test]
async fn a_small_window_is_not_full_at_once_the_room_kept_for_the_reply_scales_with_it() {
    // 8K window with the 32K maxOutput unknown models get: 25% used must not count as full.
    let used = reply_with(vec![call("1", "read", json!({ "path": "add.js" }))], |m| {
        m.usage = usage(2000, 50)
    });
    let s = setup(vec![used, reply(vec![say("done")])], |o| {
        o.model.context_window = 8192;
        o.model.max_output = 32_000;
    });
    assert_eq!(s.agent.prompt_text("go").await.unwrap(), StopCause::Done);
    assert_eq!(count(&s.events, "context_limit"), 0);
    assert_eq!(response_room(&sized(&s.api, 8192, 32_000)), 1228);
    assert_eq!(response_room(&sized(&s.api, 1_000_000, 128_000)), 16_384);
}

/// agent.test.ts: "subagents' hooks carry their own ctx.agent; the main agent's is undefined"
#[tokio::test]
async fn subagents_hooks_carry_their_own_ctx_agent_the_main_agents_is_undefined() {
    let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let s = setup(vec![reply(vec![say("main")])], |_| {});
    let sn = seen.clone();
    s.agent
        .hooks()
        .on::<hook::AssistantMessage, _, _>("test", move |_e, ctx: HookContext| {
            sn.lock().unwrap().push(ctx.agent);
            async { Ok(None) }
        });
    s.agent.prompt_text("go").await.unwrap();
    register(&s.api, vec![reply(vec![say("a")]), reply(vec![say("b")])], None);
    let sub = |p: &str| SubagentOptions {
        prompt: p.into(),
        ..Default::default()
    };
    let (x, y) = futures::join!(s.agent.run_subagent(sub("x")), s.agent.run_subagent(sub("y")));
    x.unwrap();
    y.unwrap();
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen[0], None);
    assert!(
        seen[1..]
            .iter()
            .all(|a| a.as_deref().is_some_and(|a| a.starts_with("sub-"))),
        "{seen:?}"
    );
    assert_eq!(seen[1..].iter().collect::<HashSet<_>>().len(), 2);
}

/// agent.test.ts: "maxTurns is opt-in"
#[tokio::test]
async fn max_turns_is_opt_in() {
    let lp = || reply(vec![call("x", "read", json!({ "path": "add.js" }))]);
    let s = setup(vec![lp(), lp(), lp()], |o| o.max_turns = Some(2));
    assert_eq!(s.agent.prompt_text("go").await.unwrap(), StopCause::MaxTurns);
}

/// A provider whose stream throws "boom".
struct Failing;

#[async_trait]
impl Provider for Failing {
    fn api(&self) -> &str {
        "replay"
    }
    fn stream(&self, _req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        Box::pin(futures::stream::once(async { Err(anyhow::anyhow!("boom")) }))
    }
}

/// agent.test.ts: "provider errors end the run with an error event"
#[tokio::test]
async fn provider_errors_end_the_run_with_an_error_event() {
    let dir = temp_dir("sasacode-agent-");
    let api = api_name();
    register_api(&api, Arc::new(Failing));
    let mut o = AgentOptions::new(base_model(&api), path_str(dir.path()), "s");
    o.tools = tools();
    let agent = Agent::new(o);
    let events = record(&agent);
    assert_eq!(agent.prompt_text("go").await.unwrap(), StopCause::Error);
    let errors: Vec<String> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Error { error } => Some(error.clone()),
            _ => None,
        })
        .collect();
    assert!(errors.contains(&"boom".to_string()), "{errors:?}");
}

/// agent.test.ts: "the key in use is redacted from the first message too (#34)"
#[tokio::test]
async fn the_key_in_use_is_redacted_from_the_first_message_too_34() {
    let sessions = temp_dir("sasacode-agent-sessions-");
    let session = SessionFile::create(sessions.path(), &path_str(sessions.path()), Vec::new()).unwrap();
    let path = session.path().to_path_buf();
    let s = setup(vec![reply(vec![say("ok")]), reply(vec![say("ok")])], |o| {
        o.session = Some(session);
        o.get_api_key = Some(Arc::new(|_| {
            Box::pin(async { Some("mock-secret-abcdef123".to_string()) })
        }));
    });
    s.agent.prompt_text("first mock-secret-abcdef123").await.unwrap();
    s.agent.prompt_text("second mock-secret-abcdef123").await.unwrap();
    let raw = std::fs::read_to_string(path).unwrap();
    assert!(!raw.contains("mock-secret-abcdef123"));
    assert!(raw.contains("first [REDACTED]"));
}

/// agent.test.ts: "sessions persist every message, redact secrets, and resume to the last complete turn"
#[tokio::test]
async fn sessions_persist_every_message_redact_secrets_and_resume_to_the_last_complete_turn() {
    let sessions = temp_dir("sasacode-agent-sessions-");
    let session = SessionFile::create(sessions.path(), &path_str(sessions.path()), Vec::new()).unwrap();
    session.add_secret(Some("sk-secret-123456"));
    let s = setup(
        vec![
            reply(vec![call("1", "bash", json!({ "command": "echo sk-secret-123456" }))]),
            reply(vec![say("done")]),
        ],
        |o| {
            o.session = Some(session.clone());
            o.permissions = Some(policy(PermissionMode::Auto, json!({})));
        },
    );
    s.agent.prompt_text("print it").await.unwrap();
    let raw = std::fs::read_to_string(session.path()).unwrap();
    assert!(!raw.contains("sk-secret-123456"));
    assert!(raw.contains("[REDACTED]"));

    let (_, entries) = SessionFile::open(session.path(), Vec::new()).unwrap();
    assert_eq!(restore(&entries).messages.len(), 4);
    assert_eq!(restore(&entries).model.as_deref(), Some("test/m"));
    // Simulate a crash after a tool call was issued but before its result was written.
    session
        .append(&SessionEntry::Message {
            message: Message::Assistant(reply(vec![call("9", "bash", json!({ "command": "x" }))])),
        })
        .unwrap();
    let mut f = std::fs::OpenOptions::new().append(true).open(session.path()).unwrap();
    f.write_all(br#"{"type":"mess"#).unwrap(); // torn line
    drop(f);
    let again = restore(&SessionFile::open(session.path(), Vec::new()).unwrap().1);
    // The dangling call gets a result saying its outcome is unknown (it may have run), so the history stays valid for every API.
    assert!(again.repaired);
    assert_eq!(again.messages.len(), 6);
    match again.messages.last().unwrap() {
        Message::Tool(t) => {
            assert_eq!(t.tool_call_id, "9");
            assert!(t.is_error);
            assert!(t.content[0].as_text().unwrap().contains("Result unknown"));
        }
        other => panic!("not a tool result: {other:?}"),
    }
}

/// agent.test.ts: "built-in tools register through the plugin API"
#[tokio::test]
async fn built_in_tools_register_through_the_plugin_api() {
    let dir = temp_dir("sasacode-agent-");
    let agent = Agent::new(AgentOptions::new(base_model(&api_name()), path_str(dir.path()), "s"));
    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd: path_str(dir.path()),
        settings: None,
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: Lang::En,
    });
    assert!(host.load(sasacode_tools::PLUGIN_NAME, builtin_tools).await);
    assert_eq!(
        agent
            .get_tools()
            .iter()
            .map(|t| t.name().to_string())
            .collect::<Vec<_>>(),
        vec!["read", "write", "edit", "bash"]
    );
    assert_eq!(host.tool_owner("bash").as_deref(), Some("builtin-tools"));
}

/// agent.test.ts: "a model without image support gets a note in place of each image"
#[tokio::test]
async fn a_model_without_image_support_gets_a_note_in_place_of_each_image() {
    let s = setup(vec![reply(vec![say("ok")])], |o| o.model.images = Some(false));
    s.agent
        .prompt(vec![UserContent::text("see"), image("AAAA")], None)
        .await
        .unwrap();
    let sent = json(&s.requests()[0].messages);
    assert!(!sent.contains("AAAA"));
    assert!(sent.contains("[image omitted"));
    // The history keeps it for another model.
    assert!(json(&s.agent.messages()).contains("AAAA"));
}

/// agent.test.ts: "max_tokens shrinks to the room left in a small window, and is left alone otherwise"
#[test]
fn max_tokens_shrinks_to_the_room_left_in_a_small_window_and_is_left_alone_otherwise() {
    let small = sized("replay", 8192, 32_000);
    let used = Message::Assistant(reply_with(vec![say("x")], |m| m.usage = usage(6000, 100)));
    assert_eq!(
        reply_budget(&small, std::slice::from_ref(&used), 0, 0),
        Some(8192 - (6100.0f64 * 1.1).ceil() as u64)
    );
    let fuller = Message::Assistant(reply_with(vec![say("x")], |m| m.usage = usage(8000, 100)));
    assert_eq!(reply_budget(&small, &[fuller], 0, 0), Some(1024));
    assert_eq!(reply_budget(&base_model("replay"), &[used], 0, 0), None);
}

/// agent.test.ts: "after a compaction, the nearly full usage of a kept message does not squeeze the next reply"
#[tokio::test]
async fn after_a_compaction_the_nearly_full_usage_of_a_kept_message_does_not_squeeze_the_next_reply() {
    let small = sized("replay", 32_000, 8_000);
    let stale = reply_with(vec![say("sure")], |m| m.usage = usage(28_000, 500));
    let kept = vec![
        user("[Summary of the earlier conversation] short"),
        Message::Assistant(stale),
    ];
    // What it was: the stale usage believed.
    assert_eq!(reply_budget(&small, &kept, 2000, 0), Some(1024));
    assert_eq!(reply_budget(&small, &kept, 2000, kept.len()), None);

    let s = setup(vec![reply(vec![say("written")])], |o| {
        o.model.context_window = 32_000;
        o.model.max_output = 8_000;
    });
    // What compaction does: a summary, then the kept tail.
    s.agent.replace_messages(kept).unwrap();
    s.agent.prompt_text("now write the file").await.unwrap();
    assert_eq!(s.requests()[0].max_tokens, None);
}

/// One complete() call for `sent` below: what max_tokens the request carried.
async fn sent(
    window: (u64, u64),
    text: &str,
    max_tokens: Option<u64>,
    to: Option<&str>,
    other: Option<(u64, u64)>,
) -> Option<u64> {
    let s = setup(vec![reply(vec![say("ok")])], |o| {
        o.model.context_window = window.0;
        o.model.max_output = window.1;
        let api = o.model.api.clone();
        o.resolve_model = Some(Arc::new(move |_| {
            let (cw, mo) = other.expect("no other model");
            Ok(sized(&api, cw, mo))
        }));
    });
    s.agent
        .complete(CompleteOptions {
            system: "s".into(),
            messages: vec![user(text)],
            max_tokens,
            model: to.map(str::to_string),
            signal: None,
        })
        .await
        .unwrap();
    s.requests()[0].max_tokens
}

/// agent.test.ts: "complete() asks for no more than the window has room for, nor more than the model's maximum (#60)"
#[tokio::test]
async fn complete_asks_for_no_more_than_the_window_has_room_for_nor_more_than_the_models_maximum_60() {
    // A side request (a compaction summary) in an 8K window: vLLM rejects input + max_tokens over the window.
    let small = (8192, 32_000);
    let big = (1_000_000, 64_000);
    let long = "x".repeat(20_000);
    let input = (((1 + content_chars(&user(&long))) as f64 / 4.0) * 1.1).ceil() as u64;
    assert_eq!(sent(small, &long, None, None, None).await, Some(8192 - input));
    // A smaller request is kept.
    assert_eq!(sent(small, &long, Some(512), None, None).await, Some(512));
    // The window of the model it goes to.
    assert_eq!(
        sent(big, &long, None, Some("t/small"), Some(small)).await,
        Some(8192 - input)
    );
    // A cloud model's window: as before.
    assert_eq!(sent(big, &long, None, None, None).await, Some(8192));
    assert_eq!(sent((big.0, 4096), "hi", None, None, None).await, Some(4096));
}

/// agent.test.ts: "complete() sizes max_tokens from what it sends, not from usage reported in the messages it is given (#60)"
#[tokio::test]
async fn complete_sizes_max_tokens_from_what_it_sends_not_from_usage_reported_in_the_messages_it_is_given_60() {
    // A plugin passing session history: the usage there was the main loop's request (system prompt,
    // tools), and after a compaction the kept tail still reports the nearly full window it saw.
    let s = setup(vec![reply(vec![say("A title")])], |o| {
        o.model.context_window = 200_000;
        o.model.max_output = 64_000;
    });
    let stale = reply_with(vec![say("Done.")], |m| m.usage = usage(185_000, 500));
    s.agent
        .complete(CompleteOptions {
            system: "Title it.".into(),
            messages: vec![
                user("[Summary] fixed the parser"),
                user("and the tests?"),
                Message::Assistant(stale),
                user("thanks"),
            ],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(s.requests()[0].max_tokens, Some(8192));
}

/// agent.test.ts: "an attached image counts as an image, not as its base64, when max_tokens is sized"
#[tokio::test]
async fn an_attached_image_counts_as_an_image_not_as_its_base64_when_max_tokens_is_sized() {
    // A 3 MB screenshot is ~4 MB of base64: read as text it looked like a full window, and the
    // reply was squeezed to 1,024 tokens, which a thinking model spends before it says anything.
    let s = setup(vec![reply(vec![say("a jet")])], |o| {
        o.model.context_window = 131_072;
        o.model.max_output = 32_000;
    });
    s.agent
        .prompt(vec![UserContent::text("describe"), image(&"A".repeat(4_000_000))], None)
        .await
        .unwrap();
    assert_eq!(s.requests()[0].max_tokens, None);
}

/// agent.test.ts: "a reply whose stream was cut is thrown away and asked for again, not taken as the answer"
#[tokio::test]
async fn a_reply_whose_stream_was_cut_is_thrown_away_and_asked_for_again_not_taken_as_the_answer() {
    let cut = reply_with(
        vec![AssistantContent::Thinking(ThinkingContent {
            thinking: "let me see, the file".into(),
            signature: None,
            redacted: None,
            reasoning_field: None,
        })],
        |m| {
            m.stop_reason = StopReason::Error;
            m.error_message = Some(INCOMPLETE_STREAM.into());
        },
    );
    let s = setup(vec![cut, reply(vec![say("the answer")])], |_| {});
    assert_eq!(s.agent.prompt_text("q").await.unwrap(), StopCause::Done);
    assert_eq!(s.requests().len(), 2);
    assert_eq!(count(&s.events, "message_discarded"), 1);
    let answers: Vec<String> = s
        .agent
        .messages()
        .iter()
        .filter_map(|m| m.as_assistant().map(|a| json(&a.content)))
        .collect();
    assert_eq!(answers, vec![json(&vec![say("the answer")])]);
}

/// Records whether each request went out with its signal still live, then replays.
struct Watching {
    inner: Arc<ReplayProvider>,
    sent: Arc<Mutex<Vec<bool>>>,
}

#[async_trait]
impl Provider for Watching {
    fn api(&self) -> &str {
        self.inner.api()
    }
    fn stream(&self, req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        self.sent.lock().unwrap().push(!req.signal.is_cancelled());
        self.inner.stream(req)
    }
}

/// agent.test.ts: "a subagent given a signal already aborted does not run (#125)"
#[tokio::test]
async fn a_subagent_given_a_signal_already_aborted_does_not_run_125() {
    let s = setup(Vec::new(), |_| {});
    // Before the fix the child's run had its own controller, never aborted: the request went out and the reply came back.
    let sent: Arc<Mutex<Vec<bool>>> = Arc::default();
    let inner = register(&s.api, vec![reply(vec![say("ran anyway")])], None);
    register_api(
        &s.api,
        Arc::new(Watching {
            inner,
            sent: sent.clone(),
        }),
    );
    let ac = CancellationToken::new();
    ac.cancel();
    let r = s
        .agent
        .run_subagent(SubagentOptions {
            prompt: "x".into(),
            signal: Some(ac),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.cause, StopCause::Aborted);
    assert_eq!(r.text, "");
    // A fetch given an aborted signal sends nothing.
    assert!(!sent.lock().unwrap().contains(&true));
}

fn source(s: SpendSource) -> &'static str {
    match s {
        SpendSource::Subagent => "subagent",
        SpendSource::Side => "side",
        SpendSource::Discarded => "discarded",
    }
}

/// agent.test.ts: "what subagents, side requests and discarded replies use is logged and reported once each"
#[tokio::test]
async fn what_subagents_side_requests_and_discarded_replies_use_is_logged_and_reported_once_each() {
    let sessions = temp_dir("sasacode-agent-sessions-");
    let session = SessionFile::create(sessions.path(), &path_str(sessions.path()), Vec::new()).unwrap();
    let cost = |n: u64| Usage {
        input: n,
        cost: n as f64 / 100.0,
        ..Usage::default()
    };
    let s = setup(vec![reply_with(vec![say("main")], |m| m.usage = cost(1))], |o| {
        o.session = Some(session.clone())
    });
    let hooked: Arc<Mutex<Vec<String>>> = Arc::default();
    let h = hooked.clone();
    s.agent.hooks().on::<hook::Usage, _, _>("test", move |sp, _| {
        h.lock()
            .unwrap()
            .push(format!("{}:{}", source(sp.source), sp.usage.input));
        async { Ok(None) }
    });
    let retried = Arc::new(AtomicBool::new(false));
    s.agent
        .hooks()
        .on::<hook::AssistantMessage, _, _>("test", move |_e, _| {
            let first = !retried.swap(true, Ordering::SeqCst);
            async move {
                Ok(first.then(|| AssistantMessageResult {
                    retry: true,
                    ..Default::default()
                }))
            }
        });
    register(
        &s.api,
        vec![
            reply_with(vec![say("thrown away")], |m| m.usage = cost(2)),
            reply_with(vec![say("main")], |m| m.usage = cost(1)),
        ],
        None,
    );
    s.agent.prompt_text("go").await.unwrap();
    // The usage hook runs on its own task (`void this.hooks.run("usage", s)`): let it finish.
    s.agent.wait_for_tasks().await;
    // A subagent with two turns; a hook inside it cannot see "usage" (only the parent reports it).
    register(
        &s.api,
        vec![
            reply_with(vec![call("1", "read", json!({ "path": "nope" }))], |m| {
                m.usage = cost(3)
            }),
            reply_with(vec![say("sub")], |m| m.usage = cost(4)),
        ],
        None,
    );
    s.agent
        .run_subagent(SubagentOptions {
            prompt: "x".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    s.agent.wait_for_tasks().await;
    register(
        &s.api,
        vec![reply_with(vec![say("summary")], |m| m.usage = cost(5))],
        None,
    );
    s.agent
        .complete(CompleteOptions {
            system: "s".into(),
            messages: vec![user("sum up")],
            ..Default::default()
        })
        .await
        .unwrap();
    s.agent.wait_for_tasks().await;

    let expected = vec!["discarded:2", "subagent:3", "subagent:4", "side:5"];
    assert_eq!(*hooked.lock().unwrap(), expected);
    let reported: Vec<String> = s
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Usage { usage, source: src, .. } => Some(format!("{}:{}", source(*src), usage.input)),
            _ => None,
        })
        .collect();
    assert_eq!(reported, expected);
    let (_, entries) = SessionFile::open(session.path(), Vec::new()).unwrap();
    let logged: Vec<String> = entries
        .iter()
        .filter_map(|e| match e {
            SessionEntry::Usage(sp) => Some(format!("{}:{}", source(sp.source), sp.usage.input)),
            _ => None,
        })
        .collect();
    assert_eq!(logged, expected);
    // They never enter the conversation.
    assert_eq!(
        restore(&entries).messages.iter().map(|m| m.role()).collect::<Vec<_>>(),
        vec!["user", "assistant"]
    );
}

/// agent.test.ts: "an approval that fails is a refused call; a concurrent call already running is waited for (#41)"
#[tokio::test]
async fn an_approval_that_fails_is_a_refused_call_a_concurrent_call_already_running_is_waited_for_41() {
    // The TS object had no kind; Other is the kind the permission system treats as unknown.
    let slow = ToolDef::new(
        "slow",
        "",
        json!({ "type": "object", "properties": {} }),
        ToolKind::Other,
        |_a, _c| async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(ToolResult::text("slow done"))
        },
    )
    .concurrent(true)
    .into_ref();
    let asked = Arc::new(AtomicUsize::new(0));
    let s = setup(
        vec![
            reply(vec![call("1", "slow", json!({})), call("2", "slow", json!({}))]),
            reply(vec![say("ok")]),
        ],
        |o| {
            o.tools = vec![slow];
            o.permissions = Some(policy(PermissionMode::Ask, json!({})));
            o.approve = Some(Arc::new(move |_r| {
                let n = asked.fetch_add(1, Ordering::SeqCst) + 1;
                Box::pin(async move {
                    if n == 2 {
                        anyhow::bail!("UI bridge died");
                    }
                    Ok(ApprovalAnswer::new(ApprovalDecision::Allow))
                })
            }));
        },
    );
    assert_eq!(s.agent.prompt_text("go").await.unwrap(), StopCause::Done);
    let messages = s.agent.messages();
    let results: Vec<String> = tool_results(&messages).iter().map(|m| json(&m.content)).collect();
    assert_eq!(results.len(), 2);
    assert!(results[0].contains("slow done"), "{results:?}");
    assert!(results[1].contains("UI bridge died"), "{results:?}");
}

/// agent.test.ts: "a tool result with a lone surrogate goes into the history well-formed (#88)"
#[tokio::test]
async fn a_tool_result_with_a_lone_surrogate_goes_into_the_history_well_formed_88() {
    // `"xx😀".slice(0, 3)` cuts the emoji in half. A Rust String cannot hold the lone surrogate that
    // leaves, so the cut is made on UTF-16 units and decoded as JS's toWellFormed() would: the
    // property holds by construction, and the asserts check the text that reaches the model.
    let half = ToolDef::new(
        "half",
        "cuts an emoji in half",
        json!({ "type": "object", "properties": {} }),
        ToolKind::Read,
        |_a, _c| async {
            let units: Vec<u16> = "xx😀".encode_utf16().take(3).collect();
            Ok(ToolResult::text(String::from_utf16_lossy(&units)))
        },
    )
    .into_ref();
    let s = setup(
        vec![reply(vec![call("1", "half", json!({}))]), reply(vec![say("ok")])],
        |o| {
            o.tools = vec![half];
            o.permissions = Some(policy(PermissionMode::Auto, json!({})));
        },
    );
    s.agent.prompt_text("go").await.unwrap();
    let sent = json(last(&s.requests()[1]));
    assert!(!sent.contains("\\ud83d"));
    assert!(sent.contains("xx\u{FFFD}"), "{sent}");
}

/// agent.test.ts: "a refusal's tool calls are not kept: the next request has no call without a result (#97)"
#[tokio::test]
async fn a_refusals_tool_calls_are_not_kept_the_next_request_has_no_call_without_a_result_97() {
    let refusal = reply_with(
        vec![say("I can't"), call("call_1", "read", json!({ "path": "x" }))],
        |m| m.stop_reason = StopReason::Refusal,
    );
    let s = setup(vec![refusal, reply(vec![say("ok")])], |_| {});
    assert_eq!(s.agent.prompt_text("first").await.unwrap(), StopCause::Refusal);
    assert_eq!(s.agent.prompt_text("second").await.unwrap(), StopCause::Done);
    let requests = s.requests();
    let sent = &requests[1].messages;
    let calls: Vec<_> = sent
        .iter()
        .filter_map(Message::as_assistant)
        .flat_map(|a| a.tool_calls())
        .collect();
    assert!(calls.is_empty(), "{calls:?}");
    match &sent[1] {
        Message::Assistant(a) => assert_eq!(a.content, vec![say("I can't")]),
        other => panic!("not an assistant message: {other:?}"),
    }
}

/// agent.test.ts: "an abort that comes in between requests stops the next one: no reply is taken after it (#96)"
#[tokio::test]
async fn an_abort_that_comes_in_between_requests_stops_the_next_one_no_reply_is_taken_after_it_96() {
    let s = setup(vec![reply(vec![say("generated after the abort")])], |_| {});
    let agent = s.agent.clone();
    s.agent.hooks().on::<hook::BeforeRequest, _, _>("test", move |_e, _| {
        agent.abort();
        async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            Ok(None)
        }
    });
    assert_eq!(s.agent.prompt_text("hi").await.unwrap(), StopCause::Aborted);
    assert_eq!(s.requests().len(), 1);
    assert!(!json(&s.agent.messages()).contains("generated after the abort"));
}
