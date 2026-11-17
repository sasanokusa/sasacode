//! plugins/workflows/index.test.ts. The TS replaced `agent.inject` to watch what the command
//! injects; here the agent talks to a replay api of its own, and an injection "now" shows up as the
//! run it starts (one request whose last message is the prompt).

mod support_core;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::plugins::{OwnedCommand, PluginHost, PluginHostOptions};
use sasacode_ai::{Message, register_api};
use sasacode_bundled::workflows::{self, Workflow, load_workflows, render_prompt};
use sasacode_plugin_api::{CommandContext, Lang, PermissionMode, SelectOption};
use sasacode_testing::{ReplayOptions, ReplayProvider};
use support_core::*;

#[test]
fn localized_defaults_can_be_extended_overridden_and_disabled() {
    assert!(
        load_workflows(Lang::Ja, &json!({})).unwrap()["review"]
            .prompt
            .contains("レビュー")
    );
    assert!(
        load_workflows(Lang::En, &json!({})).unwrap()["review"]
            .prompt
            .contains("Review")
    );
    let w = load_workflows(
        Lang::En,
        &json!({ "workflows": { "review": false, "explain": { "description": "Explain", "prompt": "Explain {{input}}" } } }),
    )
    .unwrap();
    assert!(!w.contains_key("review"));
    assert_eq!(w["explain"].prompt, "Explain {{input}}");
}

#[test]
fn template_expansion_is_literal_single_pass_and_appends_when_no_placeholder() {
    let input = "$& {{input}} $(touch nope)\nsecond line";
    let w = |prompt: &str| Workflow {
        description: String::new(),
        prompt: prompt.into(),
    };
    assert_eq!(render_prompt(&w("Review {{input}}"), input), format!("Review {input}"));
    assert_eq!(render_prompt(&w("Review"), "here"), "Review\n\nhere");
}

#[test]
fn rejects_malformed_settings_and_unsupported_placeholders() {
    for workflows in [
        json!([]),
        json!({ "show": false }),
        json!({ "bad": { "description": "x", "prompt": "{{env}}" } }),
        json!({ "bad": { "description": "x", "prompt": "" } }),
    ] {
        assert!(
            load_workflows(Lang::En, &json!({ "workflows": workflows })).is_err(),
            "{workflows}"
        );
    }
}

struct Harness {
    agent: Agent,
    host: Arc<PluginHost>,
    provider: Arc<ReplayProvider>,
    notices: Arc<Mutex<Vec<String>>>,
}

/// `loadPlugin` (lang en, mode edits), with the agent on a replay api of its own.
async fn load(settings: Value) -> Harness {
    let api = api_name();
    let provider = Arc::new(ReplayProvider::new(
        vec![reply(vec![say("done")])],
        ReplayOptions::api(&api),
    ));
    register_api(&api, provider.clone());
    let cwd = path_str(&std::env::current_dir().unwrap());
    let mut o = AgentOptions::new(model(&api, 10_000, 1000), cwd.clone(), "test");
    o.permissions = Some(policy(PermissionMode::Edits));
    let agent = Agent::new(o);
    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd,
        settings: Some(Arc::new(move |_| settings.clone())),
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: Lang::En,
    });
    let ui = NoticeUi::default();
    let notices = ui.notices.clone();
    host.set_ui(Some(Arc::new(ui)));
    assert!(host.load("workflows", workflows::plugin).await);
    Harness {
        agent,
        host,
        provider,
        notices,
    }
}

/// The prompts the agent sent to its model: one per injection made "now".
async fn injected(h: &Harness) -> Vec<String> {
    // An injection starts its run on its own: give it the time to.
    tokio::time::sleep(Duration::from_millis(50)).await;
    h.agent.wait_for_idle().await;
    h.provider
        .requests()
        .iter()
        .map(|r| match last(r) {
            Message::User(u) => u
                .content
                .iter()
                .filter_map(|c| c.as_text())
                .collect::<Vec<_>>()
                .join(""),
            m => format!("{m:?}"),
        })
        .collect()
}

#[tokio::test]
async fn list_preview_never_inject_execution_injects_exactly_once_and_completes_names() {
    let h =
        load(json!({ "workflows": { "explain": { "description": "Explain", "prompt": "Explain {{input}}" } } })).await;
    let command: OwnedCommand = h
        .host
        .commands()
        .into_iter()
        .find(|c| c.command.name == "workflow")
        .unwrap();
    let run = |args: &str| (command.command.run)(CommandContext { args: args.into() });
    run("").await.unwrap();
    run("show explain $&\nnext line").await.unwrap();
    assert!(injected(&h).await.is_empty());
    assert_eq!(
        h.notices.lock().unwrap().last().unwrap(),
        "/workflow explain\nExplain $&\nnext line"
    );
    run("missing").await.unwrap();
    assert!(injected(&h).await.is_empty());
    run("explain login").await.unwrap();
    assert_eq!(injected(&h).await, vec!["Explain login"]);
    let complete = command.command.complete.as_ref().unwrap();
    assert_eq!(
        complete("show exp".into()).await,
        vec![SelectOption::new("show explain", "explain").describe("Explain")]
    );
}
