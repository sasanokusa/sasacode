//! packages/bundled/test/loop-guard.test.ts.

mod support_core;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use serde_json::{Value, json};

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_ai::{AssistantContent, AssistantMessage, Message, ToolCall, register_api};
use sasacode_plugin_api::{
    AssistantMessageEvent, HookContext, Lang, PermissionMode, PluginApi, StopCause, ToolCallEvent, ToolDef, ToolKind,
    ToolResult, ToolResultEvent, hook,
};
use sasacode_testing::{ReplayOptions, ReplayProvider};
use support_core::*;

/// A response with one tool call.
fn one(id: &str, name: &str, input: Value) -> AssistantMessage {
    reply_at(vec![call(id, name, input)], 0)
}

fn done() -> AssistantMessage {
    reply_at(vec![say("done")], 0)
}

fn tools(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let n = Arc::new(AtomicUsize::new(0));
        let number = json!({ "type": "object", "properties": { "x": { "type": "number" } } });
        let none = json!({ "type": "object", "properties": {} });
        api.register_tool(
            ToolDef::new("check", "", number, ToolKind::Read, |_a, _c| async {
                Ok(ToolResult::text("same output"))
            })
            .into_ref(),
        );
        api.register_tool(
            ToolDef::new("noop", "", none.clone(), ToolKind::Edit, |_a, _c| async {
                Ok(ToolResult::text("unchanged").with_details(obj(json!({ "changed": false }))))
            })
            .paths(|_, _| Vec::new())
            .into_ref(),
        );
        api.register_tool(
            ToolDef::new("failed", "", none.clone(), ToolKind::Edit, |_a, _c| async {
                let mut r = ToolResult::text("failed").with_details(obj(json!({ "changed": true })));
                r.is_error = Some(true);
                Ok(r)
            })
            .paths(|_, _| Vec::new())
            .into_ref(),
        );
        api.register_tool(
            ToolDef::new("fix", "", none, ToolKind::Edit, move |_a, _c| {
                let k = n.fetch_add(1, Ordering::SeqCst) + 1;
                async move { Ok(ToolResult::text(format!("changed {k}"))) }
            })
            .paths(|_, _| Vec::new())
            .into_ref(),
        );
        Ok(())
    })
}

fn agent_on(api: &str, cwd: &str) -> (Agent, Arc<PluginHost>) {
    let mut o = AgentOptions::new(model(api, 100_000, 1000), cwd, "");
    o.permissions = Some(policy(PermissionMode::Auto));
    let agent = Agent::new(o);
    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd: cwd.into(),
        settings: None,
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: Lang::Ja,
    });
    (agent, host)
}

fn tool_results(agent: &Agent) -> Vec<String> {
    agent
        .messages()
        .iter()
        .filter_map(|m| match m {
            Message::Tool(t) => Some(serde_json::to_string(&t.content).unwrap_or_default()),
            _ => None,
        })
        .collect()
}

/// The TS `run()`: the four tools, loop-guard, "go"; the tool results and how the run ended.
async fn run_guard(script: Vec<AssistantMessage>) -> (Vec<String>, StopCause) {
    let api = api_name();
    register_api(&api, Arc::new(ReplayProvider::new(script, ReplayOptions::api(&api))));
    let dir = temp_dir("sasacode-loop-");
    let (agent, host) = agent_on(&api, &path_str(dir.path()));
    assert!(host.load("tools", tools).await);
    assert!(host.load("loop-guard", bundled("loop-guard")).await);
    let cause = agent.prompt_text("go").await.unwrap_or(StopCause::Error);
    (tool_results(&agent), cause)
}

fn repeat(n: usize, f: impl Fn(usize) -> AssistantMessage) -> Vec<AssistantMessage> {
    (0..n).map(f).collect()
}

#[tokio::test]
async fn the_same_call_with_the_same_result_noted_on_the_3rd_blocked_on_the_5th() {
    let mut script = repeat(5, |i| one(&i.to_string(), "check", json!({ "x": 1 })));
    script.push(done());
    let (results, _) = run_guard(script).await;
    assert!(!results[1].contains("[harness]"));
    assert!(
        results[2].contains("call 3 of check with the same arguments"),
        "{}",
        results[2]
    );
    assert!(
        results[4].contains("loop-guard: the same check call returned the same result 4 times"),
        "{}",
        results[4]
    );
}

#[tokio::test]
async fn a_file_change_in_between_resets_the_count_different_arguments_are_not_a_loop() {
    let (results, _) = run_guard(vec![
        one("1", "check", json!({ "x": 1 })),
        one("2", "check", json!({ "x": 1 })),
        one("3", "fix", json!({})),
        one("4", "check", json!({ "x": 1 })),
        one("5", "check", json!({ "x": 2 })),
        done(),
    ])
    .await;
    let all = results.join(",");
    assert!(!all.contains("[harness]"), "{all}");
    assert!(!all.contains("loop-guard"), "{all}");
}

