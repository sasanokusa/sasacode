//! The `ant auth login` profile, against the SDK sources (`core/credentials.ts`,
//! `lib/credentials/user-oauth.ts`, `types.ts`). Every test has its own config directory, passed in an
//! [`Env`] built by hand: the real `~/.config/anthropic` is never read.

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;
use crate::list_models::test_server::{Res, TestServer};

fn dir_env(dir: &Path) -> Env {
    Env::default().with("ANTHROPIC_CONFIG_DIR", dir.to_string_lossy())
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A credentials file only its owner can read, as `ant auth login` writes it.
fn write_creds(path: &Path, v: &Value) {
    write(path, &v.to_string());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn config(dir: &Path, profile: &str, v: Value) {
    write(&dir.join("configs").join(format!("{profile}.json")), &v.to_string());
}

fn oauth(extra: Value) -> Value {
    let mut c = json!({ "authentication": { "type": "user_oauth", "client_id": "cid" } });
    if let (Some(c), Some(e)) = (c.as_object_mut(), extra.as_object()) {
        c.extend(e.clone());
    }
    c
}

fn now() -> i64 {
    now_secs() as i64
}

/// What the token endpoint received.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

fn token_server(status: u16, reply: Value) -> (TestServer, Arc<Mutex<Vec<Seen>>>) {
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let log = seen.clone();
    let server = TestServer::start(move |req| {
        log.lock().unwrap().push(Seen {
            method: req.method.clone(),
            path: req.path.clone(),
            headers: req.headers.clone(),
            body: req.body.clone(),
        });
        let mut res = Res::json(reply.clone());
        res.status = status;
        res
    });
    (server, seen)
}

fn header<'a>(s: &'a Seen, name: &str) -> Option<&'a str> {
    s.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

#[test]
fn the_config_directory_is_anthropic_config_dir_then_xdg_then_home() {
    let all = Env::default()
        .with("ANTHROPIC_CONFIG_DIR", "/c")
        .with("XDG_CONFIG_HOME", "/x")
        .with("HOME", "/h");
    assert_eq!(config_dir(&all), Some("/c".into()));
    if cfg!(windows) {
        return;
    }
    let all = all.with("ANTHROPIC_CONFIG_DIR", "  ");
    assert_eq!(config_dir(&all), Some(Path::new("/x").join("anthropic")));
    let home = Env::default().with("HOME", "/h");
    assert_eq!(
        config_dir(&home),
        Some(Path::new("/h").join(".config").join("anthropic"))
    );
    assert_eq!(config_dir(&Env::default()), None);
}

#[test]
fn the_profile_is_anthropic_profile_then_active_config_then_default() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    for name in ["default", "work", "other"] {
        config(
            dir,
            name,
            oauth(json!({ "base_url": format!("https://{name}.example") })),
        );
    }
    let base = |env: &Env| load(env).unwrap().unwrap().base_url.unwrap();
    let path = |env: &Env| load(env).unwrap().unwrap().credentials_path;
    assert_eq!(base(&dir_env(dir)), "https://default.example");
    assert_eq!(path(&dir_env(dir)), dir.join("credentials").join("default.json"));

    write(&dir.join("active_config"), "  other\n");
    assert_eq!(base(&dir_env(dir)), "https://other.example");
    assert_eq!(path(&dir_env(dir)), dir.join("credentials").join("other.json"));

    let env = dir_env(dir).with("ANTHROPIC_PROFILE", "work");
    assert_eq!(base(&env), "https://work.example");
    assert_eq!(path(&env), dir.join("credentials").join("work.json"));

    // An empty active_config is the default profile.
    write(&dir.join("active_config"), "\n");
    assert_eq!(base(&dir_env(dir)), "https://default.example");

    // The config's credentials_path wins over the per-profile file.
    config(
        dir,
        "default",
        json!({ "authentication": { "type": "user_oauth", "credentials_path": "/elsewhere/c.json" } }),
    );
    assert_eq!(path(&dir_env(dir)), Path::new("/elsewhere/c.json"));
}

