//! ChatGPT-plan credentials for the openai-codex provider (`openai-codex/auth.ts`): where they are
//! stored and how they are kept fresh. The login itself (browser, callback server) is in login.rs.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use base64::Engine;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use sasacode_ai::Fetch;
use sasacode_plugin_api::sasacode_home;

use crate::net_util::utf16_prefix;

/// The public OAuth client of OpenAI's Codex CLI. There is no secret; PKCE protects the flow.
pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const CODEX_SCOPES: &str = "openid profile email offline_access";
const AUTH_CLAIM: &str = "https://api.openai.com/auth";
/// Refresh when less than this is left on the access token.
const REFRESH_WINDOW_MS: f64 = 5.0 * 60.0 * 1000.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexTokens {
    pub access: String,
    pub refresh: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Epoch ms, from the access token's `exp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
}

/// Stored beside the user's config, never in a project: a repository must not supply a login.
pub fn codex_auth_path() -> PathBuf {
    codex_auth_path_in(&sasacode_home())
}

/// [`codex_auth_path`] under another sasacode home.
pub fn codex_auth_path_in(home: &Path) -> PathBuf {
    home.join("codex-auth.json")
}

/// The stored login at the default path; None when nobody signed in (or the file is unreadable).
pub fn read_codex_auth() -> Option<CodexTokens> {
    read_codex_auth_at(&codex_auth_path())
}

fn str_field(t: &Map<String, Value>, key: &str) -> Option<String> {
    t.get(key).and_then(Value::as_str).map(str::to_string)
}

/// [`read_codex_auth`] of another file.
pub fn read_codex_auth_at(path: &Path) -> Option<CodexTokens> {
    let text = std::fs::read_to_string(path).ok()?;
    let t: Map<String, Value> = serde_json::from_str(&text).ok()?;
    let access = str_field(&t, "access").filter(|s| !s.is_empty())?;
    let refresh = str_field(&t, "refresh").filter(|s| !s.is_empty())?;
    let id_token = str_field(&t, "idToken");
    let account_id = str_field(&t, "accountId")
        .or_else(|| account_id(id_token.as_deref()))
        .or_else(|| account_id(Some(&access)));
    let expires_at = expiry(&access);
    Some(CodexTokens {
        access,
        refresh,
        id_token,
        account_id,
        expires_at,
    })
}

/// The last tokens [`codex_tokens`] looked up, shared for a few seconds.
type TokensFuture = Shared<BoxFuture<'static, Result<Option<CodexTokens>, String>>>;
static CACHED: Mutex<Option<(Instant, TokensFuture)>> = Mutex::new(None);

fn clear_cache() {
    *CACHED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Save credentials (owner-only), replacing the file in one step.
pub fn write_codex_auth(tokens: &CodexTokens, path: &Path) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        sasacode_plugin_api::create_private_dir(dir)?;
    }
    // Temp file + rename: a crash mid-write must not leave a broken credential behind.
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    let text = format!(
        "{}\n",
        sasacode_ai::json::stringify_pretty(&serde_json::to_value(tokens)?)
    );
    sasacode_plugin_api::write_private(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    clear_cache();
    Ok(())
}

/// Remove the stored login; whether there was one.
pub fn clear_codex_auth(path: &Path) -> bool {
    let had = read_codex_auth_at(path).is_some();
    let _ = std::fs::remove_file(path);
    clear_cache();
    had
}

// ── token claims (read, never verified: the server does that) ───────────────

/// The claims of a JWT, None when it is not one.
pub fn jwt_claims(token: Option<&str>) -> Option<Map<String, Value>> {
    let payload = token?.split('.').nth(1)?;
    // Buffer.from(…, "base64url") ignores padding and characters outside the alphabet.
    let clean: String = payload
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&clean)
        .or_else(|_| {
            // A dangling last character carries no whole byte: Node drops it.
            let usable = clean.len() - clean.len() % 4;
            let usable = if clean.len() % 4 == 1 { usable } else { clean.len() };
            base64::engine::GeneralPurpose::new(
                &base64::alphabet::URL_SAFE,
                base64::engine::GeneralPurposeConfig::new()
                    .with_encode_padding(false)
                    .with_decode_allow_trailing_bits(true)
                    .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
            )
            .decode(&clean[..usable])
        })
        .ok()?;
    serde_json::from_slice::<Value>(&bytes).ok()?.as_object().cloned()
}

fn auth_claim(token: Option<&str>) -> Option<Map<String, Value>> {
    jwt_claims(token)?.get(AUTH_CLAIM)?.as_object().cloned()
}

