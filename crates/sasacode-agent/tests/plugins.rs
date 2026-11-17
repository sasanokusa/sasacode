//! `packages/agent/test/plugins.test.ts`: hooks, plugin session state, subagents, tool search and
//! the plugin host's registries.

mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::events::AgentEvent;
use sasacode_agent::plugins::{LoadedPlugin, PluginHost, PluginHostOptions};
use sasacode_agent::session::{SessionFile, restore};
use sasacode_agent::tool_search::{TOOL_SEARCH_NAME, ToolSearchConfig, search_tools};
use sasacode_ai::{AssistantContent, AssistantMessage, Message, UserContent, UserMessage};
use sasacode_plugin_api::{
    Command, ContextLimitResult, Decision, Lang, PluginInit, StopCause, SubagentOptions, SystemPromptResult,
    ToolCallResult, ToolDef, ToolKind, ToolRef, ToolResult, ToolResultResult, TurnEndResult, UserPromptResult, hook,
};
use sasacode_testing::ReplayProvider;

use support::*;

/// plugins.test.ts `reply`: provider "test", usage input 100.
fn reply(content: Vec<AssistantContent>) -> AssistantMessage {
    let mut m = support::reply(content, "test");
    m.usage = usage(100, 0);
    m
}

struct Setup {
    agent: Agent,
    host: Arc<PluginHost>,
    provider: Arc<ReplayProvider>,
    events: Arc<Mutex<Vec<AgentEvent>>>,
    dir: tempfile::TempDir,
}

fn host_for(agent: &Agent, dir: &tempfile::TempDir, disabled_tools: Vec<String>) -> Arc<PluginHost> {
    PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd: path_str(dir.path()),
        settings: None,
        register_provider: None,
        disabled_tools,
        lang: Lang::En,
    })
}

/// plugins.test.ts `setup`: system prompt "base", mode auto, the built-in tools, then `plugins`.
async fn setup(
    script: Vec<AssistantMessage>,
    plugins: Vec<(&str, PluginInit)>,
    f: impl FnOnce(&mut AgentOptions),
) -> Setup {
    let dir = temp_dir("sasacode-plugins-");
    let api = api_name();
    let provider = register(&api, script, None);
    let mut o = AgentOptions::new(model(&api, "test", 100_000, 4000), path_str(dir.path()), "base");
    o.permissions = Some(policy(sasacode_plugin_api::PermissionMode::Auto, json!({})));
    f(&mut o);
    let agent = Agent::new(o);
    let host = host_for(&agent, &dir, Vec::new());
    assert!(host.load("builtin-tools", builtin_tools).await);
    for (n, p) in plugins {
        host.load(n, p).await;
    }
    let events = record(&agent);
    Setup {
        agent,
        host,
        provider,
        events,
        dir,
    }
}

fn last_json(r: &sasacode_ai::Request) -> String {
    json(r.messages.last().expect("a message"))
}

/// plugins.test.ts: "hooks transform prompts, system prompt, requests, tool calls and results"
#[tokio::test]
async fn hooks_transform_prompts_system_prompt_requests_tool_calls_and_results() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let sn = seen.clone();
    let p = plug(move |api| {
        api.on::<hook::UserPrompt, _, _>(|ev, _| async move {
            let mut content = ev.content;
            content.push(UserContent::text("(added by plugin)"));
            Ok(Some(UserPromptResult {
                content: Some(content),
                handled: false,
            }))
        });
        api.on::<hook::SystemPrompt, _, _>(|ev, _| async move {
            Ok(Some(SystemPromptResult {
                prompt: Some(format!("{}\nextra instructions", ev.prompt)),
            }))
        });
        api.on::<hook::BeforeRequest, _, _>(move |ev, _| {
            sn.lock().unwrap().push(format!("req:{}", ev.messages.len()));
            async { Ok(None) }
        });
        api.on::<hook::ToolCall, _, _>(|ev, _| async move {
            let mut args = ev.args;
            args.insert("command".into(), json!("echo rewritten"));
            Ok(Some(ToolCallResult {
                args: Some(args),
                ..Default::default()
            }))
        });
        api.on::<hook::ToolResult, _, _>(|ev, _| async move {
            let mut result = ev.result;
            result.content.push(UserContent::text("lint: ok"));
            Ok(Some(ToolResultResult { result: Some(result) }))
        });
    });
    let s = setup(
        vec![
            reply(vec![call("1", "bash", json!({ "command": "echo original" }))]),
            reply(vec![say("ok")]),
        ],
        vec![("p", p)],
        |_| {},
    )
    .await;
    s.agent.prompt_text("go").await.unwrap();
    let requests = s.provider.requests();
    assert_eq!(requests[0].system, "base\nextra instructions");
    assert!(json(&requests[0].messages[0]).contains("(added by plugin)"));
    let result = last_json(&requests[1]);
    assert!(result.contains("rewritten"), "{result}");
    assert!(result.contains("lint: ok"), "{result}");
    assert_eq!(*seen.lock().unwrap(), vec!["req:1", "req:3"]);
}

