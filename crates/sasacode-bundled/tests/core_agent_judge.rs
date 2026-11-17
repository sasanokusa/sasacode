//! packages/bundled/test/agent-judge.test.ts: the "agent" permission mode's judge, a bundled
//! plugin: a model decides the calls the mode leaves to the user, through the permission hook, so
//! it never loosens a rule.

mod support_core;

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::permission::PermissionPolicy;
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_agent::tool_exec::{ApprovalAnswer, ApprovalDecision, ApprovalRequest};
use sasacode_ai::{AssistantMessage, Message, register_api};
use sasacode_plugin_api::{Lang, PermissionMode, PermissionRules};
use sasacode_testing::{ReplayOptions, ReplayProvider};
use support_core::*;

fn bash(command: &str) -> AssistantMessage {
    reply_at(vec![call("1", "bash", json!({ "command": command }))], 0)
}

fn text(t: &str) -> AssistantMessage {
    reply_at(vec![say(t)], 0)
}

struct Run {
    models: Vec<String>,
    asked: Vec<String>,
    tool: String,
}

async fn run(script: Vec<AssistantMessage>, mode: PermissionMode, rules: PermissionRules, settings: Value) -> Run {
    let dir = temp_dir("sasacode-agent-judge-");
    let cwd = path_str(dir.path());
    let api = api_name();
    let provider = Arc::new(ReplayProvider::new(script, ReplayOptions::api(&api)));
    register_api(&api, provider.clone());
    let base = model(&api, 100_000, 4000);
    let mut o = AgentOptions::new(base.clone(), cwd.clone(), "s");
    o.permissions = Some(Arc::new(PermissionPolicy::new(mode, &rules).unwrap()));
    o.resolve_model = Some(Arc::new(move |spec: &str| {
        let mut m = base.clone();
        m.id = spec[spec.find('/').map_or(0, |i| i + 1)..].to_string();
        Ok(m)
    }));
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let rec = asked.clone();
    o.approve = Some(Arc::new(move |r: ApprovalRequest| {
        rec.lock().unwrap_or_else(|e| e.into_inner()).push(r.reason.clone());
        Box::pin(async { Ok(ApprovalAnswer::new(ApprovalDecision::Deny)) })
    }));
    let agent = Agent::new(o);
    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd,
        settings: Some(Arc::new(move |_: &str| settings.clone())),
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: Lang::Ja,
    });
    assert!(host.load("builtin-tools", builtin_tools).await);
    assert!(host.load("agent-judge", bundled("agent-judge")).await);
    let _ = agent.prompt_text("go").await;
    let tool = agent
        .messages()
        .iter()
        .find_map(|m| match m {
            Message::Tool(t) => Some(serde_json::to_string(&t.content).unwrap_or_default()),
            _ => None,
        })
        .unwrap_or_default();
    let asked = asked.lock().unwrap().clone();
    Run {
        models: provider.requests().iter().map(|r| r.model.id.clone()).collect(),
        asked,
        tool,
    }
}

#[tokio::test]
async fn a_call_judged_safe_runs_unasked_with_the_judging_model_from_settings_the_run_keeps_its_own() {
    let r = run(
        vec![
            bash("echo j"),
            text(r#"{"safe": true, "reason": "echo"}"#),
            text("done"),
        ],
        PermissionMode::Agent,
        PermissionRules::default(),
        json!({ "model": "t/judge" }),
    )
    .await;
    assert_eq!(r.models, vec!["m", "judge", "m"]);
    assert!(r.asked.is_empty());
    assert!(r.tool.contains('j'), "{}", r.tool);
}

#[tokio::test]
async fn risky_unreadable_or_failed_judgements_go_to_the_user() {
    for verdict in [r#"{"safe": false, "reason": "deletes"}"#, "no json here"] {
        let r = run(
            vec![bash("rm -rf x"), text(verdict), text("done")],
            PermissionMode::Agent,
            PermissionRules::default(),
            json!({}),
        )
        .await;
        assert_eq!(r.asked.len(), 1);
        let want = if verdict.contains("deletes") {
            "agent judged risky: deletes"
        } else {
            "judge failed: judge gave no verdict"
        };
        assert!(r.asked[0].contains(want), "{}", r.asked[0]);
    }
}

#[tokio::test]
async fn the_judge_never_loosens_a_rule_and_other_modes_are_left_alone() {
    let rules = PermissionRules {
        ask: vec!["bash(echo*)".into()],
        ..PermissionRules::default()
    };
    let ruled = run(
        vec![bash("echo x"), text("done")],
        PermissionMode::Agent,
        rules,
        json!({}),
    )
    .await;
    assert_eq!(ruled.models, vec!["m", "m"]); // no judging request
    assert_eq!(ruled.asked.len(), 1);
    let edits = run(
        vec![bash("echo x"), text("done")],
        PermissionMode::Edits,
        PermissionRules::default(),
        json!({}),
    )
    .await;
    assert_eq!(edits.models, vec!["m", "m"]);
    assert_eq!(edits.asked.len(), 1);
}