#[tokio::test]
async fn the_same_call_twice_in_one_response_runs_once() {
    let same = |id: &str| call(id, "check", json!({ "x": 1 }));
    let (results, _) = run_guard(vec![reply_at(vec![same("1"), same("2"), same("3")], 0), done()]).await;
    assert!(results[0].contains("same output"));
    assert!(
        results[1].contains("appears earlier in this response"),
        "{}",
        results[1]
    );
    assert!(
        results[2].contains("appears earlier in this response"),
        "{}",
        results[2]
    );
}

#[tokio::test]
async fn a_short_cycle_a_b_a_b_is_noted_and_then_blocked() {
    let mut script = repeat(10, |i| {
        one(&i.to_string(), "check", json!({ "x": if i == 9 { 0 } else { i % 2 } }))
    });
    script.push(done());
    let (results, _) = run_guard(script).await;
    assert!(
        results[5].contains("The same 2 tool calls have now repeated 3 times"),
        "{}",
        results[5]
    );
    assert!(
        results[8].contains("loop-guard: the same 2 calls have repeated 4 times"),
        "{}",
        results[8]
    );
    // A refusal does not reset the count: asking for the refused call again is refused again.
    assert!(results[9].contains("loop-guard"), "{}", results[9]);
}

#[tokio::test]
async fn a_call_rejected_before_running_invalid_arguments_counts_as_a_repeated_error() {
    let mut script = repeat(3, |i| one(&i.to_string(), "check", json!({ "x": "nope" })));
    script.push(done());
    let (results, _) = run_guard(script).await;
    assert!(results[0].contains("Invalid arguments"), "{}", results[0]);
    assert!(
        results[2].contains("[harness] This is call 3 of check"),
        "{}",
        results[2]
    );
}

#[tokio::test]
async fn repeating_a_check_after_a_change_in_the_same_response_is_allowed() {
    let (results, _) = run_guard(vec![
        reply_at(
            vec![
                call("1", "check", json!({ "x": 1 })),
                call("2", "fix", json!({})),
                call("3", "check", json!({ "x": 1 })),
            ],
            0,
        ),
        done(),
    ])
    .await;
    assert!(!results.join(",").contains("loop-guard"));
}

#[tokio::test]
async fn once_a_cycle_is_stopped_all_of_its_calls_stay_refused_until_a_file_changes() {
    // A B A B A B A B, then the model keeps alternating: every later step is refused, not just every other one.
    let mut script = repeat(12, |i| one(&i.to_string(), "check", json!({ "x": i % 2 })));
    script.extend([one("f", "fix", json!({})), one("a", "check", json!({ "x": 0 })), done()]);
    let (results, _) = run_guard(script).await;
    for r in &results[8..12] {
        assert!(r.contains("loop-guard"), "{r}");
    }
    assert!(results[13].contains("same output"), "{}", results[13]); // a file changed: the calls are allowed again
}

#[tokio::test]
async fn a_run_in_which_no_tool_call_can_run_for_5_turns_is_stopped_by_the_plugin() {
    let (results, cause) = run_guard(repeat(20, |i| one(&i.to_string(), "no_such_tool", json!({})))).await;
    assert_eq!(cause, StopCause::Stopped);
    assert_eq!(results.len(), 5);
}

#[tokio::test]
async fn parallel_subagents_keep_separate_state_the_same_read_in_each_is_not_a_repeat() {
    let task = |i: usize| {
        call(
            &format!("t{i}"),
            "task",
            json!({ "description": format!("part {i}"), "prompt": format!("look {i}") }),
        )
    };
    let mut script = vec![reply_at((1..=5).map(task).collect(), 0)];
    script.extend((1..=5).map(|i| one(&format!("c{i}"), "check", json!({ "x": 1 }))));
    script.extend((1..=5).map(|i| reply_at(vec![say(&format!("report {i}"))], 0)));
    script.push(done());
    let api = api_name();
    register_api(&api, Arc::new(ReplayProvider::new(script, ReplayOptions::api(&api))));
    let dir = temp_dir("sasacode-loop-");
    let (agent, host) = agent_on(&api, &path_str(dir.path()));
    let refused: Arc<Mutex<Vec<String>>> = Arc::default();
    // The TS subagents interleaved at every await, so each took one check from the shared script
    // before any asked again. Here a replayed response is ready at once and a subagent could run
    // ahead and take all five; the checks wait for each other (at most 2s) to keep them parallel.
    let all_started = Arc::new(tokio::sync::Barrier::new(5));
    assert!(
        host.load(
            "tools",
            move |api: PluginApi| -> BoxFuture<'static, anyhow::Result<()>> {
                Box::pin(async move {
                    api.register_tool(
                        ToolDef::new(
                            "check",
                            "",
                            json!({ "type": "object", "properties": { "x": { "type": "number" } } }),
                            ToolKind::Read,
                            move |_a, _c| {
                                let all_started = all_started.clone();
                                async move {
                                    let _ = tokio::time::timeout(Duration::from_secs(2), all_started.wait()).await;
                                    Ok(ToolResult::text("same output"))
                                }
                            },
                        )
                        .into_ref(),
                    );
                    Ok(())
                })
            }
        )
        .await
    );
    assert!(host.load("subagent", bundled("subagent")).await);
    assert!(host.load("loop-guard", bundled("loop-guard")).await);
    let log = refused.clone();
    assert!(
        host.load("log", move |api: PluginApi| -> BoxFuture<'static, anyhow::Result<()>> {
            Box::pin(async move {
                api.on::<hook::ToolResult, _, _>(move |ev, _c| {
                    if ev.call.name == "check" && !ev.ran {
                        log.lock().unwrap_or_else(|e| e.into_inner()).push(ev.call.id.clone());
                    }
                    async { Ok(None) }
                });
                Ok(())
            })
        })
        .await
    );
    let _ = agent.prompt_text("go").await;
    assert_eq!(*refused.lock().unwrap(), Vec::<String>::new());
}

