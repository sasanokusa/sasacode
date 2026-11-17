//! packages/bundled/test/openai-codex.test.ts: credentials, the provider, the client version and
//! login, with an injected Fetch (the network is never reached) and files in a temp dir.

mod support_net;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine;
use futures::StreamExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use sasacode_ai::{
    Fetch, Message, ModelInfo, Provider, ProviderConfig, Request, SamplingOptions, StreamEvent, ToolSpec, UserContent,
    UserMessage,
};
use sasacode_bundled::openai_codex::auth::{
    account_id, clear_codex_auth, current_codex_tokens, plan_type, write_codex_auth,
};
use sasacode_bundled::openai_codex::login::{authorize_url, parse_callback};
use sasacode_bundled::openai_codex::provider::{CODEX_CLIENT_VERSION, FALLBACK_MODELS, latest_codex_version};
use sasacode_bundled::openai_codex::{
    CodexProvider, CodexTokens, LoginDeps, ProviderDeps, codex_auth_path_in, codex_login_with, codex_provider_with,
    read_codex_auth_at,
};
use support_net::*;

// ── fakes ───────────────────────────────────────────────────────────────────

type Handler = dyn Fn(reqwest::Request) -> anyhow::Result<reqwest::Response> + Send + Sync;

/// A fetch answered by a closure (`fetch` replaced in the TS tests).
struct FnFetch(Box<Handler>);

impl Fetch for FnFetch {
    fn send(&self, req: reqwest::Request) -> BoxFuture<'static, anyhow::Result<reqwest::Response>> {
        let r = (self.0)(req);
        Box::pin(async move { r })
    }
}

fn fetch(f: impl Fn(reqwest::Request) -> anyhow::Result<reqwest::Response> + Send + Sync + 'static) -> Arc<dyn Fetch> {
    Arc::new(FnFetch(Box::new(f)))
}

