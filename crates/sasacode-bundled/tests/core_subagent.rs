//! packages/bundled/test/subagent.test.ts, and the subagent test of bundled.test.ts.

mod support_core;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_ai::{AssistantMessage, Provider, Request, StreamEvent, register_api};
use sasacode_bundled::subagent::report_of;
use sasacode_plugin_api::{
    Lang, PermissionMode, SessionEndEvent, SessionEndReason, SessionStartEvent, StopCause, hook,
};
use sasacode_testing::{ReplayOptions, ReplayProvider};
use support_core::*;

/// JSON of the last message of the last request.
fn last_text(requests: &[Request]) -> String {
    requests
        .last()
        .and_then(|r| r.messages.last())
        .map(json_of)
        .unwrap_or_default()
}

/// A provider that holds every stream until `gate` is cancelled (released).
struct Gated {
    api: String,
    inner: Arc<ReplayProvider>,
    gate: CancellationToken,
}

#[async_trait]
impl Provider for Gated {
    fn api(&self) -> &str {
        &self.api
    }
    fn stream(&self, req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        let (inner, gate) = (self.inner.clone(), self.gate.clone());
        futures::stream::once(async move {
            gate.cancelled().await;
            inner.stream(req)
        })
        .flatten()
        .boxed()
    }
}

/// The caller's provider, its second request held until that request is aborted (Esc).
struct HoldSecond {
    api: String,
    inner: Arc<ReplayProvider>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for HoldSecond {
    fn api(&self) -> &str {
        &self.api
    }
    fn stream(&self, req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let inner = self.inner.clone();
        futures::stream::once(async move {
            if n == 2 {
                req.signal.cancelled().await;
            }
            inner.stream(req)
        })
        .flatten()
        .boxed()
    }
}

struct Setup {
    agent: Agent,
    main: Arc<ReplayProvider>,
    notices: Arc<Mutex<Vec<String>>>,
    _host: Arc<PluginHost>,
    _dir: tempfile::TempDir,
}

/// The caller runs on api "main-<n>", subagents (any model) on "sub-<n>", so each has its own script.
/// `main_provider`: what to register for the caller instead of the plain replay of `main`.
async fn setup_with(
    main: Vec<AssistantMessage>,
    sub: Arc<dyn Provider>,
    main_provider: impl FnOnce(&str, Arc<ReplayProvider>) -> Arc<dyn Provider>,
) -> Setup {
    let (main_api, sub_api) = (api_name(), api_name());
    let replay = Arc::new(ReplayProvider::new(main, ReplayOptions::api(&main_api)));
    register_api(&main_api, main_provider(&main_api, replay.clone()));
    register_api(&sub_api, sub);
    let dir = temp_dir("sasacode-subagent-");
    let cwd = path_str(dir.path());
    let mut o = AgentOptions::new(model(&main_api, 100_000, 1000), cwd.clone(), "base");
    o.permissions = Some(policy(PermissionMode::Edits));
    let resolved = model(&sub_api, 100_000, 1000);
    o.resolve_model = Some(Arc::new(move |_| Ok(resolved.clone())));
    let agent = Agent::new(o);
    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd,
        settings: Some(Arc::new(|_| json!({}))),
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: Lang::Ja,
    });
    let ui = NoticeUi::default();
    let notices = ui.notices.clone();
    host.set_ui(Some(Arc::new(ui)));
    assert!(host.load("builtin-tools", builtin_tools).await);
    assert!(host.load("subagent", bundled("subagent")).await);
    Setup {
        agent,
        main: replay,
        notices,
        _host: host,
        _dir: dir,
    }
}

async fn setup(main: Vec<AssistantMessage>, sub: Arc<dyn Provider>) -> Setup {
    setup_with(main, sub, |_, r| r).await
}

fn replay(script: Vec<AssistantMessage>) -> Arc<ReplayProvider> {
    let api = api_name();
    Arc::new(ReplayProvider::new(script, ReplayOptions::api(&api)))
}

fn gated(inner: Arc<ReplayProvider>, gate: &CancellationToken) -> Arc<dyn Provider> {
    Arc::new(Gated {
        api: "gated".into(),
        inner,
        gate: gate.clone(),
    })
}

async fn until(what: &str, cond: impl Fn() -> bool) {
    for _ in 0..200 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(cond(), "timed out waiting for {what}");
}

