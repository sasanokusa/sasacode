//! `packages/agent/test/session-switch.test.ts`: /clear, /resume and /fork keep the Agent and swap
//! its conversation: nothing of the old one may leak in.

mod support;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::task::JoinHandle;

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::events::AgentEvent;
use sasacode_ai::{AssistantContent, AssistantMessage, StopReason};
use sasacode_plugin_api::{PermissionMode, StopCause, hook};
use sasacode_testing::ReplayProvider;

use support::*;

/// session-switch.test.ts `reply`: provider "test", usage 100/10, stopReason "stop" even for a tool call.
fn reply(content: Vec<AssistantContent>) -> AssistantMessage {
    let mut m = support::reply(content, "test");
    m.usage = usage(100, 10);
    m.stop_reason = StopReason::Stop;
    m
}

/// session-switch.test.ts `setup`: a replay api of its own streaming 20 characters at a time.
fn setup(
    script: Vec<AssistantMessage>,
    f: impl FnOnce(&mut AgentOptions),
) -> (Arc<ReplayProvider>, Agent, tempfile::TempDir) {
    let dir = temp_dir("sasacode-switch-");
    let api = api_name();
    let provider = register(&api, script, Some(20));
    let mut o = AgentOptions::new(model(&api, "test", 100_000, 4000), path_str(dir.path()), "sys");
    f(&mut o);
    (provider, Agent::new(o), dir)
}

type Handle = Arc<Mutex<Option<JoinHandle<()>>>>;

/// session-switch.test.ts: "the interrupted note stays with its conversation: a reset one starts without it"
#[tokio::test]
async fn the_interrupted_note_stays_with_its_conversation_a_reset_one_starts_without_it() {
    let (provider, agent, _dir) = setup(
        vec![reply(vec![say(&"x".repeat(2000))]), reply(vec![say("fresh")])],
        |_| {},
    );
    let a = agent.clone();
    let off = agent.events().on(move |e| {
        if matches!(e, AgentEvent::MessageUpdate { .. }) {
            a.abort();
        }
    });
    assert_eq!(agent.prompt_text("first").await.unwrap(), StopCause::Aborted);
    off.off();
    agent.reset(Vec::new());
    agent.prompt_text("second, in a new session").await.unwrap();
    assert!(!json(&provider.requests()[1].messages).contains("interrupted the previous response"));
}

/// session-switch.test.ts: "halt drops queued input: it is not run on the conversation being left"
#[tokio::test]
async fn halt_drops_queued_input_it_is_not_run_on_the_conversation_being_left() {
    let (provider, agent, _dir) = setup(
        vec![
            reply(vec![say(&"x".repeat(2000))]),
            reply(vec![say("answer to the queued one")]),
        ],
        |_| {},
    );
    // In the TS, prompt() suspended at `await this.hooks.run("user_prompt")` even with no handler,
    // so halt() cleared the queue first and the input was queued after it, behind the run being
    // stopped: what kept it from running was the `!this.halted` check when that run ended. A
    // handler that yields once puts the same await here.
    agent.hooks().on::<hook::UserPrompt, _, _>("test", |_e, _| async {
        tokio::task::yield_now().await;
        Ok(None)
    });
    let halted: Handle = Arc::default();
    let queued: Handle = Arc::default();
    let polls: Arc<Mutex<Vec<bool>>> = Arc::default();
    let once = Arc::new(AtomicBool::new(false));
    let (a, h, q, pl) = (agent.clone(), halted.clone(), queued.clone(), polls.clone());
    let off = agent.events().on(move |e| {
        if !matches!(e, AgentEvent::MessageUpdate { .. }) || once.swap(true, Ordering::SeqCst) {
            return;
        }
        // `void agent.prompt("queued while running")`: it runs up to the user_prompt await.
        let a1 = a.clone();
        let mut prompt: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
            let _ = a1.prompt_text("queued while running").await;
        });
        pl.lock().unwrap().push(poll_once(&mut prompt).is_pending());
        // What /clear does mid-reply: `halted = agent.halt()`.
        let a2 = a.clone();
        *h.lock().unwrap() = Some(start(async move { a2.halt().await }));
        // The prompt's microtask runs before the stream goes on: it queues behind the stopping run.
        let pending = poll_once(&mut prompt).is_pending();
        pl.lock().unwrap().push(pending);
        *q.lock().unwrap() = Some(if pending {
            tokio::spawn(prompt)
        } else {
            tokio::spawn(async {})
        });
    });
    assert_eq!(agent.prompt_text("first").await.unwrap(), StopCause::Aborted);
    let halt = halted.lock().unwrap().take().expect("halt was called");
    halt.await.unwrap();
    // The queued prompt waited on the run that was halted; it is over too.
    let queued_prompt = queued.lock().unwrap().take().expect("the prompt was sent");
    queued_prompt.await.unwrap();
    off.off();
    // Suspended in the hook, then waiting on the run it was queued for.
    assert_eq!(*polls.lock().unwrap(), vec![true, true]);
    assert_eq!(provider.requests().len(), 1);
    agent.reset(Vec::new());
    agent.prompt_text("in the new session").await.unwrap();
    assert_eq!(provider.requests().len(), 2);
    assert!(!json(&provider.requests()[1].messages).contains("queued while running"));
}

/// session-switch.test.ts: "input sent while a halt waits for the run to stop is not lost: it goes to the conversation after the halt"
#[tokio::test]
async fn input_sent_while_a_halt_waits_for_the_run_to_stop_is_not_lost_it_goes_to_the_conversation_after_the_halt() {
    let (provider, agent, _dir) = setup(
        vec![
            reply(vec![call("1", "bash", json!({ "command": "trap '' TERM; sleep 5" }))]),
            reply(vec![say("got it")]),
        ],
        |o| {
            o.tools = vec![Arc::new(sasacode_tools::BashTool)];
            o.permissions = Some(policy(PermissionMode::Auto, json!({})));
        },
    );
    let halted: Handle = Arc::default();
    let next: Handle = Arc::default();
    let (a, h, n) = (agent.clone(), halted.clone(), next.clone());
    let off = agent.events().on(move |e| {
        if !matches!(e, AgentEvent::ToolStart { .. }) {
            return;
        }
        // Once the command runs (it ignores SIGTERM, so stopping it takes a while), halt; then the user types on.
        let (a, h, n) = (a.clone(), h.clone(), n.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let a1 = a.clone();
            *h.lock().unwrap() = Some(start(async move { a1.halt().await }));
            tokio::time::sleep(Duration::from_millis(5)).await;
            *n.lock().unwrap() = Some(start(async move {
                let _ = a.prompt_text("next instruction").await;
            }));
        });
    });
    agent.prompt_text("run it").await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let halt = halted.lock().unwrap().take();
    if let Some(h) = halt {
        h.await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    let following = next.lock().unwrap().take();
    if let Some(n) = following {
        n.await.unwrap();
    }
    agent.wait_for_idle().await;
    off.off();
    assert_eq!(provider.requests().len(), 2);
    assert!(json(&provider.requests()[1].messages).contains("next instruction"));
}
