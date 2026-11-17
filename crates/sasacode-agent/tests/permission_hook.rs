//! `packages/agent/test/permission-hook.test.ts`: what a `permission` hook sees and how far it can
//! move a verdict. Also the permission example of the tool-authoring skill
//! (`packages/skills/test/authoring-doc.test.ts`).

mod support;

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use sasacode_agent::agent::{Agent, AgentOptions, ApprovalAnswer, ApprovalDecision};
use sasacode_agent::permission::PermissionCheck;
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_ai::{AssistantContent, AssistantMessage, Message, Request};
use sasacode_plugin_api::{
    Decision, Lang, PermissionEvent, PermissionMode, PermissionResult, PermissionRules, PluginInit, ToolCallResult,
    VerdictSource, hook,
};
use sasacode_tools::BashTool;

use support::*;

fn bash(command: &str) -> AssistantMessage {
    reply(vec![call("1", "bash", json!({ "command": command }))], "t")
}
fn done() -> AssistantMessage {
    reply(vec![say("done")], "t")
}

type Decide = Arc<dyn Fn(&PermissionEvent) -> Option<PermissionResult> + Send + Sync>;

#[derive(Default)]
struct Opts {
    mode: Option<PermissionMode>,
    rules: Option<Value>,
    decide: Option<Decide>,
    /// None: no approver (headless); Some(answer): the user allows or denies.
    approve: Option<bool>,
}

fn decide(decision: Decision, reason: Option<&str>) -> Option<Decide> {
    let reason = reason.map(str::to_string);
    Some(Arc::new(move |_| {
        Some(PermissionResult {
            decision: Some(decision),
            reason: reason.clone(),
        })
    }))
}

struct Ran {
    seen: Vec<PermissionEvent>,
    /// The reason each approval request carried.
    asked: Vec<String>,
    /// `JSON.stringify` of the first tool result's content.
    result: String,
    requests: usize,
}

struct Built {
    agent: Agent,
    host: Arc<PluginHost>,
    provider: Arc<sasacode_testing::ReplayProvider>,
    _dir: tempfile::TempDir,
}

/// An agent on its own replay api with the built-in tools loaded through the host.
async fn build(script: Vec<AssistantMessage>, f: impl FnOnce(&mut AgentOptions)) -> Built {
    let dir = temp_dir("sasacode-permission-hook-");
    let api = api_name();
    let provider = register(&api, script, None);
    let mut o = AgentOptions::new(model(&api, "t", 100_000, 4000), path_str(dir.path()), "s");
    f(&mut o);
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
    Built {
        agent,
        host,
        provider,
        _dir: dir,
    }
}

fn first_tool_result(messages: &[Message]) -> String {
    messages
        .iter()
        .find_map(|m| match m {
            Message::Tool(t) => Some(json(&t.content)),
            _ => None,
        })
        .unwrap_or_default()
}

/// Runs one bash call with a permission plugin that answers `decide`, and says what happened.
async fn run(command: &str, opts: Opts) -> Ran {
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let rules = opts.rules.clone().unwrap_or_else(|| json!({}));
    let mode = opts.mode.unwrap_or(PermissionMode::Auto);
    let a = asked.clone();
    let b = build(vec![bash(command), done()], move |o| {
        o.permissions = Some(policy(mode, rules));
        if let Some(yes) = opts.approve {
            o.approve = Some(Arc::new(move |r| {
                a.lock().unwrap().push(r.reason.clone());
                Box::pin(async move {
                    Ok(ApprovalAnswer::new(if yes {
                        ApprovalDecision::Allow
                    } else {
                        ApprovalDecision::Deny
                    }))
                })
            }));
        }
    })
    .await;
    let seen: Arc<Mutex<Vec<PermissionEvent>>> = Arc::default();
    let (sn, d) = (seen.clone(), opts.decide.clone());
    let plugin = plug(move |api| {
        api.on::<hook::Permission, _, _>(move |ev, _| {
            sn.lock().unwrap().push(ev.clone());
            let r = d.as_ref().and_then(|d| d(&ev));
            async move { Ok(r) }
        })
    });
    assert!(b.host.load("p", plugin).await);
    b.agent.prompt_text("go").await.unwrap();
    let seen = seen.lock().unwrap().clone();
    let asked = asked.lock().unwrap().clone();
    Ran {
        seen,
        asked,
        result: first_tool_result(&b.agent.messages()),
        requests: b.provider.requests().len(),
    }
}