#[tokio::test]
async fn successful_no_op_writes_and_failed_edits_are_not_progress() {
    for name in ["noop", "failed"] {
        let mut script = repeat(5, |i| one(&i.to_string(), name, json!({})));
        script.push(done());
        let (results, _) = run_guard(script).await;
        assert!(results[2].contains("[harness]"), "{name}: {}", results[2]);
        assert!(results[4].contains("loop-guard"), "{name}: {}", results[4]);
    }
}

#[tokio::test]
async fn read_then_no_op_edit_then_same_read_in_one_response_remains_a_duplicate() {
    let (results, _) = run_guard(vec![
        reply_at(
            vec![
                call("r1", "check", json!({ "x": 1 })),
                call("w", "noop", json!({})),
                call("r2", "check", json!({ "x": 1 })),
            ],
            0,
        ),
        done(),
    ])
    .await;
    assert!(
        results[2].contains("appears earlier in this response"),
        "{}",
        results[2]
    );
}

#[tokio::test]
async fn a_change_by_another_agent_clears_both_locked_calls_and_their_stale_history() {
    let dir = temp_dir("sasacode-loop-");
    // The api is never called: the hooks are run by hand.
    let (agent, host) = agent_on("replay", &path_str(dir.path()));
    assert!(
        host.load("tools", |api: PluginApi| -> BoxFuture<'static, anyhow::Result<()>> {
            Box::pin(async move {
                api.register_tool(
                    ToolDef::new("check", "", json!({}), ToolKind::Read, |_a, _c| async {
                        Ok(ToolResult {
                            content: Vec::new(),
                            is_error: None,
                            details: None,
                        })
                    })
                    .into_ref(),
                );
                Ok(())
            })
        })
        .await
    );
    assert!(host.load("loop-guard", bundled("loop-guard")).await);
    let read = ToolCall {
        id: "read".into(),
        name: "check".into(),
        input: obj(json!({})),
        raw_input: None,
    };
    let message = || AssistantMessageEvent {
        message: reply_at(Vec::<AssistantContent>::new(), 0),
        stopped: None,
    };
    for _ in 0..4 {
        agent.hooks().emit::<hook::AssistantMessage>(message()).await;
        agent
            .hooks()
            .emit::<hook::ToolResult>(ToolResultEvent {
                call: read.clone(),
                ran: true,
                result: ToolResult::text("same"),
            })
            .await;
    }
    let tool = agent.get_tools().into_iter().find(|t| t.name() == "check").unwrap();
    let verdict = || {
        let (agent, read, tool) = (agent.clone(), read.clone(), tool.clone());
        async move {
            agent.hooks().emit::<hook::AssistantMessage>(message()).await;
            let mut decision = None;
            agent
                .hooks()
                .run::<hook::ToolCall>(
                    ToolCallEvent {
                        call: read,
                        tool,
                        args: obj(json!({})),
                    },
                    |r, _e, _o| {
                        decision = r.decision;
                        true
                    },
                )
                .await;
            decision
        }
    };
    assert_eq!(verdict().await, Some(sasacode_plugin_api::Decision::Deny));
    let changed = ToolCall {
        id: "changed".into(),
        name: "write".into(),
        ..read.clone()
    };
    agent
        .hooks()
        .without(
            &[],
            HookContext {
                agent: Some("worker".into()),
            },
        )
        .emit::<hook::ToolResult>(ToolResultEvent {
            call: changed,
            ran: true,
            result: ToolResult {
                content: Vec::new(),
                is_error: None,
                details: Some(obj(json!({ "changed": true }))),
            },
        })
        .await;
    assert_eq!(verdict().await, None);
}
