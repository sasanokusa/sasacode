//! An Anthropic request with neither key nor `ANTHROPIC_AUTH_TOKEN` takes its token from the installed
//! credentials (the SDK's credential chain; the CLI installs the `ant auth login` profile): the bearer
//! header, the OAuth beta added to the request's own, the profile's headers where unset, a refresh and
//! one more try after a 401 (`@anthropic-ai/sdk` 0.128 `client.ts` `authHeaders`, `prepareRequest`,
//! `shouldRetry`, `validateHeaders`).
//!
//! The credentials and the environment are process-wide, so each test runs again alone in a child
//! process with the variables it needs (an inherited key would otherwise win over the chain).

mod common;

use std::process::Command;
use std::sync::{Arc, Mutex};

use common::*;
use futures::FutureExt;
use sasacode_ai::transport::{ApiClient, Bearer, Credentials, set_default_credentials};
use serde_json::json;
use tokio_util::sync::CancellationToken;

const CHILD: &str = "SASACODE_OAUTH_TEST_CHILD";

/// Run `name` again in a child process with `env` set and the Anthropic key variables removed;
/// true when this is that child.
fn in_child(name: &str, env: &[(&str, &str)]) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([name, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD, "1");
    for var in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_CUSTOM_HEADERS"] {
        cmd.env_remove(var);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(text.contains("1 passed"), "the child ran no test: {text}");
    false
}

/// What the credentials were asked: (base URL, force).
type Asked = Arc<Mutex<Vec<(String, bool)>>>;

fn install(answer: impl Fn(bool) -> anyhow::Result<Option<Bearer>> + Send + Sync + 'static) -> Asked {
    let asked: Asked = Arc::default();
    let log = asked.clone();
    let answer = Arc::new(answer);
    let credentials: Credentials = Arc::new(move |base: &str, force: bool| {
        log.lock().unwrap().push((base.to_string(), force));
        let answer = answer.clone();
        async move { answer(force) }.boxed()
    });
    set_default_credentials(Some(credentials));
    asked
}