#[test]
fn an_invalid_profile_name_is_refused_with_the_sdks_words() {
    let tmp = TempDir::new().unwrap();
    let err = |name: &str| {
        load(&dir_env(tmp.path()).with("ANTHROPIC_PROFILE", name))
            .unwrap_err()
            .to_string()
    };
    assert_eq!(err("../x"), "profile name \"../x\" must not contain path separators");
    assert_eq!(err("a\\b"), "profile name \"a\\b\" must not contain path separators");
    assert_eq!(err(".."), "profile name \"..\" is not allowed");
    assert_eq!(
        err("a b"),
        "profile name \"a b\" contains disallowed characters (allowed: letters, digits, '_', '.', '-')"
    );
    assert_eq!(
        validate_profile_name("").unwrap_err().to_string(),
        "profile name is empty"
    );
    assert!(validate_profile_name("Work_2.prod-x").is_ok());
    // From active_config too.
    write(&tmp.path().join("active_config"), "a/b");
    assert_eq!(
        load(&dir_env(tmp.path())).unwrap_err().to_string(),
        "profile name \"a/b\" must not contain path separators"
    );
}

#[tokio::test]
async fn without_a_config_directory_there_is_no_profile() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("nothing-here");
    assert_eq!(load(&dir_env(&missing)).unwrap(), None);
    assert!(
        bearer(&dir_env(&missing), "https://api.anthropic.com", false)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        load(&Env::default().with("HOME", missing.to_string_lossy())).unwrap(),
        None
    );
    // A directory without configs/<profile>.json is no profile either.
    write(&tmp.path().join("credentials").join("default.json"), "{}");
    assert_eq!(load(&dir_env(tmp.path())).unwrap(), None);
}

#[test]
fn a_config_the_sdk_could_not_use_is_an_error() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let shown = dir.join("configs").join("default.json").display().to_string();
    let err = || load(&dir_env(dir)).unwrap_err().to_string();
    config(dir, "default", json!({ "base_url": "x" }));
    assert_eq!(err(), format!("config file {shown} is missing \"authentication\""));
    config(dir, "default", json!({ "authentication": { "type": "magic" } }));
    assert_eq!(
        err(),
        "authentication.type \"magic\" is not a known authentication type"
    );
    config(dir, "default", json!({ "authentication": {} }));
    assert_eq!(
        err(),
        "authentication.type \"undefined\" is not a known authentication type"
    );
    write(&dir.join("configs").join("default.json"), "{not json");
    assert!(err().starts_with(&format!("failed to parse config file {shown}: SyntaxError: ")));
    // Workload identity is not ported, from a file or from the environment alone.
    config(
        dir,
        "default",
        json!({ "authentication": { "type": "oidc_federation", "federation_rule_id": "fdrl_x" } }),
    );
    assert_eq!(
        err(),
        "authentication.type \"oidc_federation\" is not a known authentication type"
    );
    let empty = TempDir::new().unwrap();
    let env = dir_env(empty.path())
        .with("ANTHROPIC_FEDERATION_RULE_ID", "fdrl_x")
        .with("ANTHROPIC_ORGANIZATION_ID", "org");
    assert_eq!(
        load(&env).unwrap_err().to_string(),
        "authentication.type \"oidc_federation\" is not a known authentication type"
    );
    assert_eq!(
        load(&dir_env(empty.path()).with("ANTHROPIC_ORGANIZATION_ID", "org")).unwrap(),
        None
    );
}

#[test]
fn environment_variables_fill_only_what_the_file_leaves_out() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let env = dir_env(dir)
        .with("ANTHROPIC_BASE_URL", "https://env.example")
        .with("ANTHROPIC_WORKSPACE_ID", "wrkspc_env");
    config(dir, "default", oauth(json!({})));
    let p = load(&env).unwrap().unwrap();
    assert_eq!(p.base_url.as_deref(), Some("https://env.example"));
    assert_eq!(
        p.headers(),
        [("anthropic-workspace-id".to_string(), "wrkspc_env".to_string())]
    );
    config(
        dir,
        "default",
        oauth(json!({ "base_url": "https://file.example", "workspace_id": "" })),
    );
    let p = load(&env).unwrap().unwrap();
    assert_eq!(p.base_url.as_deref(), Some("https://file.example"));
    // An empty workspace_id in the file stays empty: no header.
    assert!(p.headers().is_empty());
}