fn claim_str(token: Option<&str>, key: &str) -> Option<String> {
    auth_claim(token)?
        .get(key)?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn account_id(token: Option<&str>) -> Option<String> {
    claim_str(token, "chatgpt_account_id")
}

pub fn plan_type(id_token: Option<&str>) -> Option<String> {
    claim_str(id_token, "chatgpt_plan_type")
}

fn expiry(token: &str) -> Option<f64> {
    jwt_claims(Some(token))?.get("exp")?.as_f64().map(|exp| exp * 1000.0)
}

/// Turn a token endpoint response into stored credentials.
pub fn tokens_from(data: &Value, previous: Option<&CodexTokens>) -> anyhow::Result<CodexTokens> {
    let field = |key: &str| data.get(key).and_then(Value::as_str).map(str::to_string);
    let Some(access) = field("access_token").filter(|a| !a.is_empty()) else {
        anyhow::bail!("the token response has no access_token");
    };
    let refresh = field("refresh_token").or_else(|| previous.map(|p| p.refresh.clone()));
    let Some(refresh) = refresh.filter(|r| !r.is_empty()) else {
        anyhow::bail!("the token response has no refresh_token");
    };
    let id_token = field("id_token").or_else(|| previous.and_then(|p| p.id_token.clone()));
    let account_id = account_id(id_token.as_deref())
        .or_else(|| account_id(Some(&access)))
        .or_else(|| previous.and_then(|p| p.account_id.clone()));
    let expires_at = expiry(&access);
    Ok(CodexTokens {
        access,
        refresh,
        id_token,
        account_id,
        expires_at,
    })
}

/// A form-encoded POST to the token endpoint.
pub(crate) fn token_request(form: &[(&str, &str)]) -> anyhow::Result<reqwest::Request> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form)
        .finish();
    Ok(sasacode_ai::http::client()
        .post(CODEX_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .build()?)
}

// ── refresh ─────────────────────────────────────────────────────────────────

pub async fn refresh_codex_tokens(
    tokens: &CodexTokens,
    path: &Path,
    fetch: Option<&Arc<dyn Fetch>>,
) -> anyhow::Result<CodexTokens> {
    let req = token_request(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", &tokens.refresh),
        ("client_id", CODEX_CLIENT_ID),
    ])?;
    let res = sasacode_ai::send(fetch, req).await?;
    if !res.status().is_success() {
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        anyhow::bail!("ChatGPT login refresh failed ({status}): {}", utf16_prefix(&text, 200));
    }
    let data: Value = res.json().await?;
    let next = tokens_from(&data, Some(tokens))?;
    write_codex_auth(&next, path)?;
    Ok(next)
}

/// Credentials to use now, refreshed first when about to expire; None when nobody signed in.
/// A failed refresh keeps a token that still works rather than breaking the session.
pub async fn current_codex_tokens(
    path: &Path,
    fetch: Option<&Arc<dyn Fetch>>,
    now: f64,
) -> anyhow::Result<Option<CodexTokens>> {
    let Some(stored) = read_codex_auth_at(path) else {
        return Ok(None);
    };
    if let Some(at) = stored.expires_at
        && at - now > REFRESH_WINDOW_MS
    {
        return Ok(Some(stored));
    }
    match refresh_codex_tokens(&stored, path, fetch).await {
        Ok(next) => Ok(Some(next)),
        Err(e) => {
            if stored.expires_at.is_some_and(|at| at > now) {
                return Ok(Some(stored));
            }
            Err(e)
        }
    }
}

/// [`current_codex_tokens`] at the default path, shared for a few seconds: one request asks for the
/// token more than once.
pub fn codex_tokens() -> BoxFuture<'static, anyhow::Result<Option<CodexTokens>>> {
    let shared = {
        let mut cached = CACHED.lock().unwrap_or_else(|e| e.into_inner());
        match &*cached {
            Some((at, tokens)) if at.elapsed().as_millis() <= 10_000 => tokens.clone(),
            _ => {
                let tokens = async {
                    let r = current_codex_tokens(&codex_auth_path(), None, sasacode_ai::now_ms() as f64).await;
                    r.map_err(|e| {
                        clear_cache();
                        e.to_string()
                    })
                }
                .boxed()
                .shared();
                *cached = Some((Instant::now(), tokens.clone()));
                tokens
            }
        }
    };
    async move { shared.await.map_err(anyhow::Error::msg) }.boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_decode_like_node_base64url() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"exp":12}"#);
        assert_eq!(
            jwt_claims(Some(&format!("x.{payload}.x"))).and_then(|c| c.get("exp").cloned()),
            Some(12.into())
        );
        let padded = base64::engine::general_purpose::URL_SAFE.encode(br#"{"exp":1}"#);
        assert!(jwt_claims(Some(&format!("x.{padded}.x"))).is_some());
        assert!(jwt_claims(Some("not a jwt")).is_none());
        assert!(jwt_claims(None).is_none());
    }
}
