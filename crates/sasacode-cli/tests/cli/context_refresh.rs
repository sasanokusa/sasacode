//! packages/cli/test/context-refresh.test.ts. A local model's real context window is learned after
//! its first reply (#61): a fake Ollama whose /api/ps is empty until the model has been asked
//! something, as the real one is before loading, and a fake LM Studio whose /api/v0/models has no
//! loaded_context_length until then (#59). Each test runs in a home of its own (`isolated`), with
//! its own servers.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use sasacode_agent::events::AgentEvent;
use sasacode_cli::catalog::ModelChoice;
use sasacode_cli::headless::{OutputFormat, run_headless};
use sasacode_cli::setup::{Harness, SetupOptions, setup};
use sasacode_plugin_api::hook;

use crate::support::{
    Home, KEY_VAR, Role, child, isolated, merged, provider, sink, sse, sse_answer, with_quiet_builtins,
};
use crate::test_server::{Req, Res, TestServer};

/// A streamed Chat Completions answer, "ok", to the model `req` asks for.
fn reply(req: &Req) -> Res {
    let model = req.json()["model"].as_str().unwrap_or_default().to_string();
    sse(sse_answer(&model, "ok"))
}

fn n(c: &AtomicUsize) -> usize {
    c.load(Ordering::SeqCst)
}

#[derive(Default)]
struct OllamaState {
    loaded: AtomicBool,
    ps_hits: AtomicUsize,
    list_hits: AtomicUsize,
    /// E.g. Ollama started after sasacode, or a refresh that fails.
    list_down: AtomicBool,
}

#[derive(Default)]
struct StudioState {
    /// LM Studio loads a model at its first request, with its own default length, not the maximum.
    loaded: AtomicBool,
    /// Not started yet when sasacode lists the models.
    down: AtomicBool,
    bodies: Mutex<Vec<Value>>,
    native_hits: AtomicUsize,
}

#[derive(Default)]
struct GatewayState {
    /// Its /v1/models leaves out the model in use (an alias it still answers to), or is missing.
    list_missing: AtomicBool,
    /// Everything but chat.
    hits: AtomicUsize,
}

/// The fake servers, and the sink the built-in providers go to.
struct Servers {
    ollama: TestServer,
    ollama_state: Arc<OllamaState>,
    lmstudio: TestServer,
    studio: Arc<StudioState>,
    llamacpp: TestServer,
    cpp_list_hits: Arc<AtomicUsize>,
    gateway: TestServer,
    gw: Arc<GatewayState>,
    sink: TestServer,
}