#[tokio::test]
async fn a_fresh_token_is_used_as_it_is() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let (server, seen) = token_server(200, json!({ "access_token": "never", "expires_in": 60 }));
    config(
        dir,
        "default",
        oauth(json!({ "base_url": server.url(), "workspace_id": "wrkspc_1" })),
    );
    let creds = dir.join("credentials").join("default.json");
    let stored =
        json!({ "type": "oauth_token", "access_token": "at-1", "refresh_token": "rt-1", "expires_at": now() + 3600 });
    write_creds(&creds, &stored);
    let b = bearer(&dir_env(dir), "https://api.anthropic.com", false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(b.token, "at-1");
    assert_eq!(
        b.headers,
        [("anthropic-workspace-id".to_string(), "wrkspc_1".to_string())]
    );
    // No expiry at all: fresh for ever.
    write_creds(&creds, &json!({ "access_token": "at-static" }));
    let p = load(&dir_env(dir)).unwrap().unwrap();
    assert_eq!(p.token("https://api.anthropic.com", false).await.unwrap(), "at-static");
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(&creds).unwrap(),
        json!({ "access_token": "at-static" }).to_string()
    );
}

#[tokio::test]
async fn an_expiring_token_is_refreshed_and_written_back() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let (server, seen) = token_server(
        200,
        json!({ "access_token": "at-2", "expires_in": 3600, "refresh_token": "rt-2", "token_type": "Bearer" }),
    );
    // Trailing slashes come off the profile's host.
    config(
        dir,
        "default",
        oauth(json!({ "base_url": format!("{}//", server.url()) })),
    );
    let creds = dir.join("credentials").join("default.json");
    // 20 s left: inside the 30 s margin.
    write_creds(
        &creds,
        &json!({ "version": "1.0", "type": "oauth_token", "access_token": "at-1", "expires_at": now() + 20,
                 "refresh_token": "rt-1", "scope": "user:inference", "account_email": "a@example.com" }),
    );
    let before = now();
    let b = bearer(&dir_env(dir), "https://api.anthropic.com", false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(b.token, "at-2");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let s = &seen[0];
    assert_eq!((s.method.as_str(), s.path.as_str()), ("POST", "/v1/oauth/token"));
    assert_eq!(header(s, "content-type"), Some("application/json"));
    assert_eq!(header(s, "anthropic-beta"), Some("oauth-2025-04-20"));
    assert_eq!(header(s, "user-agent"), Some(USER_AGENT));
    assert_eq!(
        s.body,
        r#"{"grant_type":"refresh_token","refresh_token":"rt-1","client_id":"cid"}"#
    );

    let text = std::fs::read_to_string(&creds).unwrap();
    let written: Value = serde_json::from_str(&text).unwrap();
    let keys: Vec<&str> = written.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "version",
            "type",
            "access_token",
            "expires_at",
            "refresh_token",
            "scope",
            "account_email"
        ]
    );
    assert_eq!(written["access_token"], "at-2");
    assert_eq!(written["refresh_token"], "rt-2");
    assert_eq!(written["account_email"], "a@example.com");
    let at = written["expires_at"].as_i64().expect("whole seconds");
    assert!((before + 3600..=now() + 3600).contains(&at), "{at}");
    // `JSON.stringify(data, null, 2)`, no newline after.
    assert!(text.starts_with("{\n  \"version\": \"1.0\",\n"), "{text}");
    assert!(text.ends_with('}'));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&creds).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let left: Vec<_> = std::fs::read_dir(creds.parent().unwrap()).unwrap().collect();
    assert_eq!(left.len(), 1, "no temp file left behind");

    // Now fresh: read back without another request.
    let again = bearer(&dir_env(dir), "https://api.anthropic.com", false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.token, "at-2");
}