fn notices(s: &Setup) -> Vec<String> {
    s.notices.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[tokio::test]
async fn a_subagent_that_fails_tells_the_caller_why_and_what_it_had_done() {
    // Then the script runs out: an error.
    let sub = replay(vec![reply(vec![
        say("checking"),
        call("s1", "read", json!({ "path": "nope.txt" })),
    ])]);
    let s = setup(
        vec![
            reply(vec![call(
                "1",
                "task",
                json!({ "description": "look", "prompt": "look", "model": "t/sub" }),
            )]),
            reply(vec![say("ok")]),
        ],
        sub,
    )
    .await;
    let _ = s.agent.prompt_text("go").await;
    let result = last_text(&s.main.requests());
    assert!(
        result.contains("subagent did not finish: error: replay: no more recorded responses"),
        "{result}"
    );
    assert!(result.contains("checking"), "{result}");
    assert!(result.contains("read("), "{result}");
}

#[test]
fn report_of_a_finished_run_is_just_its_report() {
    assert_eq!(
        report_of("found it", StopCause::Done, None, &["read(a)".to_string()]),
        "found it"
    );
    assert!(report_of("", StopCause::MaxTurns, None, &[]).contains("did not finish: max_turns"));
}

#[tokio::test]
async fn background_the_caller_keeps_going_and_the_report_arrives_as_a_message() {
    let sub = replay(vec![reply(vec![say("found 3 files")])]);
    let s = setup(
        vec![
            reply(vec![call(
                "1",
                "task",
                json!({ "description": "look", "prompt": "look", "model": "t/sub", "background": true }),
            )]),
            reply(vec![say("meanwhile")]),
            reply(vec![say("thanks")]),
        ],
        sub,
    )
    .await;
    let _ = s.agent.prompt_text("go").await;
    until("3 requests", || s.main.requests().len() == 3).await;
    s.agent.wait_for_idle().await;
    let requests = s.main.requests();
    assert!(json_of(&requests[1].messages).contains("Started t1 in the background"));
    let last = last_text(&requests);
    assert!(last.contains(r#"[task t1 \"look\" ended: done]"#), "{last}");
    assert!(last.contains("found 3 files"), "{last}");
    assert!(notices(&s).iter().any(|m| m.contains("t1")));
}

#[tokio::test]
async fn task_send_reaches_a_running_background_subagent_at_its_next_turn() {
    let gate = CancellationToken::new();
    let inner = replay(vec![
        reply(vec![call("s1", "task_status", json!({}))]),
        reply(vec![say("changed course")]),
    ]);
    let s = setup(
        vec![
            reply(vec![call(
                "1",
                "task",
                json!({ "description": "look", "prompt": "look", "model": "t/sub", "background": true }),
            )]),
            reply(vec![call(
                "2",
                "task_send",
                json!({ "id": "t1", "message": "look in src instead" }),
            )]),
            reply(vec![say("waiting")]),
            reply(vec![say("done")]),
        ],
        gated(inner.clone(), &gate),
    )
    .await;
    let _ = s.agent.prompt_text("go").await;
    gate.cancel();
    until("4 requests", || s.main.requests().len() == 4).await;
    assert!(json_of(&inner.requests()[1].messages).contains("look in src instead"));
    let last = last_text(&s.main.requests());
    assert!(last.contains("changed course"), "{last}");
}

#[tokio::test]
async fn esc_on_the_caller_stops_its_background_runs_and_the_report_waits_for_the_next_message() {
    let gate = CancellationToken::new();
    let inner = replay(vec![reply(vec![say("never reached")])]);
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    // The caller's second request is held until Esc.
    let s = setup_with(
        vec![
            reply(vec![call(
                "1",
                "task",
                json!({ "description": "look", "prompt": "look", "model": "t/sub", "background": true }),
            )]),
            reply(vec![say("carry on")]),
        ],
        gated(inner, &gate),
        move |api, r| {
            Arc::new(HoldSecond {
                api: api.to_string(),
                inner: r,
                calls: c,
            })
        },
    )
    .await;
    let agent = s.agent.clone();
    let run = tokio::spawn(async move { agent.prompt_text("go").await });
    until("the second request", || calls.load(Ordering::SeqCst) == 2).await;
    s.agent.abort();
    assert_eq!(run.await.unwrap().unwrap(), StopCause::Aborted);
    gate.cancel();
    until("the stopped notice", || {
        notices(&s).iter().any(|m| m.contains("t1") && m.contains("stopped"))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!s.agent.is_running());
    // No third request: the report waits for the user's next message.
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(s.main.requests().len() <= 2);
}

#[tokio::test]
async fn a_session_switch_stops_background_runs_and_their_reports_are_not_injected_into_the_new_session_122() {
    let gate = CancellationToken::new();
    let inner = replay(vec![reply(vec![say("AUDIT REPORT of the old conversation")])]);
    let s = setup(
        vec![
            reply(vec![call(
                "1",
                "task",
                json!({ "description": "audit", "prompt": "audit", "model": "t/sub", "background": true }),
            )]),
            reply(vec![say("waiting")]),
        ],
        gated(inner, &gate),
    )
    .await;
    assert_eq!(s.agent.prompt_text("go").await.unwrap(), StopCause::Done);
    s.agent
        .hooks()
        .emit::<hook::SessionEnd>(SessionEndEvent {
            session_id: None,
            reason: Some(SessionEndReason::Switch),
        })
        .await;
    s.agent.reset(Vec::new());
    s.agent
        .hooks()
        .emit::<hook::SessionStart>(SessionStartEvent {
            session_id: None,
            resumed: false,
        })
        .await;
    gate.cancel();
    until("the t1 notice", || notices(&s).iter().any(|m| m.contains("t1"))).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(s.agent.messages().is_empty());
    assert_eq!(s.main.requests().len(), 2);
}

/// bundled.test.ts: "subagent: task runs a fresh loop and returns its report; no approval needed to start".
#[tokio::test]
async fn subagent_task_runs_a_fresh_loop_and_returns_its_report_no_approval_needed_to_start() {
    let s = support_core::setup(
        vec![
            reply(vec![call(
                "1",
                "task",
                json!({ "description": "look around", "prompt": "list files" }),
            )]),
            reply(vec![say("found 3 files")]),
            reply(vec![say("thanks")]),
        ],
        &["subagent"],
        SetupOpts::default(),
    )
    .await;
    let _ = s.agent.prompt_text("delegate").await;
    let requests = s.requests();
    assert_eq!(requests[1].messages.len(), 1);
    assert!(!requests[1].tools.iter().any(|t| t.name == "task"));
    let last = requests[2].messages.last().map(json_of).unwrap_or_default();
    assert!(last.contains("found 3 files"), "{last}");
}