impl Servers {
    fn start() -> Servers {
        let ollama_state = Arc::new(OllamaState::default());
        let ollama = {
            let s = ollama_state.clone();
            TestServer::start(move |req| match req.path.as_str() {
                "/v1/models" if s.list_down.load(Ordering::SeqCst) => Res::text(404, ""),
                // "ghost" never shows in /api/ps: its size stays unknown.
                "/v1/models" => {
                    s.list_hits.fetch_add(1, Ordering::SeqCst);
                    let data: Vec<Value> = ["plain", "small-32k", "ghost"]
                        .iter()
                        .map(|id| json!({ "id": id, "object": "model", "owned_by": "library" }))
                        .collect();
                    Res::json(json!({ "object": "list", "data": data }))
                }
                "/api/show" => {
                    let model = req.json()["model"].as_str().unwrap_or_default().to_string();
                    let parameters = if model == "small-32k" { "num_ctx 32768" } else { "" };
                    Res::json(json!({ "model_info": { "gemma4.context_length": 131072 }, "parameters": parameters }))
                }
                "/api/ps" => {
                    s.ps_hits.fetch_add(1, Ordering::SeqCst);
                    let models = if s.loaded.load(Ordering::SeqCst) {
                        json!([{ "name": "plain", "model": "plain", "context_length": 8192 }])
                    } else {
                        json!([])
                    };
                    Res::json(json!({ "models": models }))
                }
                "/v1/chat/completions" => {
                    // Ollama loads the model to answer, with its own default length (no num_ctx).
                    s.loaded.store(true, Ordering::SeqCst);
                    reply(req)
                }
                _ => Res::text(404, ""),
            })
        };
        let studio = Arc::new(StudioState::default());
        let lmstudio = {
            let s = studio.clone();
            TestServer::start(move |req| {
                if s.down.load(Ordering::SeqCst) {
                    return Res::text(503, "");
                }
                let ids = [("qwen", 32768), ("big", 131072)];
                match req.path.as_str() {
                    "/v1/models" => {
                        let data: Vec<Value> = ids
                            .iter()
                            .map(|(id, _)| json!({ "id": id, "object": "model", "owned_by": "organization_owner" }))
                            .collect();
                        Res::json(json!({ "object": "list", "data": data }))
                    }
                    "/api/v0/models" => {
                        s.native_hits.fetch_add(1, Ordering::SeqCst);
                        let loaded = s.loaded.load(Ordering::SeqCst);
                        let data: Vec<Value> = ids
                            .iter()
                            .map(|(id, max)| {
                                let size = if loaded {
                                    json!({ "state": "loaded", "loaded_context_length": 4096 })
                                } else {
                                    json!({ "state": "not-loaded" })
                                };
                                merged(
                                    json!({ "id": id, "object": "model", "type": "llm", "max_context_length": max }),
                                    size,
                                )
                            })
                            .collect();
                        Res::json(json!({ "object": "list", "data": data }))
                    }
                    "/v1/chat/completions" => {
                        s.loaded.store(true, Ordering::SeqCst);
                        s.bodies.lock().unwrap().push(req.json());
                        reply(req)
                    }
                    _ => Res::text(404, ""),
                }
            })
        };
        // llama-server: one model, the size of each request slot in /props.
        let cpp_list_hits = Arc::new(AtomicUsize::new(0));
        let llamacpp = {
            let hits = cpp_list_hits.clone();
            TestServer::start(move |req| match req.path.as_str() {
                "/v1/models" => {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let meta = json!({ "n_ctx_train": 131072 });
                    let qwen = json!({ "id": "qwen", "object": "model", "owned_by": "llamacpp", "meta": meta });
                    Res::json(json!({ "object": "list", "data": [qwen] }))
                }
                "/props" => Res::json(json!({ "default_generation_settings": { "n_ctx": 16384 } })),
                "/v1/chat/completions" => reply(req),
                _ => Res::text(404, ""),
            })
        };
        // A cloud gateway added by hand (no flag): no /api/v0/models, and a /v1/models that leaves
        // out the model in use or is missing.
        let gw = Arc::new(GatewayState::default());
        let gateway = {
            let s = gw.clone();
            TestServer::start(move |req| {
                if req.path == "/v1/chat/completions" {
                    return reply(req);
                }
                s.hits.fetch_add(1, Ordering::SeqCst);
                if req.path == "/v1/models" && !s.list_missing.load(Ordering::SeqCst) {
                    let flash = json!({ "id": "flash-v2", "object": "model", "owned_by": "gw" });
                    return Res::json(json!({ "object": "list", "data": [flash] }));
                }
                Res::text(404, "")
            })
        };
        Servers {
            ollama,
            ollama_state,
            lmstudio,
            studio,
            llamacpp,
            cpp_list_hits,
            gateway,
            gw,
            sink: sink(),
        }
    }

    fn studio_url(&self) -> String {
        self.lmstudio.url()
    }

    /// The global config: `local` is the fake Ollama; `providers` are added; `extra` replaces
    /// top-level keys. The openai-codex plugin is off: it registers its provider again, with no key
    /// variable, and listing every provider would then ask the keychain for its key.
    fn configure(&self, t: &Home, extra: Value, model: &str, providers: Value) {
        let mut local = provider("openai-chat", &format!("{}/v1", self.ollama.url()));
        local["ollama"] = json!(true);
        let providers = merged(json!({ "local": local }), providers);
        let config = json!({
            "providers": with_quiet_builtins(&self.sink.url(), providers),
            "model": model,
            "plugins": { "disabled": ["openai-codex"] },
        });
        t.config(&merged(config, extra));
    }
}

/// `{ api: "openai-chat", baseUrl, apiKeyEnv }` plus `extra`.
fn openai(base_url: String, extra: Value) -> Value {
    merged(provider("openai-chat", &base_url), extra)
}

async fn harness(t: &Home) -> Arc<Harness> {
    setup(SetupOptions {
        no_session: true,
        ..SetupOptions::new(t.cwd())
    })
    .await
    .expect("setup")
}

async fn listed(h: &Harness, spec: &str, refresh: bool) -> Option<ModelChoice> {
    h.list_models(refresh).await.into_iter().find(|m| m.spec == spec)
}

async fn prompt(h: &Harness, text: &str) {
    h.agent.prompt_text(text).await.expect("the run");
}

fn cw(h: &Harness) -> u64 {
    h.agent.model().context_window
}