#[tokio::test]
async fn the_token_endpoint_is_the_clients_host_when_the_profile_names_none() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let (server, seen) = token_server(200, json!({ "access_token": "at-2", "expires_in": 60 }));
    config(dir, "default", oauth(json!({})));
    let creds = dir.join("credentials").join("default.json");
    write_creds(
        &creds,
        &json!({ "access_token": "at-1", "expires_at": now() - 5, "refresh_token": "rt-1" }),
    );
    let p = load(&dir_env(dir)).unwrap().unwrap();
    assert_eq!(p.token(&format!("{}/", server.url()), false).await.unwrap(), "at-2");
    assert_eq!(seen.lock().unwrap()[0].path, "/v1/oauth/token");
    // No new refresh token: the old one is kept.
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&creds).unwrap()).unwrap();
    assert_eq!(written["refresh_token"], "rt-1");
    assert_eq!(written["type"], "oauth_token");
    assert_eq!(written["version"], "1.0");
}

#[tokio::test]
async fn a_forced_refresh_replaces_even_a_fresh_token() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let (server, seen) = token_server(200, json!({ "access_token": "at-2", "expires_in": 3600 }));
    config(dir, "default", oauth(json!({ "base_url": server.url() })));
    let creds = dir.join("credentials").join("default.json");
    write_creds(
        &creds,
        &json!({ "access_token": "at-1", "expires_at": now() + 3600, "refresh_token": "rt-1" }),
    );
    let b = bearer(&dir_env(dir), "https://api.anthropic.com", true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(b.token, "at-2");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_request_dropped_during_the_refresh_still_gets_the_rotated_token_written() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let server = TestServer::start(|_| {
        std::thread::sleep(std::time::Duration::from_millis(300));
        Res::json(json!({ "access_token": "at-2", "expires_in": 3600, "refresh_token": "rt-2" }))
    });
    config(dir, "default", oauth(json!({ "base_url": server.url() })));
    let creds = dir.join("credentials").join("default.json");
    write_creds(
        &creds,
        &json!({ "access_token": "at-1", "expires_at": now() - 5, "refresh_token": "rt-1" }),
    );
    // The request gives up (dropped) while the token endpoint is still answering.
    let dropped = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        bearer(&dir_env(dir), "https://api.anthropic.com", false),
    )
    .await;
    assert!(dropped.is_err());
    let mut written = Value::Null;
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        written = serde_json::from_str(&std::fs::read_to_string(&creds).unwrap()).unwrap();
        if written["refresh_token"] == "rt-2" {
            break;
        }
    }
    assert_eq!(written["refresh_token"], "rt-2");
    assert_eq!(written["access_token"], "at-2");
}

#[tokio::test]
async fn a_failed_refresh_is_an_error_without_the_secrets_and_leaves_the_file() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let creds = dir.join("credentials").join("default.json");
    let stored = json!({ "access_token": "at-1", "expires_at": now() - 5, "refresh_token": "rt-1" });
    let fail = |status: u16, reply: Value| {
        let (server, _) = token_server(status, reply);
        config(dir, "default", oauth(json!({ "base_url": server.url() })));
        write_creds(&creds, &stored);
        let p = load(&dir_env(dir)).unwrap().unwrap();
        async move {
            let e = p
                .token("https://api.anthropic.com", false)
                .await
                .unwrap_err()
                .to_string();
            drop(server);
            e
        }
    };
    let e = fail(
        400,
        json!({ "error": "invalid_grant", "error_description": "Refresh token revoked", "refresh_token": "rt-1" }),
    )
    .await;
    assert_eq!(
        e,
        r#"User OAuth refresh failed (HTTP 400): {"error":"invalid_grant","error_description":"Refresh token revoked"}"#
    );
    assert_eq!(std::fs::read_to_string(&creds).unwrap(), stored.to_string());
    assert_eq!(
        fail(200, json!({ "expires_in": 60, "refresh_token": "x" })).await,
        "Token endpoint response missing access_token: {}"
    );
    assert_eq!(
        fail(200, json!({ "access_token": "a", "token_type": "mac" })).await,
        "Token endpoint response: unsupported token_type \"mac\" (want Bearer)"
    );
    assert_eq!(
        fail(
            200,
            json!({ "access_token": "a", "expires_in": "soon", "error": "odd" })
        )
        .await,
        r#"User OAuth refresh response missing or invalid expires_in: {"error":"odd"}"#
    );
    // A plain-text body is shown, cut after 2000 characters.
    let (server, _) = {
        let long = "x".repeat(2500);
        let server = TestServer::start(move |_| Res::text(503, &long));
        (server, ())
    };
    config(dir, "default", oauth(json!({ "base_url": server.url() })));
    let p = load(&dir_env(dir)).unwrap().unwrap();
    let e = p
        .token("https://api.anthropic.com", false)
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        e,
        format!(
            "User OAuth refresh failed (HTTP 503): {}... <500 more chars>",
            "x".repeat(2000)
        )
    );
}