/// plugins.test.ts: "user_prompt can consume input; tool_call can deny; failing handlers are reported, not fatal"
#[tokio::test]
async fn user_prompt_can_consume_input_tool_call_can_deny_failing_handlers_are_reported_not_fatal() {
    let consume = plug(|api| {
        api.on::<hook::UserPrompt, _, _>(|ev, _| async move {
            Ok(json(&ev.content).contains("/local").then_some(UserPromptResult {
                handled: true,
                content: None,
            }))
        })
    });
    let guard = plug(|api| {
        api.on::<hook::ToolCall, _, _>(|ev, _| async move {
            let rm = ev
                .args
                .get("command")
                .and_then(|c| c.as_str())
                .is_some_and(|c| c.starts_with("rm"));
            Ok(rm.then(|| ToolCallResult {
                decision: Some(Decision::Deny),
                reason: Some("no rm".into()),
                args: None,
            }))
        })
    });
    let broken = plug(|api| api.on::<hook::TurnEnd, _, _>(|_ev, _| async { Err(anyhow::anyhow!("boom")) }));
    let s = setup(
        vec![
            reply(vec![call("1", "bash", json!({ "command": "rm -rf x" }))]),
            reply(vec![say("fine")]),
        ],
        vec![("consume", consume), ("guard", guard), ("broken", broken)],
        |_| {},
    )
    .await;
    assert_eq!(s.agent.prompt_text("/local thing").await.unwrap(), StopCause::Done);
    assert_eq!(s.provider.requests().len(), 0);
    assert_eq!(s.agent.prompt_text("delete").await.unwrap(), StopCause::Done);
    assert!(last_json(&s.provider.requests()[1]).contains("Permission denied (no rm)"));
    assert!(s.events.lock().unwrap().iter().any(|e| matches!(
        e,
        AgentEvent::PluginError { plugin, hook, error }
            if plugin == "broken" && hook == "turn_end" && error == "boom"
    )));
}

/// plugins.test.ts: "turn_end inject continues the loop; context_limit retry after replaceMessages"
#[tokio::test]
async fn turn_end_inject_continues_the_loop_context_limit_retry_after_replace_messages() {
    let injected = Arc::new(AtomicBool::new(false));
    let mut big = reply(vec![say("long")]);
    big.usage = usage(99_000, 0);
    let nudge = plug(move |api| {
        api.on::<hook::TurnEnd, _, _>(move |_ev, _| {
            let first = !injected.swap(true, Ordering::SeqCst);
            async move {
                Ok(first.then(|| TurnEndResult {
                    inject: Some("please also do X".into()),
                    stop: None,
                }))
            }
        })
    });
    let compactor = plug(|api| {
        let a = api.clone();
        api.on::<hook::ContextLimit, _, _>(move |_ev, _| {
            let r = a.session().replace_messages(vec![Message::User(UserMessage {
                content: vec![UserContent::text("summary")],
                timestamp: 0,
            })]);
            async move {
                r?;
                Ok(Some(ContextLimitResult { retry: true }))
            }
        })
    });
    let s = setup(
        vec![reply(vec![say("first")]), big, reply(vec![say("after compaction")])],
        vec![("nudge", nudge), ("compactor", compactor)],
        |_| {},
    )
    .await;
    assert_eq!(s.agent.prompt_text("go").await.unwrap(), StopCause::Done);
    let requests = s.provider.requests();
    assert_eq!(requests.len(), 3);
    assert!(last_json(&requests[1]).contains("please also do X"));
    assert_eq!(requests[2].messages.len(), 1);
    assert!(count(&s.events, "messages_replaced") > 0);
}

