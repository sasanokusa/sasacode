//! packages/cli/test/plugins.test.ts, what remains with the plugins shipped inside sasacode
//! (rust/DECISIONS.md ADR-002), and the switches for the plugins that were separate packages
//! (ADR-011). The tests of discovering, loading, installing, removing and trusting plugins from disk
//! are dropped with external plugins (ADR-002); the one-trust-decision test is in src/trust.rs.

use std::sync::Arc;

use serde_json::{Value, json};

use sasacode_cli::setup::{Harness, SetupOptions, setup};

use crate::support::{Home, child, merged, replay_provider};

fn configure(t: &Home, extra: Value) {
    t.config(&merged(
        json!({ "providers": { "test": replay_provider() }, "model": "test/m" }),
        extra,
    ));
}

/// Setup and the plugins, as a headless run loads them.
async fn loaded(t: &Home, trust_project: bool) -> Arc<Harness> {
    let h = setup(SetupOptions {
        no_session: true,
        trust_project,
        ..SetupOptions::new(t.cwd())
    })
    .await
    .expect("setup");
    h.load_plugins(None).await;
    h
}

fn tools(h: &Harness) -> Vec<String> {
    h.agent.get_tools().iter().map(|t| t.name().to_string()).collect()
}

fn plugins(h: &Harness) -> Vec<String> {
    h.host.plugins().into_iter().map(|p| p.name).collect()
}

fn commands(h: &Harness) -> Vec<String> {
    h.host.commands().into_iter().map(|c| c.command.name).collect()
}

fn has(list: &[String], name: &str) -> bool {
    list.iter().any(|n| n == name)
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_disabled_and_tools_disabled_switch_things_off_p3() {
    let Some(t) = child() else { return };
    configure(&t, json!({}));
    // Without them, all are there.
    let all = tools(&*loaded(&t, true).await);
    for tool in ["read", "write", "edit", "bash", "task", "todo_write", "web_fetch"] {
        assert!(has(&all, tool), "{tool} missing from {all:?}");
    }
    t.project_config(&json!({ "plugins": { "disabled": ["todo"] }, "tools": { "disabled": ["bash", "web_fetch"] } }));
    let h = loaded(&t, true).await; // disabling plugins is a trusted setting
    let tools = tools(&h);
    assert!(!has(&tools, "todo_write"), "{tools:?}");
    assert!(!has(&plugins(&h), "todo"));
    assert!(!has(&tools, "bash"), "{tools:?}");
    assert!(!has(&tools, "web_fetch"), "{tools:?}");
    assert!(has(&tools, "read"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_is_off_by_default_loads_only_when_plugins_enabled_names_it() {
    let Some(t) = child() else { return };
    assert!(
        !sasacode_bundled::find("ask-user")
            .expect("ask-user ships")
            .on_by_default
    );
    configure(&t, json!({}));
    let h = loaded(&t, false).await;
    assert!(!has(&plugins(&h), "ask-user"));
    assert!(!has(&tools(&h), "ask_user"));
    configure(&t, json!({ "plugins": { "enabled": ["ask-user"] } }));
    let h = loaded(&t, false).await;
    assert!(has(&plugins(&h), "ask-user"), "{:?}", plugins(&h));
    assert!(has(&tools(&h), "ask_user"));
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_disabled_wins_over_plugins_enabled() {
    let Some(t) = child() else { return };
    configure(
        &t,
        json!({ "plugins": { "enabled": ["ask-user"], "disabled": ["ask-user"] } }),
    );
    let h = loaded(&t, false).await;
    assert!(!has(&plugins(&h), "ask-user"));
    assert!(!has(&tools(&h), "ask_user"));
}

#[tokio::test(flavor = "multi_thread")]
async fn jev_guards_own_enabled_setting_turns_it_on_as_it_did_when_jev_guard_was_installed() {
    let Some(t) = child() else { return };
    assert!(
        !sasacode_bundled::find("jev-guard")
            .expect("jev-guard ships")
            .on_by_default
    );
    configure(&t, json!({}));
    let h = loaded(&t, false).await;
    assert!(!has(&plugins(&h), "jev-guard"));
    assert!(!has(&commands(&h), "jev"));
    let on = json!({ "jev-guard": { "enabled": true } });
    configure(&t, json!({ "plugins": { "settings": on } }));
    let h = loaded(&t, false).await;
    assert!(has(&plugins(&h), "jev-guard"), "{:?}", plugins(&h));
    assert!(has(&commands(&h), "jev"));
    // plugins.disabled still wins.
    configure(&t, json!({ "plugins": { "settings": on, "disabled": ["jev-guard"] } }));
    assert!(!has(&plugins(&*loaded(&t, false).await), "jev-guard"));
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_enabled_in_an_untrusted_project_config_is_ignored() {
    let Some(t) = child() else { return };
    configure(&t, json!({}));
    t.project_config(&json!({ "plugins": { "enabled": ["ask-user"] } }));
    let h = loaded(&t, false).await;
    assert!(!has(&plugins(&h), "ask-user"));
    assert!(
        h.warnings()
            .iter()
            .any(|w| w.contains("not applied until you trust this project")),
        "{:?}",
        h.warnings()
    );
    // Trusted, it applies.
    assert!(has(&plugins(&*loaded(&t, true).await), "ask-user"));
}