fn bearer(token: &str) -> Bearer {
    Bearer {
        token: token.into(),
        headers: vec![("anthropic-workspace-id".into(), "wrkspc_1".into())],
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
}

#[test]
fn the_token_goes_as_a_bearer_with_the_oauth_beta_after_the_requests_own() {
    if !in_child(
        "the_token_goes_as_a_bearer_with_the_oauth_beta_after_the_requests_own",
        &[],
    ) {
        return;
    }
    runtime().block_on(async {
        let asked = install(|_| Ok(Some(bearer("tok-1"))));
        let (fetch, sent) = fixed(|| Reply::Status(400, "{}".into()));
        let headers = [("Anthropic-Beta", "model-beta"), ("anthropic-workspace-id", "mine")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into();
        let client = ApiClient::anthropic(None, "https://api.anthropic.com", 0, Some(&headers), Some(fetch));
        let signal = CancellationToken::new();
        // The adapter's binding beta, per request, replaces the model's; the OAuth beta is added after.
        let extra = [("anthropic-beta", "model-beta, thinking-binding-controls-2026-08-01")];
        let _ = client.post("/v1/messages", &json!({}), &extra, &signal).await;
        let s = sent.lock().unwrap()[0].clone();
        assert_eq!(s.header("authorization").as_deref(), Some("Bearer tok-1"));
        assert_eq!(s.header("x-api-key"), None);
        assert_eq!(
            s.header("anthropic-beta").as_deref(),
            Some("model-beta,thinking-binding-controls-2026-08-01,oauth-2025-04-20")
        );
        // The model's own header stays.
        assert_eq!(s.header("anthropic-workspace-id").as_deref(), Some("mine"));
        assert_eq!(s.header("anthropic-version").as_deref(), Some("2023-06-01"));
        assert_eq!(
            *asked.lock().unwrap(),
            [("https://api.anthropic.com".to_string(), false)]
        );

        // The Models API too; the profile's headers fill what is unset; a beta already there is not repeated.
        let (fetch, sent) = fixed(|| Reply::Status(200, r#"{"data":[]}"#.into()));
        let headers = [("anthropic-beta".to_string(), "oauth-2025-04-20".to_string())].into();
        let client = ApiClient::anthropic(None, "https://h.example", 0, Some(&headers), Some(fetch));
        client.get("/v1/models?limit=100", &signal).await.unwrap();
        let s = sent.lock().unwrap()[0].clone();
        assert_eq!(s.header("authorization").as_deref(), Some("Bearer tok-1"));
        assert_eq!(s.header("anthropic-beta").as_deref(), Some("oauth-2025-04-20"));
        assert_eq!(s.header("anthropic-workspace-id").as_deref(), Some("wrkspc_1"));
        assert_eq!(asked.lock().unwrap()[1], ("https://h.example".to_string(), false));
    });
}

#[test]
fn a_401_refreshes_the_token_and_tries_once_more() {
    if !in_child("a_401_refreshes_the_token_and_tries_once_more", &[]) {
        return;
    }
    runtime().block_on(async {
        // Like the profile: a forced refresh writes the new token, later reads give it.
        let refreshed = std::sync::atomic::AtomicBool::new(false);
        let asked = install(move |force| {
            let new = refreshed.fetch_or(force, std::sync::atomic::Ordering::SeqCst) || force;
            Ok(Some(bearer(if new { "tok-new" } else { "tok-old" })))
        });
        let unauthorized = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid token"}}"#;
        // 401, then (refreshed) 529, then (not forced again) 401: no second refresh.
        let replies = move |n: usize| match n {
            2 => Reply::Status(
                529,
                r#"{"type":"error","error":{"type":"overloaded_error","message":"x"}}"#.into(),
            ),
            _ => Reply::Status(401, unauthorized.into()),
        };
        let (fetch, sent) = fake(move |_, n| replies(n));
        let client = ApiClient::anthropic(None, "https://api.anthropic.com", 5, None, Some(fetch));
        let err = client
            .post("/v1/messages", &json!({}), &[], &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), format!("401 {unauthorized}"));
        let tokens: Vec<_> = sent.lock().unwrap().iter().map(|s| s.header("authorization")).collect();
        assert_eq!(
            tokens,
            ["Bearer tok-old", "Bearer tok-new", "Bearer tok-new"].map(|t| Some(t.to_string()))
        );
        let forced: Vec<bool> = asked.lock().unwrap().iter().map(|(_, f)| *f).collect();
        assert_eq!(forced, [false, true, false]);

        // Without retries left there is no second try.
        let (fetch, sent) = fixed(move || Reply::Status(401, unauthorized.into()));
        let client = ApiClient::anthropic(None, "https://api.anthropic.com", 0, None, Some(fetch));
        assert!(
            client
                .post("/v1/messages", &json!({}), &[], &CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(sent.lock().unwrap().len(), 1);
    });
}

#[test]
fn no_credentials_or_failing_ones_stop_the_request_before_it_is_sent() {
    if !in_child("no_credentials_or_failing_ones_stop_the_request_before_it_is_sent", &[]) {
        return;
    }
    const NO_AUTH: &str = "Could not resolve authentication method. Expected one of apiKey, authToken, credentials, config, or profile to be set. Or for one of the \"X-Api-Key\" or \"Authorization\" headers to be explicitly omitted";
    runtime().block_on(async {
        let signal = CancellationToken::new();
        let post = || async {
            let (fetch, sent) = fixed(|| Reply::Status(200, "{}".into()));
            let client = ApiClient::anthropic(None, "https://api.anthropic.com", 2, None, Some(fetch));
            let err = client.post("/v1/messages", &json!({}), &[], &signal).await.unwrap_err();
            (err.to_string(), sent.lock().unwrap().len())
        };
        // Nothing installed: as before.
        assert_eq!(post().await, (NO_AUTH.to_string(), 0));
        // Installed, but no profile.
        install(|_| Ok(None));
        assert_eq!(post().await, (NO_AUTH.to_string(), 0));
        // A profile that cannot give a token: its error.
        install(|_| anyhow::bail!("profile name \"a/b\" must not contain path separators"));
        assert_eq!(
            post().await,
            ("profile name \"a/b\" must not contain path separators".to_string(), 0)
        );
        set_default_credentials(None);
    });
}

#[test]
fn a_key_or_anthropic_auth_token_wins_over_the_credentials() {
    if !in_child(
        "a_key_or_anthropic_auth_token_wins_over_the_credentials",
        &[("ANTHROPIC_AUTH_TOKEN", " tok-env ")],
    ) {
        return;
    }
    runtime().block_on(async {
        let asked = install(|_| Ok(Some(bearer("tok-profile"))));
        let signal = CancellationToken::new();
        // ANTHROPIC_AUTH_TOKEN: a bearer token, without the OAuth beta.
        let (fetch, sent) = fixed(|| Reply::Status(400, "{}".into()));
        let client = ApiClient::anthropic(None, "https://api.anthropic.com", 0, None, Some(fetch));
        let _ = client.post("/v1/messages", &json!({}), &[], &signal).await;
        let s = sent.lock().unwrap()[0].clone();
        assert_eq!(s.header("authorization").as_deref(), Some("Bearer tok-env"));
        assert_eq!(s.header("anthropic-beta"), None);
        assert_eq!(s.header("x-api-key"), None);
        // A key from sasacode (env var or keychain): sent as x-api-key, next to the token, as the SDK did.
        let (fetch, sent) = fixed(|| Reply::Status(400, "{}".into()));
        let client = ApiClient::anthropic(Some("sk-key"), "https://api.anthropic.com", 0, None, Some(fetch));
        let _ = client.post("/v1/messages", &json!({}), &[], &signal).await;
        let s = sent.lock().unwrap()[0].clone();
        assert_eq!(s.header("x-api-key").as_deref(), Some("sk-key"));
        assert_eq!(s.header("authorization").as_deref(), Some("Bearer tok-env"));
        assert!(asked.lock().unwrap().is_empty(), "the credentials were not asked");
    });
}
