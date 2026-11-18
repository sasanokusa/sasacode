//! packages/cli/test/catalog.test.ts

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use futures::FutureExt;
use serde_json::json;

use super::*;
use crate::list_models::test_server::{Res, TestServer};

pub(crate) fn pc(api: &str, base_url: &str, key_env: Option<&str>) -> ProviderConfig {
    ProviderConfig {
        api: api.into(),
        base_url: Some(base_url.into()),
        api_key_env: key_env.map(str::to_string),
        ..Default::default()
    }
}

pub(crate) fn shared(list: Vec<(&str, ProviderConfig)>) -> Providers {
    Arc::new(RwLock::new(list.into_iter().map(|(n, p)| (n.to_string(), p)).collect()))
}

pub(crate) fn keys(f: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> GetKey {
    Arc::new(move |p: String| {
        let k = f(&p);
        async move { k }.boxed()
    })
}

pub(crate) fn current(name: &str) -> Arc<dyn Fn() -> String + Send + Sync> {
    let name = name.to_string();
    Arc::new(move || name.clone())
}

fn specs(list: &[ModelChoice]) -> Vec<&str> {
    list.iter().map(|m| m.spec.as_str()).collect()
}

/// One fake OpenAI-compatible endpoint standing in for a multi-format service like Command Code.
fn service_server() -> TestServer {
    TestServer::start(|req| {
        if req.header("authorization") != Some("Bearer k") {
            return Res::text(401, "{}");
        }
        Res::json(json!({
            "object": "list",
            "data": [
                { "id": "claude-x", "object": "model", "context_length": 1_000_000, "supported_endpoints": ["/messages"] },
                { "id": "gpt-y", "object": "model", "context_length": 400_000, "supported_endpoints": ["/chat/completions", "/responses"] },
                { "id": "oddball", "object": "model", "supported_endpoints": ["/systemone"] },
                { "id": "text-embedding-3-small", "object": "model" },
                { "id": "plain", "object": "model" },
            ],
        }))
    })
}

fn service_providers(server: &TestServer) -> Providers {
    let base = format!("{}/v1", server.url());
    shared(vec![
        ("svc", pc("openai-chat", &base, Some("SVC_KEY"))),
        ("svc-responses", pc("openai-responses", &base, Some("SVC_KEY"))),
        ("svc-anthropic", pc("anthropic", &server.url(), Some("SVC_KEY"))),
        ("nokey", pc("openai-chat", "http://127.0.0.1:9/v1", Some("MISSING_KEY"))),
    ])
}

fn override_cw(cw: u64) -> ModelOverride {
    ModelOverride {
        context_window: Some(cw),
        ..Default::default()
    }
}

#[tokio::test]
async fn lists_each_service_once_routes_models_to_the_provider_speaking_their_endpoint_keeps_context_windows() {
    let server = service_server();
    let providers = service_providers(&server);
    let catalog = ModelCatalog::new(
        providers.clone(),
        keys(|p| p.starts_with("svc").then(|| "k".to_string())),
        current("svc"),
    );
    let listed = catalog.list(false).await;
    assert_eq!(specs(&listed), ["svc-anthropic/claude-x", "svc/gpt-y", "svc/plain"]);
    // svc has no flag, so it is asked whether it is LM Studio: this server answers /api/v0/models with
    // its OpenAI-style list too, which has no max_context_length and so is not LM Studio's.
    assert!(!listed.iter().any(|m| m.server.is_some()));
    assert_eq!(providers.read().unwrap()["svc"].lmstudio, None);
    assert_eq!(catalog.discovered().get("svc/gpt-y"), Some(&override_cw(400_000)));
    assert_eq!(
        catalog.discovered().get("svc-anthropic/claude-x"),
        Some(&override_cw(1_000_000))
    );
}

#[tokio::test]
async fn providers_without_a_key_are_skipped_and_failures_just_yield_nothing() {
    let server = service_server();
    let providers = service_providers(&server);
    let bad = ModelCatalog::new(providers.clone(), keys(|_| Some("wrong".into())), current("none"));
    assert_eq!(bad.list(false).await, []); // 401 from the service, nokey unreachable
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let svc = providers.read().unwrap()["svc"].clone();
    let cached = ModelCatalog::new(
        shared(vec![("svc", svc)]),
        keys(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            Some("k".into())
        }),
        current("svc"),
    );
    cached.list(false).await;
    cached.list(false).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    cached.list(true).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_refresh_that_fails_keeps_the_last_listing_the_servers_models_stay_in_model() {
    let up = Arc::new(AtomicBool::new(true));
    let state = up.clone();
    let flaky = TestServer::start(move |_| {
        if state.load(Ordering::SeqCst) {
            Res::json(json!({ "object": "list", "data": [{ "id": "m", "object": "model" }] }))
        } else {
            Res::text(404, "")
        }
    });
    let catalog = ModelCatalog::new(
        shared(vec![("svc", pc("openai-chat", &format!("{}/v1", flaky.url()), None))]),
        keys(|_| None),
        current("svc"),
    );
    assert_eq!(specs(&catalog.list(false).await), ["svc/m"]);
    up.store(false, Ordering::SeqCst);
    assert_eq!(specs(&catalog.list(true).await), ["svc/m"]);
    assert_eq!(catalog.entry("svc/m", true).await.map(|m| m.spec), Some("svc/m".into()));
}

#[tokio::test]
async fn ollama_num_ctx_is_the_context_window_otherwise_the_trained_maximum_is_shown_openai_falls_back_to_known_sizes()
{
    let ollama = TestServer::start(|req| match req.path.as_str() {
        "/v1/models" => Res::json(json!({
            "object": "list",
            "data": [{ "id": "small-32k", "object": "model" }, { "id": "plain", "object": "model" }],
        })),
        "/api/show" => {
            let model = req.json()["model"].as_str().unwrap_or_default().to_string();
            Res::json(json!({
                "model_info": { "llama.context_length": 131072 },
                "parameters": if model == "small-32k" { "num_ctx                        32768\nstop <eos>" } else { "" },
            }))
        }
        _ => Res::text(404, ""),
    });
    let openai = TestServer::start(|_| {
        Res::json(json!({
            "object": "list",
            "data": [{ "id": "gpt-5.5", "object": "model" }, { "id": "gpt-unknown", "object": "model" }],
        }))
    });
    let mut local = pc("openai-chat", &format!("{}/v1", ollama.url()), None);
    local.ollama = Some(true);
    let catalog = ModelCatalog::new(
        shared(vec![
            ("local", local),
            (
                "oa",
                pc("openai-responses", &format!("{}/v1", openai.url()), Some("OA")),
            ),
        ]),
        keys(|_| Some("k".into())),
        current("local"),
    );
    let listed = catalog.list(false).await;
    let by_id = |spec: &str| listed.iter().find(|m| m.spec == spec).cloned();
    assert_eq!(
        by_id("local/small-32k"),
        Some(ModelChoice {
            spec: "local/small-32k".into(),
            context_window: Some(32768),
            max_context: None,
            server: Some("ollama".into()),
        })
    );
    assert_eq!(
        by_id("local/plain"),
        Some(ModelChoice {
            spec: "local/plain".into(),
            context_window: None,
            max_context: Some(131072),
            server: Some("ollama".into()),
        })
    );
    assert_eq!(catalog.discovered().get("local/small-32k"), Some(&override_cw(32768))); // used for thresholds too
    assert!(!catalog.has_discovered("local/plain")); // the maximum is not what requests get
    assert_eq!(by_id("oa/gpt-5.5").and_then(|m| m.context_window), Some(400_000));
    assert_eq!(by_id("oa/gpt-unknown").map(|m| m.context_window), Some(None));
}

#[tokio::test]
async fn two_servers_behind_one_proxy_told_apart_by_path_are_listed_each_with_its_own_models() {
    let proxy = TestServer::start(|req| match req.path.as_str() {
        "/vllm/v1/models" => {
            Res::json(json!({ "object": "list", "data": [{ "id": "qwen-coder", "object": "model" }] }))
        }
        "/tabby/v1/models" => {
            Res::json(json!({ "object": "list", "data": [{ "id": "deepseek-coder", "object": "model" }] }))
        }
        _ => Res::text(404, ""),
    });
    let catalog = ModelCatalog::new(
        shared(vec![
            ("vllm", pc("openai-chat", &format!("{}/vllm/v1", proxy.url()), None)),
            ("tabby", pc("openai-chat", &format!("{}/tabby/v1", proxy.url()), None)),
        ]),
        keys(|_| None),
        current("vllm"),
    );
    let mut all: Vec<String> = catalog.list(false).await.into_iter().map(|m| m.spec).collect();
    all.sort();
    assert_eq!(all, ["tabby/deepseek-coder", "vllm/qwen-coder"]);
    assert_eq!(
        catalog.offers("tabby/deepseek-coder").await,
        Some(Offers {
            ok: true,
            similar: vec![]
        })
    );
    assert_eq!(catalog.offers("tabby/qwen-coder").await.map(|o| o.ok), Some(false)); // the other server's model
    assert_eq!(
        catalog.entry("tabby/deepseek-coder", false).await.map(|m| m.spec),
        Some("tabby/deepseek-coder".into())
    );
    // The rest of this TS test (setup's startup check of -m: checkModel) goes through setup.
}

#[tokio::test]
async fn one_service_under_one_key_stays_one_group_whether_its_base_url_ends_in_v1_or_not() {
    let server = service_server();
    let catalog = ModelCatalog::new(
        shared(vec![
            (
                "svc",
                pc("openai-chat", &format!("{}/v1/", server.url()), Some("SVC_KEY")),
            ),
            (
                "svc-anthropic",
                pc("anthropic", &format!("{}/", server.url()), Some("SVC_KEY")),
            ),
        ]),
        keys(|_| Some("k".into())),
        current("svc"),
    );
    assert_eq!(
        specs(&catalog.list(false).await),
        ["svc-anthropic/claude-x", "svc/gpt-y", "svc/plain"]
    );
}

#[test]
fn similar_names_share_words() {
    let ids: Vec<String> = ["deepseek-v4-flash", "deepseek-v4-pro", "gpt-5.5"]
        .map(String::from)
        .to_vec();
    assert_eq!(
        similar("deepseek-v4-flsh", &ids),
        ["deepseek-v4-flash", "deepseek-v4-pro"]
    );
    assert_eq!(similar("x", &ids), Vec::<String>::new());
}

#[test]
fn service_and_untold() {
    assert_eq!(
        service(&pc("openai-chat", "https://api.commandcode.ai/provider/v1", None)),
        "api.commandcode.ai/provider"
    );
    assert_eq!(
        service(&pc("anthropic", "https://api.commandcode.ai/provider", None)),
        "api.commandcode.ai/provider"
    );
    assert_eq!(
        service(&ProviderConfig {
            api: "anthropic".into(),
            ..Default::default()
        }),
        "api.anthropic.com"
    );
    assert_eq!(service(&pc("openai-chat", "not a url", None)), "not a url");
    assert!(untold("mine", Some(&pc("openai-chat", "http://h/v1", None))));
    assert!(!untold("ollama", Some(&pc("openai-chat", "http://h/v1", None))));
    assert!(!untold("mine", Some(&pc("anthropic", "http://h", None))));
    assert!(!untold("mine", None));
    assert_eq!(provider_of("a/b/c"), Some("a"));
    assert_eq!(provider_of("abc"), Some("ab"));
    assert_eq!(model_of("a/b/c"), "b/c");
}

/// What setup, main and the TUI spawn or await must be `Send`.
#[test]
fn public_futures_are_send() {
    fn send<T: Send>(_: T) {}
    let catalog = ModelCatalog::new(shared(vec![]), keys(|_| None), current("x"));
    send(catalog.list(false));
    send(catalog.entry("a/b", false));
    send(catalog.offers("a/b"));
    let mut providers = IndexMap::new();
    let mut warnings = Vec::new();
    send(crate::endpoints::resolve_endpoints(
        &mut providers,
        keys(|_| None),
        &mut warnings,
    ));
    send(crate::endpoints::endpoint_command(&[], std::path::Path::new(".")));
    send(crate::update::update_command(&[]));
    send(crate::update::notify_if_newer(Arc::new(|_, _| {})));
}