#[tokio::test]
async fn a_token_that_cannot_be_refreshed_says_why() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let creds = dir.join("credentials").join("default.json");
    let shown = creds.display().to_string();
    // No client_id: a static token.
    config(dir, "default", json!({ "authentication": { "type": "user_oauth" } }));
    write_creds(
        &creds,
        &json!({ "access_token": "at-1", "expires_at": now() - 5, "refresh_token": "rt-1" }),
    );
    let p = load(&dir_env(dir)).unwrap().unwrap();
    assert_eq!(
        p.token("https://api.anthropic.com", false)
            .await
            .unwrap_err()
            .to_string(),
        format!("Access token at {shown} has expired and no refresh is available (client_id empty, refresh_token set)")
    );
    config(dir, "default", oauth(json!({ "base_url": "http://token.example" })));
    write_creds(&creds, &json!({ "access_token": "at-1", "expires_at": now() - 5 }));
    let p = load(&dir_env(dir)).unwrap().unwrap();
    assert_eq!(
        p.token("https://api.anthropic.com", false)
            .await
            .unwrap_err()
            .to_string(),
        format!("Access token at {shown} has expired and no refresh is available (client_id set, refresh_token empty)")
    );
    // A refresh token goes over https, or to this machine only.
    write_creds(
        &creds,
        &json!({ "access_token": "at-1", "expires_at": now() - 5, "refresh_token": "rt-1" }),
    );
    assert_eq!(
        p.token("https://api.anthropic.com", false)
            .await
            .unwrap_err()
            .to_string(),
        "Refusing to send credential over non-https token endpoint \"http://token.example\""
    );
    // Missing or broken files.
    write_creds(&creds, &json!({ "refresh_token": "rt-1" }));
    assert_eq!(
        p.token("https://api.anthropic.com", false)
            .await
            .unwrap_err()
            .to_string(),
        format!("Credentials file at {shown} must include 'access_token'")
    );
    std::fs::remove_file(&creds).unwrap();
    assert_eq!(
        p.token("https://api.anthropic.com", false)
            .await
            .unwrap_err()
            .to_string(),
        format!("Credentials file not found at {shown}: Error: ENOENT: no such file or directory, open '{shown}'")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_credentials_file_others_can_read_or_write_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    config(dir, "default", oauth(json!({})));
    let creds = dir.join("credentials").join("default.json");
    write_creds(&creds, &json!({ "access_token": "at-1" }));
    let real = std::fs::canonicalize(&creds).unwrap().display().to_string();
    let p = load(&dir_env(dir)).unwrap().unwrap();
    std::fs::set_permissions(&creds, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        p.token("https://api.anthropic.com", false)
            .await
            .unwrap_err()
            .to_string(),
        format!(
            "Credentials file at {real} is group/world-readable (mode 0o644); run `chmod 600 {real}` before retrying."
        )
    );
    std::fs::set_permissions(&creds, std::fs::Permissions::from_mode(0o620)).unwrap();
    assert_eq!(
        p.token("https://api.anthropic.com", false)
            .await
            .unwrap_err()
            .to_string(),
        format!(
            "Credentials file at {real} is group/world-writable (mode 0o620); this allows other local users to plant tokens. Run `chmod 600 {real}`."
        )
    );
}