/// plugins.test.ts: "plugin session entries and replaced messages survive a resume"
#[tokio::test]
async fn plugin_session_entries_and_replaced_messages_survive_a_resume() {
    let sessions = temp_dir("sasacode-plugins-s-");
    let session = SessionFile::create(sessions.path(), &path_str(sessions.path()), Vec::new()).unwrap();
    let s = setup(vec![reply(vec![say("hi")])], Vec::new(), |o| {
        o.session = Some(session.clone())
    })
    .await;
    let api = s.host.api("mem");
    s.agent.prompt_text("hello").await.unwrap();
    api.session().append("note", json!({ "n": 1 })).unwrap();
    let compacted = Message::User(UserMessage {
        content: vec![UserContent::text("compacted")],
        timestamp: 0,
    });
    api.session().replace_messages(vec![compacted.clone()]).unwrap();
    let (_, entries) = SessionFile::open(session.path(), Vec::new()).unwrap();
    assert_eq!(restore(&entries).messages, vec![compacted]);
    let fresh = host_for(&s.agent, &s.dir, Vec::new());
    fresh.set_session_entries(&entries);
    assert_eq!(fresh.api("mem").session().entries("note"), vec![json!({ "n": 1 })]);
    assert!(fresh.api("other").session().entries("note").is_empty());
}

/// plugins.test.ts: "subagents run with their own history and share tools and tool hooks"
#[tokio::test]
async fn subagents_run_with_their_own_history_and_share_tools_and_tool_hooks() {
    let hook_calls = Arc::new(AtomicUsize::new(0));
    let hc = hook_calls.clone();
    let counter = plug(move |api| {
        api.on::<hook::ToolCall, _, _>(move |_ev, _| {
            hc.fetch_add(1, Ordering::SeqCst);
            async { Ok(None) }
        })
    });
    let spawner = plug(|api| {
        let a = api.clone();
        api.register_tool(
            ToolDef::new(
                "spawn",
                "spawn",
                json!({ "type": "object", "properties": {} }),
                ToolKind::Other,
                move |_args, _ctx| {
                    let a = a.clone();
                    async move {
                        let r = a
                            .agent()
                            .run(SubagentOptions {
                                prompt: "investigate".into(),
                                exclude_tools: vec!["spawn".into()],
                                ..Default::default()
                            })
                            .await?;
                        Ok(ToolResult::text(r.text))
                    }
                },
            )
            .into_ref(),
        );
    });
    let s = setup(
        vec![
            reply(vec![call("1", "spawn", json!({}))]),
            reply(vec![call("s1", "read", json!({ "path": "nothing.txt" }))]), // subagent turn 1
            reply(vec![say("sub report")]),                                    // subagent turn 2
            reply(vec![say("parent done")]),
        ],
        vec![("counter", counter), ("spawner", spawner)],
        |_| {},
    )
    .await;
    s.agent.prompt_text("go").await.unwrap();
    let requests = s.provider.requests();
    // A fresh history.
    assert_eq!(requests[1].messages.len(), 1);
    assert!(!requests[1].tools.iter().any(|t| t.name == "spawn"));
    assert!(last_json(&requests[3]).contains("sub report"));
    assert_eq!(hook_calls.load(Ordering::SeqCst), 2);
}