fn server(h: &Harness) -> Value {
    serde_json::to_value(h.agent.model()).expect("the model")["server"].clone()
}

/// The context window the model plans each turn with (taken when the turn starts).
fn turn_sizes(h: &Harness) -> Arc<Mutex<Vec<u64>>> {
    let sizes = Arc::new(Mutex::new(Vec::new()));
    let (agent, out) = (h.agent.clone(), sizes.clone());
    h.agent.events().on(move |e| {
        if matches!(e, AgentEvent::TurnStart { .. }) {
            out.lock().unwrap().push(agent.model().context_window);
        }
    });
    sizes
}

async fn headless(h: &Harness) {
    run_headless(
        h,
        "hi".into(),
        OutputFormat::Text,
        None::<tokio::io::Empty>,
        Default::default(),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn listed_before_loading_the_model_plans_with_the_default_after_its_first_reply_with_what_ollama_loaded_it_with()
{
    let Some(t) = child() else { return };
    let s = Servers::start();
    s.configure(&t, json!({}), "local/plain", json!({}));
    let h = harness(&t).await;
    let plain = listed(&h, "local/plain", false).await.expect("local/plain listed");
    assert_eq!(
        (plain.context_window, plain.max_context, plain.server.as_deref()),
        (None, Some(131072), Some("ollama"))
    );
    assert_eq!(cw(&h), 128_000);
    // A plugin's turn_end hook (compaction's threshold) runs after the size is learned, in the same turn.
    let seen_by_plugins = Arc::new(AtomicU64::new(0));
    {
        let (agent, seen) = (h.agent.clone(), seen_by_plugins.clone());
        h.agent
            .hooks()
            .on::<hook::TurnEnd, _, _>("later-plugin", move |_e, _ctx| {
                seen.store(agent.model().context_window, Ordering::SeqCst);
                async { Ok(None) }
            });
    }
    prompt(&h, "hi").await;
    assert_eq!(cw(&h), 8192);
    assert_eq!(seen_by_plugins.load(Ordering::SeqCst), 8192);
    // /model shows it too.
    assert_eq!(
        listed(&h, "local/plain", false).await.and_then(|m| m.context_window),
        Some(8192)
    );
    // Once known, the next turns do not ask again.
    let hits = n(&s.ollama_state.ps_hits);
    prompt(&h, "again").await;
    assert_eq!(n(&s.ollama_state.ps_hits), hits);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_could_not_list_the_model_yet_a_failed_refresh_is_asked_again_at_a_later_turns_end() {
    let Some(t) = child() else { return };
    let s = Servers::start();
    s.configure(&t, json!({}), "local/plain", json!({}));
    s.ollama_state.list_down.store(true, Ordering::SeqCst);
    let h = harness(&t).await;
    assert_eq!(listed(&h, "local/plain", false).await, None);
    prompt(&h, "hi").await; // the reply comes, but the refresh at its end fails too
    assert_eq!(cw(&h), 128_000);
    s.ollama_state.list_down.store(false, Ordering::SeqCst);
    prompt(&h, "again").await;
    assert_eq!(cw(&h), 8192);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_size_that_stays_unknown_is_asked_at_three_turns_ends_and_no_more() {
    let Some(t) = child() else { return };
    let s = Servers::start();
    s.configure(&t, json!({}), "local/ghost", json!({}));
    let h = harness(&t).await;
    h.list_models(false).await;
    let hits = n(&s.ollama_state.list_hits);
    for i in 1..=5 {
        prompt(&h, &format!("turn {i}")).await;
    }
    assert_eq!(n(&s.ollama_state.list_hits) - hits, 3);
    assert_eq!(cw(&h), 128_000);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_context_window_set_in_model_overrides_is_kept_and_the_server_is_not_asked_again_for_it() {
    let Some(t) = child() else { return };
    let s = Servers::start();
    s.configure(
        &t,
        json!({ "modelOverrides": { "local/plain": { "contextWindow": 1234 } } }),
        "local/plain",
        json!({}),
    );
    let h = harness(&t).await;
    h.list_models(false).await;
    let hits = n(&s.ollama_state.ps_hits);
    prompt(&h, "hi").await;
    assert_eq!(cw(&h), 1234);
    assert_eq!(n(&s.ollama_state.ps_hits), hits);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lm_studio_a_model_not_loaded_yet_shows_its_maximum_after_its_first_reply_it_plans_with_the_loaded_length_59() {
    let Some(t) = child() else { return };
    let s = Servers::start();
    let studio = openai(format!("{}/v1", s.studio_url()), json!({ "lmstudio": true }));
    s.configure(&t, json!({}), "studio/qwen", json!({ "studio": studio }));
    let h = harness(&t).await;
    assert_eq!(
        listed(&h, "studio/qwen", false).await,
        Some(ModelChoice {
            spec: "studio/qwen".into(),
            context_window: None,
            max_context: Some(32768),
            server: Some("lmstudio".into()),
        })
    );
    // Not the default 128K: whatever LM Studio loads it with, it is no more than the maximum.
    assert_eq!(cw(&h), 32768);
    prompt(&h, "hi").await;
    assert_eq!(cw(&h), 4096);
    assert_eq!(
        listed(&h, "studio/qwen", false).await.and_then(|m| m.context_window),
        Some(4096)
    );
    // Unloaded later (LM Studio's idle TTL): the length it was loaded with stays the better guess.
    s.studio.loaded.store(false, Ordering::SeqCst);
    h.list_models(true).await;
    assert_eq!(cw(&h), 4096);
    h.shutdown().await;
}

/// Added before sasacode told LM Studio apart: `endpoint add` wrote { api, baseUrl } and no flag, and
/// a URL-only entry was detected and cached in endpoints.json the same way.
async fn lm_studio_set_up_before_it_was_told_apart(url_only: bool) {
    let Some(t) = child() else { return };
    let s = Servers::start();
    let url = s.studio_url();
    let studio = if url_only {
        json!({ "baseUrl": url, "apiKeyEnv": KEY_VAR })
    } else {
        openai(format!("{url}/v1"), json!({}))
    };
    s.configure(&t, json!({}), "studio/qwen", json!({ "studio": studio }));
    let cache = json!({ url.clone(): { "api": "openai-chat", "baseUrl": format!("{url}/v1") } });
    std::fs::write(t.home.join("endpoints.json"), cache.to_string()).unwrap();
    let h = harness(&t).await;
    let qwen = listed(&h, "studio/qwen", false).await.expect("studio/qwen listed");
    assert_eq!(
        (qwen.max_context, qwen.server.as_deref()),
        (Some(32768), Some("lmstudio"))
    );
    assert_eq!(n(&s.studio.native_hits), 1); // the list that told it apart is the one the sizes come from
    prompt(&h, "hi").await;
    assert_eq!(cw(&h), 4096);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lm_studio_set_up_before_it_was_told_apart_added_with_endpoint_add_is_recognised_when_listed_59() {
    lm_studio_set_up_before_it_was_told_apart(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lm_studio_set_up_before_it_was_told_apart_url_only_detected_and_cached_is_recognised_when_listed_59() {
    lm_studio_set_up_before_it_was_told_apart(true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lm_studio_set_up_before_it_was_told_apart_gets_max_tokens_from_its_first_request_once_listed_59() {
    let Some(t) = child() else { return };
    let s = Servers::start();
    let studio = openai(format!("{}/v1", s.studio_url()), json!({}));
    s.configure(&t, json!({}), "studio/big", json!({ "studio": studio }));
    let h = harness(&t).await;
    h.list_models(false).await;
    // Its maximum (131072) is above the default: no size is learned, but the server is.
    assert_eq!((server(&h), cw(&h)), (json!("lmstudio"), 128_000));
    prompt(&h, "hi").await;
    let body = s.studio.bodies.lock().unwrap()[0].clone();
    assert!(body.get("max_tokens").is_some(), "{body}");
    assert!(body.get("max_completion_tokens").is_none(), "{body}");
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lm_studio_set_up_before_it_was_told_apart_and_down_when_first_listed_is_recognised_at_a_turns_end_59() {
    let Some(t) = child() else { return };
    let s = Servers::start();
    let studio = openai(format!("{}/v1", s.studio_url()), json!({}));
    s.configure(&t, json!({}), "studio/qwen", json!({ "studio": studio }));
    s.studio.down.store(true, Ordering::SeqCst);
    let h = harness(&t).await;
    assert_eq!(listed(&h, "studio/qwen", false).await, None);
    s.studio.down.store(false, Ordering::SeqCst);
    prompt(&h, "hi").await;
    assert_eq!((cw(&h), server(&h)), (4096, json!("lmstudio")));
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_custom_server_not_told_apart_that_has_listed_the_model_is_not_listed_again_at_turns_ends() {
    let Some(t) = child() else { return };
    let s = Servers::start();
    let cpp = openai(format!("{}/v1", s.llamacpp.url()), json!({}));
    s.configure(&t, json!({}), "cpp/qwen", json!({ "cpp": cpp }));
    let h = harness(&t).await;
    let qwen = listed(&h, "cpp/qwen", false).await.expect("cpp/qwen listed");
    assert_eq!(qwen.context_window, None);
    let hits = n(&s.cpp_list_hits);
    prompt(&h, "hi").await;
    prompt(&h, "again").await;
    assert_eq!(n(&s.cpp_list_hits), hits);
    assert_eq!(server(&h), Value::Null);
    h.shutdown().await;
}

async fn a_custom_server_not_told_apart_whose_list(missing: bool) {
    let Some(t) = child() else { return };
    let s = Servers::start();
    let gw = openai(format!("{}/v1", s.gateway.url()), json!({}));
    s.configure(&t, json!({}), "gw/flash", json!({ "gw": gw }));
    s.gw.list_missing.store(missing, Ordering::SeqCst);
    let h = harness(&t).await;
    h.list_models(false).await;
    let hits = n(&s.gw.hits);
    prompt(&h, "hi").await; // it has just answered, so it is up: listed once more
    assert_eq!(n(&s.gw.hits) - hits, 2); // /api/v0/models and /v1/models
    for i in 2..=4 {
        prompt(&h, &format!("turn {i}")).await;
    }
    // Telling a server apart is the provider's: another of its models is not listed again either.
    h.agent.set_model(h.resolve("gw/flash-v3").unwrap()).unwrap();
    prompt(&h, "other model").await;
    assert_eq!(n(&s.gw.hits) - hits, 2);
    assert_eq!(server(&h), Value::Null);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_custom_server_not_told_apart_whose_list_leaves_out_the_model_is_listed_again_once_not_at_each_turn_59() {
    a_custom_server_not_told_apart_whose_list(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_custom_server_not_told_apart_whose_list_is_missing_is_listed_again_once_at_a_turns_end_not_at_each_59() {
    a_custom_server_not_told_apart_whose_list(true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn headless_lm_studio_set_up_before_it_was_told_apart_also_learns_the_loaded_length() {
    let Some(t) = child() else { return };
    let s = Servers::start();
    let studio = openai(format!("{}/v1", s.studio_url()), json!({}));
    s.configure(&t, json!({}), "studio/qwen", json!({ "studio": studio }));
    let h = harness(&t).await;
    headless(&h).await;
    assert_eq!(cw(&h), 4096);
}

#[tokio::test(flavor = "multi_thread")]
async fn headless_warned_on_stderr_like_the_tui_when_num_ctx_is_unset_then_the_real_size_is_used() {
    let t = match isolated() {
        Role::Child(t) => t,
        Role::Parent(out) => {
            assert!(
                out.stderr.contains("warning: local/plain has no num_ctx"),
                "{}",
                out.stderr
            );
            assert!(out.stderr.contains("this model goes up to 128k"), "{}", out.stderr);
            return;
        }
    };
    let s = Servers::start();
    s.configure(&t, json!({}), "local/plain", json!({}));
    let h = harness(&t).await;
    headless(&h).await;
    assert_eq!(cw(&h), 8192);
}

#[tokio::test(flavor = "multi_thread")]
async fn headless_a_model_with_num_ctx_plans_its_first_turn_with_that_size_and_gets_no_warning() {
    let t = match isolated() {
        Role::Child(t) => t,
        Role::Parent(out) => {
            assert!(!out.stderr.contains("num_ctx"), "{}", out.stderr);
            return;
        }
    };
    let s = Servers::start();
    s.configure(&t, json!({}), "local/small-32k", json!({}));
    let h = harness(&t).await;
    let sizes = turn_sizes(&h);
    headless(&h).await;
    assert_eq!(*sizes.lock().unwrap(), [32768]);
}

#[tokio::test(flavor = "multi_thread")]
async fn headless_llama_cpps_slot_size_from_props_is_used_from_the_first_turn() {
    let Some(t) = child() else { return };
    let s = Servers::start();
    let cpp = openai(format!("{}/v1", s.llamacpp.url()), json!({ "llamacpp": true }));
    s.configure(&t, json!({}), "cpp/qwen", json!({ "cpp": cpp }));
    let h = harness(&t).await;
    let sizes = turn_sizes(&h);
    headless(&h).await;
    assert_eq!(*sizes.lock().unwrap(), [16384]);
}