#[test]
fn a_listing_without_a_chosen_host_goes_to_the_profiles() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    assert_eq!(listing_base_url(&dir_env(dir)), None);
    config(dir, "default", oauth(json!({ "base_url": "https://profile.example/" })));
    assert_eq!(
        listing_base_url(&dir_env(dir)).as_deref(),
        Some("https://profile.example")
    );
    config(dir, "default", oauth(json!({})));
    assert_eq!(listing_base_url(&dir_env(dir)), None);
}

#[test]
fn redaction_keeps_only_the_oauth_error_fields() {
    assert_eq!(
        redact_text(r#"{"error":"e","access_token":"s","error_uri":"u"}"#),
        r#"{"error":"e","error_uri":"u"}"#
    );
    assert_eq!(redact_text("[1]"), "null");
    assert_eq!(redact_text(r#""plain""#), r#""plain""#);
    assert_eq!(redact_text(""), "");
    assert_eq!(redact_text("short text"), "short text");
}

/// What `main` installs, end to end: a keyless listing of the anthropic provider (no `baseUrl`) goes to
/// the profile's host, refreshes the expired token there first, and sends it with the OAuth headers.
/// It reads the process environment, so it runs again in a child process whose `ANTHROPIC_CONFIG_DIR`
/// is a temporary directory and whose Anthropic variables are removed.
#[test]
fn a_keyless_listing_uses_the_installed_profile_end_to_end() {
    const CHILD: &str = "SASACODE_PROFILE_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let tmp = TempDir::new().unwrap();
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "anthropic_profile::tests::a_keyless_listing_uses_the_installed_profile_end_to_end",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("ANTHROPIC_CONFIG_DIR", tmp.path());
        for var in VARS
            .iter()
            .chain(&["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_CUSTOM_HEADERS"])
        {
            if *var != "ANTHROPIC_CONFIG_DIR" {
                cmd.env_remove(var);
            }
        }
        let out = cmd.output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{text}");
        assert!(text.contains("1 passed"), "the child ran no test: {text}");
        return;
    }
    let dir = std::path::PathBuf::from(std::env::var("ANTHROPIC_CONFIG_DIR").unwrap());
    let (server, seen) = {
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let log = seen.clone();
        let server = TestServer::start(move |req| {
            log.lock().unwrap().push(Seen {
                method: req.method.clone(),
                path: req.path.clone(),
                headers: req.headers.clone(),
                body: req.body.clone(),
            });
            match req.path.as_str() {
                "/v1/oauth/token" => Res::json(json!({ "access_token": "at-2", "expires_in": 3600 })),
                _ => Res::json(json!({ "data": [{ "type": "model", "id": "claude-x" }], "has_more": false })),
            }
        });
        (server, seen)
    };
    config(
        &dir,
        "default",
        oauth(json!({ "base_url": server.url(), "workspace_id": "wrkspc_e2e" })),
    );
    write_creds(
        &dir.join("credentials").join("default.json"),
        &json!({ "access_token": "at-1", "expires_at": now() - 1, "refresh_token": "rt-1" }),
    );
    sasacode_ai::transport::set_default_credentials(Some(credentials()));
    let p = sasacode_ai::ProviderConfig {
        api: "anthropic".into(),
        ..Default::default()
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let models = rt
        .block_on(crate::list_models::list_models(&p, None, &Default::default(), None))
        .unwrap();
    assert_eq!(models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["claude-x"]);
    let seen = seen.lock().unwrap().clone();
    let asked: Vec<(&str, &str)> = seen.iter().map(|s| (s.method.as_str(), s.path.as_str())).collect();
    assert_eq!(asked, [("POST", "/v1/oauth/token"), ("GET", "/v1/models")]);
    let listing = &seen[1];
    assert_eq!(header(listing, "authorization"), Some("Bearer at-2"));
    assert_eq!(header(listing, "anthropic-beta"), Some("oauth-2025-04-20"));
    assert_eq!(header(listing, "anthropic-workspace-id"), Some("wrkspc_e2e"));
    assert_eq!(header(listing, "x-api-key"), None);
}