fn respond(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> reqwest::Response {
    let r = http::Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(body.into());
    reqwest::Response::from(r.unwrap_or_else(|e| panic!("response: {e}")))
}

fn json_res(body: Value, status: u16) -> anyhow::Result<reqwest::Response> {
    Ok(respond(status, "application/json", body.to_string()))
}

fn body_of(req: &reqwest::Request) -> String {
    req.body()
        .and_then(|b| b.as_bytes())
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default()
}

fn form(body: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(body.as_bytes()).into_owned().collect()
}

fn get(form: &[(String, String)], key: &str) -> Option<String> {
    form.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

static N: AtomicUsize = AtomicUsize::new(0);

fn file(dir: &tempfile::TempDir) -> PathBuf {
    dir.path()
        .join(format!("auth-{}.json", N.fetch_add(1, Ordering::Relaxed) + 1))
}

/// Unsigned JWTs with just the claims we read (the server verifies them, we do not).
fn jwt(claims: Value) -> String {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("x.{b64}.x")
}

const AUTH: &str = "https://api.openai.com/auth";

fn now_ms() -> f64 {
    sasacode_ai::now_ms() as f64
}

fn access(secs: i64, account: &str) -> String {
    let exp = (now_ms() / 1000.0).floor() as i64 + secs;
    jwt(json!({ "exp": exp, AUTH: { "chatgpt_account_id": account } }))
}

fn tokens(secs: i64) -> CodexTokens {
    CodexTokens {
        access: access(secs, "acct-1"),
        refresh: "rt-old".into(),
        id_token: Some(jwt(
            json!({ AUTH: { "chatgpt_account_id": "acct-1", "chatgpt_plan_type": "pro" } }),
        )),
        account_id: Some("acct-1".into()),
        expires_at: None,
    }
}

/// tokens() as the file holds them: writeCodexAuth is given what the TS gave it.
fn write(t: &CodexTokens, path: &std::path::Path) {
    write_codex_auth(t, path).unwrap_or_else(|e| panic!("write: {e}"));
}

// ── credentials ─────────────────────────────────────────────────────────────

#[test]
fn credentials_live_in_the_sasacode_home_readable_only_by_the_user() {
    let dir = temp_dir("sasacode-codex-");
    assert_eq!(
        codex_auth_path_in(std::path::Path::new("/h")),
        std::path::Path::new("/h").join("codex-auth.json")
    );
    let path = file(&dir);
    write(&tokens(3600), &path);
    // Unix permission bits; on Windows the file is private through the ACL of the user profile it is in.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).map(|m| m.permissions().mode()).unwrap_or(0);
        assert_eq!(mode & 0o777, 0o600);
    }
    let read = read_codex_auth_at(&path).unwrap_or_else(|| panic!("no tokens"));
    assert_eq!(read.refresh, "rt-old");
    assert_eq!(read.account_id.as_deref(), Some("acct-1"));
    assert_eq!(plan_type(read.id_token.as_deref()).as_deref(), Some("pro"));
    assert!(clear_codex_auth(&path));
    assert!(read_codex_auth_at(&path).is_none());
    assert!(!clear_codex_auth(&path));
}

#[test]
fn the_account_id_is_recovered_from_the_tokens_and_junk_is_not_fatal() {
    let dir = temp_dir("sasacode-codex-");
    let path = file(&dir);
    let stored = json!({ "access": access(3600, "acct-from-access"), "refresh": "r" });
    std::fs::write(&path, stored.to_string()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        read_codex_auth_at(&path).and_then(|t| t.account_id).as_deref(),
        Some("acct-from-access")
    );
    assert_eq!(account_id(Some("not a jwt")), None);
    std::fs::write(&path, "{ torn").unwrap_or_else(|e| panic!("{e}"));
    assert!(read_codex_auth_at(&path).is_none());
}

#[tokio::test]
async fn a_token_with_time_left_is_used_as_is_one_about_to_expire_is_refreshed_form_encoded_and_saved() {
    let dir = temp_dir("sasacode-codex-");
    let path = file(&dir);
    write(&tokens(3600), &path);
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let no_fetch = fetch(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
        json_res(json!({}), 200)
    });
    let t = current_codex_tokens(&path, Some(&no_fetch), now_ms())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(t.map(|t| t.refresh).as_deref(), Some("rt-old"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    write(&tokens(60), &path); // inside the 5 minute window
    let sent: Arc<Mutex<(Option<String>, String)>> = Arc::default();
    let s2 = sent.clone();
    let refresh = fetch(move |req| {
        let ct = req
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        *s2.lock().unwrap_or_else(|e| e.into_inner()) = (ct, body_of(&req));
        json_res(
            json!({ "access_token": access(3600, "acct-1"), "refresh_token": "rt-new" }),
            200,
        )
    });
    let next = current_codex_tokens(&path, Some(&refresh), now_ms())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let (ct, body) = sent.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(ct.as_deref(), Some("application/x-www-form-urlencoded"));
    assert_eq!(get(&form(&body), "grant_type").as_deref(), Some("refresh_token"));
    assert_eq!(next.map(|t| t.refresh).as_deref(), Some("rt-new"));
    assert_eq!(read_codex_auth_at(&path).map(|t| t.refresh).as_deref(), Some("rt-new"));
}

#[tokio::test]
async fn a_failed_refresh_keeps_a_token_that_still_works_and_fails_once_it_has_expired() {
    let dir = temp_dir("sasacode-codex-");
    let path = file(&dir);
    let down = fetch(|_| json_res(json!({ "error": "server" }), 500));
    write(&tokens(60), &path);
    let t = current_codex_tokens(&path, Some(&down), now_ms())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(t.map(|t| t.refresh).as_deref(), Some("rt-old"));
    write(&tokens(-60), &path);
    let e = current_codex_tokens(&path, Some(&down), now_ms())
        .await
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(e.contains("refresh failed (500)"), "{e}");
}

// ── provider ────────────────────────────────────────────────────────────────

fn model() -> ModelInfo {
    serde_json::from_value(json!({
        "id": "gpt-5.5", "provider": "openai-codex", "api": "openai-codex",
        "baseUrl": "https://chatgpt.com/backend-api/codex", "contextWindow": 400_000, "maxOutput": 64_000,
    }))
    .unwrap_or_else(|e| panic!("model: {e}"))
}

fn config() -> ProviderConfig {
    ProviderConfig {
        api: "openai-codex".into(),
        base_url: Some("https://chatgpt.com/backend-api/codex".into()),
        ..ProviderConfig::default()
    }
}

#[derive(Default, Clone)]
struct Seen {
    url: String,
    headers: reqwest::header::HeaderMap,
    body: Value,
}

/// Answers with a scripted Responses stream and keeps what was sent.
fn sse(events: Vec<Value>, seen: Arc<Mutex<Option<Seen>>>) -> Arc<dyn Fetch> {
    fetch(move |req| {
        let body = serde_json::from_str(&body_of(&req)).unwrap_or(Value::Null);
        *seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(Seen {
            url: req.url().to_string(),
            headers: req.headers().clone(),
            body,
        });
        let text: String = events
            .iter()
            .map(|e| {
                format!(
                    "event: {}\ndata: {e}\n\n",
                    e.get("type").and_then(Value::as_str).unwrap_or("")
                )
            })
            .collect();
        Ok(respond(200, "text/event-stream", text))
    })
}

fn reply_events() -> Vec<Value> {
    vec![
        json!({ "type": "response.output_item.added", "item": { "type": "message", "id": "m1" } }),
        json!({ "type": "response.output_text.delta", "item_id": "m1", "delta": "Hi" }),
        json!({ "type": "response.output_item.done", "item": { "type": "message", "id": "m1" } }),
        json!({ "type": "response.completed", "response": { "usage": { "input_tokens": 100, "output_tokens": 7, "input_tokens_details": { "cached_tokens": 40 } } } }),
    ]
}

fn session(id: &'static str) -> sasacode_bundled::openai_codex::provider::SessionIdFn {
    Arc::new(move || id.to_string())
}

fn deps(t: Option<CodexTokens>, f: Option<Arc<dyn Fetch>>) -> ProviderDeps {
    ProviderDeps {
        tokens: Some(Arc::new(move || {
            let t = t.clone();
            Box::pin(async move { Ok(t) })
        })),
        fetch: f,
        // Never npm: tests stay offline.
        version: Some(Arc::new(|| Box::pin(async { "9.0.0".to_string() }))),
    }
}

#[derive(Default)]
struct Extra {
    tools: Vec<ToolSpec>,
    max_retries: Option<u32>,
    fetch: Option<Arc<dyn Fetch>>,
}

async fn run(provider: &CodexProvider, extra: Extra) -> anyhow::Result<Vec<StreamEvent>> {
    let req = Request {
        model: model(),
        api_key: None,
        system: "be brief".into(),
        messages: vec![Message::User(UserMessage {
            content: vec![UserContent::text("hello")],
            timestamp: 0,
        })],
        tools: extra.tools,
        thinking: None,
        max_tokens: Some(1000),
        max_retries: extra.max_retries,
        sampling: Some(SamplingOptions {
            temperature: Some(0.2),
            ..SamplingOptions::default()
        }),
        signal: CancellationToken::new(),
        fetch: extra.fetch,
    };
    let mut out = Vec::new();
    let mut stream = provider.stream(req);
    while let Some(e) = stream.next().await {
        out.push(e?);
    }
    Ok(out)
}

#[tokio::test]
async fn requests_go_to_the_chatgpt_backend_with_the_plans_token_and_headers_minus_the_fields_it_rejects() {
    let seen: Arc<Mutex<Option<Seen>>> = Arc::default();
    let t = tokens(3600);
    let provider = codex_provider_with(
        session("sess-1"),
        deps(Some(t.clone()), Some(sse(reply_events(), seen.clone()))),
    );
    let tools = vec![ToolSpec {
        name: "read".into(),
        description: String::new(),
        parameters: json!({ "type": "object" }),
    }];
    let events = run(
        &provider,
        Extra {
            tools,
            ..Extra::default()
        },
    )
    .await
    .unwrap_or_else(|e| panic!("{e:#}"));
    let seen = seen
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| panic!("nothing sent"));
    assert_eq!(seen.url, "https://chatgpt.com/backend-api/codex/responses");
    let h = |k: &str| seen.headers.get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
    assert_eq!(h("authorization"), Some(format!("Bearer {}", t.access)));
    assert_eq!(h("chatgpt-account-id").as_deref(), Some("acct-1"));
    assert_eq!(h("openai-beta").as_deref(), Some("responses=experimental"));
    assert_eq!(h("session-id").as_deref(), Some("sess-1"));
    assert!(h("version").is_some_and(|v| !v.is_empty()));
    let b = &seen.body;
    assert_eq!(b["model"], "gpt-5.5");
    assert_eq!(b["instructions"], "be brief");
    assert_eq!(b["store"], false);
    assert_eq!(b["stream"], true);
    assert_eq!(b["prompt_cache_key"], "sess-1");
    assert!(b.get("max_output_tokens").is_none());
    assert!(b.get("temperature").is_none());
    assert_eq!(b["tools"][0]["type"], "function");
    assert_eq!(b["tools"][0]["name"], "read");
    let Some(StreamEvent::Done { message }) = events.last() else {
        panic!("no done event")
    };
    assert_eq!(message.api, "openai-codex");
    assert_eq!(message.content.len(), 1);
    assert_eq!(message.content[0].as_text(), Some("Hi"));
    assert_eq!(
        (message.usage.input, message.usage.cache_read, message.usage.output),
        (60, 40, 7)
    );
}

#[tokio::test]
async fn not_signed_in_or_signed_out_on_the_server_the_user_is_told_to_run_sasacode_login() {
    let e = run(&codex_provider_with(session("s"), deps(None, None)), Extra::default()).await;
    let e = e.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(e.contains("sasacode login"), "{e}");
    let rejected = fetch(|_| json_res(json!({ "error": { "message": "token expired" } }), 401));
    let provider = codex_provider_with(session("s"), deps(Some(tokens(3600)), Some(rejected)));
    let e = run(
        &provider,
        Extra {
            max_retries: Some(0),
            ..Extra::default()
        },
    )
    .await;
    let e = e.err().map(|e| e.to_string()).unwrap_or_default();
    let re = regex::Regex::new(r"rejected the login \(401\).*sasacode login").unwrap_or_else(|e| panic!("{e}"));
    assert!(re.is_match(&e), "{e}");
}

async fn list(provider: &CodexProvider) -> Vec<sasacode_ai::ListedModel> {
    let r = provider.list_models(&config(), None, CancellationToken::new()).await;
    r.unwrap_or_else(|| panic!("no listing"))
        .unwrap_or_else(|e| panic!("{e}"))
}

#[tokio::test]
async fn models_come_from_the_backend_when_signed_in_none_are_offered_when_signed_out() {
    let listing = fetch(|req| {
        if req.url().as_str().contains("/models?client_version=") {
            json_res(
                json!({ "models": [{ "slug": "gpt-5.6-sol", "context_window": 1_050_000 }, { "slug": "codex-auto-review", "visibility": "hide" }] }),
                200,
            )
        } else {
            json_res(json!({}), 500)
        }
    });
    let signed_in = codex_provider_with(session("s"), deps(Some(tokens(3600)), Some(listing)));
    let models = list(&signed_in).await;
    assert_eq!(models.len(), 1);
    assert_eq!(
        (models[0].id.as_str(), models[0].context_window),
        ("gpt-5.6-sol", Some(1_050_000))
    );
    assert_eq!((models[0].max_output, models[0].price.is_none()), (None, true));
    let down = codex_provider_with(
        session("s"),
        deps(Some(tokens(3600)), Some(fetch(|_| json_res(json!({}), 503)))),
    );
    let ids: Vec<String> = list(&down).await.into_iter().map(|m| m.id).collect();
    assert_eq!(ids, FALLBACK_MODELS);
    assert!(
        list(&codex_provider_with(session("s"), deps(None, None)))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn the_client_version_is_npms_latest_codex_release_else_the_last_one_seen_there_else_the_built_in_one() {
    let dir = temp_dir("sasacode-codex-");
    let tags = |body: Value, status: u16| {
        fetch(move |req| {
            if req.url().as_str().ends_with("/-/package/@openai/codex/dist-tags") {
                json_res(body.clone(), status)
            } else {
                json_res(json!({}), 404)
            }
        })
    };
    let offline = fetch(|_| anyhow::bail!("offline"));
    let saved = dir.path().join("codex-version");
    assert_eq!(latest_codex_version(Some(&offline), &saved).await, CODEX_CLIENT_VERSION); // never answered yet
    let latest = tags(json!({ "latest": "9.0.0", "alpha": "9.1.0-alpha.1" }), 200);
    assert_eq!(latest_codex_version(Some(&latest), &saved).await, "9.0.0");
    assert_eq!(latest_codex_version(Some(&offline), &saved).await, "9.0.0"); // remembered
    assert_eq!(latest_codex_version(Some(&tags(json!({}), 500)), &saved).await, "9.0.0");
    assert_eq!(
        latest_codex_version(Some(&tags(json!({ "latest": "1.0.0-beta" }), 200)), &saved).await,
        "9.0.0"
    ); // not a release
    assert_eq!(
        latest_codex_version(Some(&tags(json!({ "latest": "9.0.1" }), 200)), &saved).await,
        "9.0.1"
    );
    assert_eq!(latest_codex_version(Some(&offline), &saved).await, "9.0.1");

    let urls: Arc<Mutex<Vec<String>>> = Arc::default();
    let u = urls.clone();
    let listing = fetch(move |req| {
        u.lock().unwrap_or_else(|e| e.into_inner()).push(req.url().to_string());
        json_res(json!({ "models": [] }), 200)
    });
    list(&codex_provider_with(
        session("s"),
        deps(Some(tokens(3600)), Some(listing)),
    ))
    .await;
    let first = urls
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .first()
        .cloned()
        .unwrap_or_default();
    assert!(first.ends_with("/models?client_version=9.0.0"), "{first}");
}

// ── login ───────────────────────────────────────────────────────────────────

#[test]
fn the_sign_in_url_uses_pkce_and_the_redirect_registered_for_the_codex_client() {
    let url = url::Url::parse(&authorize_url("challenge", "state-1")).unwrap_or_else(|e| panic!("{e}"));
    let q = |k: &str| url.query_pairs().find(|(key, _)| key == k).map(|(_, v)| v.into_owned());
    assert_eq!(
        q("redirect_uri").as_deref(),
        Some("http://localhost:1455/auth/callback")
    );
    assert_eq!(q("code_challenge_method").as_deref(), Some("S256"));
    assert_eq!(q("scope").as_deref(), Some("openid profile email offline_access"));
}

#[test]
fn a_pasted_code_or_redirect_url_is_accepted_one_from_another_attempt_is_not() {
    let p = |input: &str| parse_callback(input, "s").map_err(|e| e.to_string());
    assert_eq!(p("abc#xyz"), Ok(Some("abc".into())));
    assert_eq!(
        p("http://localhost:1455/auth/callback?code=c1&state=s"),
        Ok(Some("c1".into()))
    );
    let e = p("http://localhost:1455/auth/callback?code=c1&state=other")
        .err()
        .unwrap_or_default();
    assert!(e.contains("state mismatch"), "{e}");
    assert_eq!(p("  "), Ok(None));
}

fn prompt(answer: &'static str) -> sasacode_bundled::openai_codex::login::PromptFn {
    Arc::new(move |_, _| Box::pin(async move { Ok(answer.to_string()) }))
}

#[tokio::test]
async fn login_exchanges_the_code_with_its_pkce_verifier_and_stores_the_tokens() {
    let dir = temp_dir("sasacode-codex-");
    let path = file(&dir);
    let sent: Arc<Mutex<String>> = Arc::default();
    let s2 = sent.clone();
    let exchange = fetch(move |req| {
        *s2.lock().unwrap_or_else(|e| e.into_inner()) = body_of(&req);
        json_res(
            json!({ "access_token": access(3600, "acct-login"), "refresh_token": "rt-login", "id_token": jwt(json!({ AUTH: { "chatgpt_plan_type": "plus" } })) }),
            200,
        )
    });
    let challenge: Arc<Mutex<String>> = Arc::default();
    let c2 = challenge.clone();
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let l2 = lines.clone();
    let deps = LoginDeps {
        no_server: true,
        path: Some(path.clone()),
        fetch: Some(exchange),
        open_browser: Some(Arc::new(move |u: &str| {
            let url = url::Url::parse(u).unwrap_or_else(|e| panic!("{e}"));
            let c = url
                .query_pairs()
                .find(|(k, _)| k == "code_challenge")
                .map(|(_, v)| v.into_owned());
            *c2.lock().unwrap_or_else(|e| e.into_inner()) = c.unwrap_or_default();
        })),
        prompt: Some(prompt("the-code")),
        out: Some(Arc::new(move |l: &str| {
            l2.lock().unwrap_or_else(|e| e.into_inner()).push(l.to_string())
        })),
    };
    codex_login_with(deps).await.unwrap_or_else(|e| panic!("{e:#}"));
    let form = form(&sent.lock().unwrap_or_else(|e| e.into_inner()));
    assert_eq!(get(&form, "grant_type").as_deref(), Some("authorization_code"));
    assert_eq!(get(&form, "code").as_deref(), Some("the-code"));
    // The verifier sent now hashes to the challenge sent to the browser.
    let verifier = get(&form, "code_verifier").unwrap_or_default();
    let hashed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    assert_eq!(hashed, *challenge.lock().unwrap_or_else(|e| e.into_inner()));
    let stored = read_codex_auth_at(&path).unwrap_or_else(|| panic!("nothing stored"));
    assert_eq!(
        (stored.refresh.as_str(), stored.account_id.as_deref()),
        ("rt-login", Some("acct-login"))
    );
    assert!(
        lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .join("\n")
            .contains("ChatGPT plus")
    );
}

#[tokio::test]
async fn an_empty_paste_cancels_and_a_failed_exchange_says_why() {
    let dir = temp_dir("sasacode-codex-");
    let base = LoginDeps {
        no_server: true,
        path: Some(file(&dir)),
        open_browser: Some(Arc::new(|_: &str| {})),
        out: Some(Arc::new(|_: &str| {})),
        ..LoginDeps::default()
    };
    let e = codex_login_with(LoginDeps {
        prompt: Some(prompt("")),
        ..base.clone()
    })
    .await;
    let e = e.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(e.contains("cancelled"), "{e}");
    let refused = fetch(|_| Ok(respond(400, "text/plain", "bad code")));
    let e = codex_login_with(LoginDeps {
        prompt: Some(prompt("c")),
        fetch: Some(refused),
        ..base
    })
    .await;
    let e = e.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(e.contains("returned 400"), "{e}");
}

#[tokio::test]
async fn codex_delegates_its_adapted_wire_body_to_the_per_request_fetch_hook() {
    let seen: Arc<Mutex<Option<Seen>>> = Arc::default();
    let unused = Arc::new(AtomicUsize::new(0));
    let u = unused.clone();
    let own = fetch(move |_| {
        u.fetch_add(1, Ordering::SeqCst);
        json_res(json!({}), 500)
    });
    let provider = codex_provider_with(session("observed-session"), deps(Some(tokens(3600)), Some(own)));
    run(
        &provider,
        Extra {
            fetch: Some(sse(reply_events(), seen.clone())),
            ..Extra::default()
        },
    )
    .await
    .unwrap_or_else(|e| panic!("{e:#}"));
    assert_eq!(unused.load(Ordering::SeqCst), 0);
    let seen = seen
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| panic!("nothing sent"));
    assert_eq!(seen.body["prompt_cache_key"], "observed-session");
    assert!(seen.body.get("max_output_tokens").is_none());
}
