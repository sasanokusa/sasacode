//! The parts of packages/cli/test/catalog.test.ts and endpoints.test.ts that go through setup (the
//! rest are lib tests in src/catalog_tests.rs and src/endpoints_tests.rs).

use serde_json::{Value, json};

use sasacode_cli::setup::{SetupOptions, setup};

use crate::support::{child, provider};
use crate::test_server::{Res, TestServer};

/// The end of catalog.test.ts "two servers behind one proxy, told apart by path, are listed each
/// with its own models, and -m takes either (#105)": the startup check of -m goes through the same
/// listing.
#[tokio::test(flavor = "multi_thread")]
async fn m_takes_either_of_two_servers_behind_one_proxy_told_apart_by_path_105() {
    let Some(t) = child() else { return };
    let proxy = TestServer::start(|req| match req.path.as_str() {
        "/vllm/v1/models" => {
            Res::json(json!({ "object": "list", "data": [{ "id": "qwen-coder", "object": "model" }] }))
        }
        "/tabby/v1/models" => {
            Res::json(json!({ "object": "list", "data": [{ "id": "deepseek-coder", "object": "model" }] }))
        }
        _ => Res::text(404, ""),
    });
    let providers = json!({
        "vllm": provider("openai-chat", &format!("{}/vllm/v1", proxy.url())),
        "tabby": provider("openai-chat", &format!("{}/tabby/v1", proxy.url())),
    });
    t.config(
        &json!({ "providers": providers, "model": "vllm/qwen-coder", "plugins": { "disabled": ["openai-codex"] } }),
    );
    let h = setup(SetupOptions {
        model: Some("tabby/deepseek-coder".into()),
        no_session: true,
        ..SetupOptions::new(t.cwd())
    })
    .await
    .expect("setup");
    h.check_model("tabby/deepseek-coder")
        .await
        .expect("tabby lists deepseek-coder");
    let e = h
        .check_model("tabby/qwen-coder")
        .await
        .expect_err("qwen-coder is the other server's");
    assert!(format!("{e:#}").contains("tabby/qwen-coder"), "{e:#}");
    h.shutdown().await;
}

/// endpoints.test.ts "setup resolves a model on a URL-only provider".
#[tokio::test(flavor = "multi_thread")]
async fn setup_resolves_a_model_on_a_url_only_provider() {
    let Some(t) = child() else { return };
    let anthropic = TestServer::start(|req| {
        if req.path == "/v1/models" {
            let model =
                json!({ "type": "model", "id": "claude-local", "display_name": "Local", "max_input_tokens": 200_000 });
            Res::json(json!({
                "data": [model],
                "has_more": false,
                "first_id": "claude-local",
                "last_id": "claude-local",
            }))
        } else {
            Res::text(404, "")
        }
    });
    let mine = json!({ "baseUrl": format!("{}/v1", anthropic.url()), "apiKeyEnv": crate::support::KEY_VAR });
    t.config(&json!({ "providers": { "mine": mine }, "model": "mine/claude-local" }));
    let h = setup(SetupOptions {
        no_session: true,
        ..SetupOptions::new(t.cwd())
    })
    .await
    .expect("setup");
    let model = serde_json::to_value(h.agent.model()).unwrap();
    let picked: Value = ["provider", "api", "baseUrl", "id"]
        .iter()
        .map(|k| (k.to_string(), model[*k].clone()))
        .collect::<serde_json::Map<_, _>>()
        .into();
    assert_eq!(
        picked,
        json!({ "provider": "mine", "api": "anthropic", "baseUrl": anthropic.url(), "id": "claude-local" })
    );
    assert!(t.home.join("endpoints.json").exists());
}

/// The agent is not kept alive by the closures it hands its own options: once the harness is
/// dropped, the agent goes with it.
#[tokio::test(flavor = "multi_thread")]
async fn the_agent_is_freed_when_the_harness_is_dropped() {
    let Some(t) = child() else { return };
    t.config(&json!({ "plugins": { "disabled": ["openai-codex"] } }));
    let h = setup(SetupOptions {
        no_session: true,
        ..SetupOptions::new(t.cwd())
    })
    .await
    .expect("setup");
    // Held only by a listener of the agent's own bus, so it goes when the agent does.
    let alive = std::sync::Arc::new(());
    let seen = std::sync::Arc::downgrade(&alive);
    h.agent.events().on(move |_| {
        let _ = &alive;
    });
    assert!(seen.upgrade().is_some());
    h.shutdown().await;
    drop(h);
    assert!(seen.upgrade().is_none(), "the agent outlived its harness");
}