/// permission-hook.test.ts: "permission hook: sees the verdict and its limit; can make any verdict stricter"
#[tokio::test]
async fn permission_hook_sees_the_verdict_and_its_limit_can_make_any_verdict_stricter() {
    let r = run(
        "echo hi",
        Opts {
            decide: decide(Decision::Deny, Some("second opinion")),
            ..Default::default()
        },
    )
    .await;
    let ev = &r.seen[0];
    assert_eq!(ev.mode, PermissionMode::Auto);
    assert_eq!(
        (ev.verdict.decision, ev.verdict.source),
        (Decision::Allow, VerdictSource::Mode)
    );
    assert_eq!(ev.lowest, Decision::Allow);
    assert_eq!(ev.args.get("command"), Some(&json!("echo hi")));
    assert!(r.result.contains("Permission denied (second opinion)"), "{}", r.result);
}

/// permission-hook.test.ts: "permission hook: cannot loosen the user's deny rule, nor an ask rule"
#[tokio::test]
async fn permission_hook_cannot_loosen_the_users_deny_rule_nor_an_ask_rule() {
    let deny = run(
        "echo nope",
        Opts {
            rules: Some(json!({ "deny": ["bash(echo nope*)"] })),
            decide: decide(Decision::Allow, None),
            ..Default::default()
        },
    )
    .await;
    let ev = &deny.seen[0];
    assert_eq!(
        (ev.verdict.decision, ev.verdict.source),
        (Decision::Deny, VerdictSource::Rule)
    );
    assert_eq!(ev.lowest, Decision::Deny);
    assert!(
        deny.result.contains("Permission denied (rule deny: bash(echo nope*))"),
        "{}",
        deny.result
    );

    let ask = run(
        "echo q",
        Opts {
            rules: Some(json!({ "ask": ["bash(echo q*)"] })),
            decide: decide(Decision::Allow, None),
            approve: Some(false),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(ask.asked.len(), 1);
    assert!(ask.result.contains("The user denied"), "{}", ask.result);
}

/// permission-hook.test.ts: "softDeny: denied like deny; a permission hook can only hand it to the user"
#[tokio::test]
async fn soft_deny_denied_like_deny_a_permission_hook_can_only_hand_it_to_the_user() {
    let rules = json!({ "softDeny": ["bash(echo soft*)"] });
    let plain = run(
        "echo soft",
        Opts {
            rules: Some(rules.clone()),
            ..Default::default()
        },
    )
    .await;
    assert!(
        plain
            .result
            .contains("Permission denied (rule softDeny: bash(echo soft*))"),
        "{}",
        plain.result
    );

    let handed = run(
        "echo soft",
        Opts {
            rules: Some(rules),
            decide: decide(Decision::Allow, Some("looks fine")),
            approve: Some(true),
            ..Default::default()
        },
    )
    .await;
    let ev = &handed.seen[0];
    assert_eq!(
        (ev.verdict.decision, ev.verdict.source),
        (Decision::Deny, VerdictSource::Soft)
    );
    assert_eq!(ev.lowest, Decision::Ask);
    assert_eq!(handed.asked[0], "looks fine");
    assert!(handed.result.contains("soft"), "{}", handed.result);

    // A hard deny still wins when both match.
    let both = run(
        "echo soft",
        Opts {
            rules: Some(json!({ "softDeny": ["bash(echo soft*)"], "deny": ["bash(echo soft*)"] })),
            decide: decide(Decision::Ask, None),
            approve: Some(true),
            ..Default::default()
        },
    )
    .await;
    assert!(both.asked.is_empty());
    assert!(both.result.contains("rule deny"), "{}", both.result);
}

/// permission-hook.test.ts: "agent mode: a hook's decision is final; without one the core asks (the judge is a plugin)"
#[tokio::test]
async fn agent_mode_a_hooks_decision_is_final_without_one_the_core_asks_the_judge_is_a_plugin() {
    let decided = run(
        "echo a",
        Opts {
            mode: Some(PermissionMode::Agent),
            decide: decide(Decision::Allow, None),
            ..Default::default()
        },
    )
    .await;
    let ev = &decided.seen[0];
    assert_eq!(ev.mode, PermissionMode::Agent);
    assert_eq!(
        (ev.verdict.decision, ev.verdict.source),
        (Decision::Ask, VerdictSource::Mode)
    );
    assert!(decided.result.contains('a'), "{}", decided.result);
    // The call and the final answer: no judge request.
    assert_eq!(decided.requests, 2);

    let asked = run(
        "echo b",
        Opts {
            mode: Some(PermissionMode::Agent),
            approve: Some(false),
            ..Default::default()
        },
    )
    .await;
    // No judging request from the core.
    assert_eq!(asked.requests, 2);
    assert_eq!(asked.asked.len(), 1);
}

/// permission-hook.test.ts: "a tool_call decision cannot be loosened by a permission hook"
#[tokio::test]
async fn a_tool_call_decision_cannot_be_loosened_by_a_permission_hook() {
    let b = build(vec![bash("echo t"), done()], |o| {
        o.permissions = Some(policy(PermissionMode::Auto, json!({})))
    })
    .await;
    let lowest: Arc<Mutex<Option<Decision>>> = Arc::default();
    let first = plug(|api| {
        api.on::<hook::ToolCall, _, _>(|_ev, _| async {
            Ok(Some(ToolCallResult {
                decision: Some(Decision::Deny),
                reason: Some("tool_call says no".into()),
                args: None,
            }))
        })
    });
    let l = lowest.clone();
    let second = plug(move |api| {
        api.on::<hook::Permission, _, _>(move |ev, _| {
            *l.lock().unwrap() = Some(ev.lowest);
            async {
                Ok(Some(PermissionResult {
                    decision: Some(Decision::Allow),
                    reason: None,
                }))
            }
        })
    });
    assert!(b.host.load("first", first).await);
    assert!(b.host.load("second", second).await);
    b.agent.prompt_text("go").await.unwrap();
    assert_eq!(*lowest.lock().unwrap(), Some(Decision::Deny));
    assert!(first_tool_result(&b.agent.messages()).contains("tool_call says no"));
}

// ── packages/skills/test/authoring-doc.test.ts ───────────────────────

/// authoring-doc.test.ts `agentWith`: system prompt "", the built-in tools, then `plugins`; prompts
/// "go" when there is a script.
async fn agent_with(script: Vec<AssistantMessage>, plugins: Vec<(&str, PluginInit)>, mode: PermissionMode) -> Built {
    let prompt = !script.is_empty();
    let b = build(script, |o| {
        o.model.max_output = 1000;
        o.system_prompt = String::new();
        o.permissions = Some(policy(mode, json!({})));
    })
    .await;
    for (n, p) in plugins {
        b.host.load(n, p).await;
    }
    if prompt {
        b.agent.prompt_text("go").await.unwrap();
    }
    b
}

fn allow_all() -> PluginInit {
    plug(|api| {
        api.on::<hook::ToolCall, _, _>(|_ev, _| async {
            Ok(Some(ToolCallResult {
                decision: Some(Decision::Allow),
                reason: Some("plugin says ok".into()),
                args: None,
            }))
        })
    })
}

fn sudo_deny() -> PermissionRules {
    PermissionRules {
        deny: vec!["bash(sudo *)".into()],
        ..Default::default()
    }
}

/// authoring-doc.test.ts: "a plugin's allow replaces the mode default but not the user's deny rules"
#[tokio::test]
async fn a_plugins_allow_replaces_the_mode_default_but_not_the_users_deny_rules() {
    let b = agent_with(Vec::new(), vec![("allowAll", allow_all())], PermissionMode::Edits).await;
    b.agent.permissions().add_rules(&sudo_deny()).unwrap();
    let denied = b.agent.permissions().check(&PermissionCheck {
        tool: Arc::new(BashTool),
        args: obj(json!({ "command": "sudo ls" })),
        cwd: b.agent.cwd(),
    });
    assert_eq!((denied.decision, denied.source), (Decision::Deny, VerdictSource::Rule));

    let calls: Vec<AssistantContent> = vec![
        call("1", "bash", json!({ "command": "echo hi" })),
        call("2", "bash", json!({ "command": "sudo ls" })),
    ];
    let deny: PluginInit = Box::new(|api| Box::pin(async move { api.add_permission_rules(sudo_deny()) }));
    let b = agent_with(
        vec![reply(calls, "t"), reply(vec![say("done")], "t")],
        vec![("allowAll", allow_all()), ("deny", deny)],
        PermissionMode::Edits,
    )
    .await;
    let requests: Vec<Request> = b.provider.requests();
    let sent = &requests[1].messages;
    let (ok, blocked) = (&sent[sent.len() - 2], &sent[sent.len() - 1]);
    // Edits mode would have asked; the plugin allowed it.
    assert!(json(ok).contains("hi"), "{}", json(ok));
    assert!(
        json(blocked).contains("Permission denied (rule deny: bash(sudo *))"),
        "{}",
        json(blocked)
    );
}