/// plugins.test.ts: "tool search defers plugin tools past the count threshold and loads them on demand"
#[tokio::test]
async fn tool_search_defers_plugin_tools_past_the_count_threshold_and_loads_them_on_demand() {
    let many = plug(|api| {
        for i in 0..31 {
            let description = if i == 7 {
                "Create a GitHub issue".to_string()
            } else {
                format!("Tool number {i}")
            };
            api.register_tool(
                ToolDef::new(
                    &format!("mcp__srv__tool{i}"),
                    &description,
                    json!({ "type": "object", "properties": {} }),
                    ToolKind::Other,
                    move |_a, _c| async move { Ok(ToolResult::text(format!("ran {i}"))) },
                )
                .into_ref(),
            );
        }
    });
    let s = setup(
        vec![
            reply(vec![call("1", TOOL_SEARCH_NAME, json!({ "query": "github issue" }))]),
            reply(vec![call("2", "mcp__srv__tool7", json!({}))]),
            reply(vec![say("done")]),
        ],
        vec![("many", many)],
        |_| {},
    )
    .await;
    s.agent.prompt_text("file an issue").await.unwrap();
    let requests = s.provider.requests();
    let names0: Vec<&str> = requests[0].tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names0, vec!["read", "write", "edit", "bash", TOOL_SEARCH_NAME]);
    assert!(
        requests[0]
            .tools
            .last()
            .unwrap()
            .description
            .contains("mcp__srv__tool7: Create a GitHub issue")
    );
    assert!(requests[1].tools.iter().any(|t| t.name == "mcp__srv__tool7"));
    assert!(last_json(&requests[2]).contains("ran 7"));

    s.agent.set_tool_search(ToolSearchConfig {
        mode: Some("never".into()),
        ..Default::default()
    });
    assert_eq!(s.agent.request_tools().len(), 35);
    s.agent.set_tool_search(ToolSearchConfig {
        count: Some(100),
        percent: Some(50.0),
        mode: None,
    });
    assert_eq!(s.agent.request_tools().len(), 35);
}

/// plugins.test.ts: "searchTools ranks names over descriptions and supports select:"
#[test]
fn search_tools_ranks_names_over_descriptions_and_supports_select() {
    let t = |name: &str, description: &str| -> ToolRef {
        ToolDef::new(name, description, json!({}), ToolKind::Other, |_a, _c| async {
            Ok(ToolResult::default())
        })
        .into_ref()
    };
    let tools = vec![
        t("slack_send", "send a message"),
        t("gh_issue", "create issue"),
        t("x", "mentions slack"),
    ];
    let names = |found: Vec<ToolRef>| found.iter().map(|x| x.name().to_string()).collect::<Vec<_>>();
    // searchTools' default limit is 5.
    assert_eq!(names(search_tools("slack", &tools, 5)), vec!["slack_send", "x"]);
    assert_eq!(
        names(search_tools("select:gh_issue,x", &tools, 5)),
        vec!["gh_issue", "x"]
    );
}

/// plugins.test.ts: "disabled tools are not registered and registerCommand replaces by name"
#[tokio::test]
async fn disabled_tools_are_not_registered_and_register_command_replaces_by_name() {
    let dir = temp_dir("sasacode-plugins-");
    let agent = Agent::new(AgentOptions::new(
        model(&api_name(), "test", 100_000, 4000),
        path_str(dir.path()),
        "s",
    ));
    let host = host_for(&agent, &dir, vec!["bash".into()]);
    assert!(host.load("builtin-tools", builtin_tools).await);
    assert!(!agent.get_tools().iter().any(|t| t.name() == "bash"));
    host.api("a")
        .register_command(Command::new("x", "a", |_| async { Ok(()) }));
    host.api("b")
        .register_command(Command::new("x", "b", |_| async { Ok(()) }));
    assert_eq!(
        host.commands()
            .iter()
            .map(|c| format!("{}:{}", c.command.name, c.owner))
            .collect::<Vec<_>>(),
        vec!["x:b"]
    );
    let bad: PluginInit = Box::new(|_api| Box::pin(async { Err(anyhow::anyhow!("nope")) }));
    assert!(!host.load("bad", bad).await);
    assert_eq!(
        host.plugins().last(),
        Some(&LoadedPlugin {
            name: "bad".into(),
            error: Some("nope".into())
        })
    );
}
