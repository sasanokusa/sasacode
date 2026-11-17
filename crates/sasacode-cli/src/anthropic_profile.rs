//! The `ant auth login` profile the Anthropic SDK (`@anthropic-ai/sdk` 0.128) fell back on when an
//! Anthropic request had no key and no `ANTHROPIC_AUTH_TOKEN` (`core/credentials.ts`,
//! `lib/credentials/{credential-chain,user-oauth,types}.ts`): the config directory, the active profile,
//! `configs/<profile>.json` and its credentials file, the access token while it is fresh, a
//! `refresh_token` grant when it is not, and the atomic write-back. `main` installs [`credentials`] in
//! the Anthropic client, which asks it for every request. Workload identity (`oidc_federation`) is not
//! ported: such a profile is an error.

use std::collections::HashMap;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail};
use futures::FutureExt;
use sasacode_ai::json::{stringify, stringify_pretty};
use sasacode_ai::transport::{Bearer, Credentials, OAUTH_BETA, js_string, js_truthy};
use serde_json::{Value, json};

/// `CREDENTIALS_FILE_VERSION`, written with refreshed tokens.
const CREDENTIALS_FILE_VERSION: &str = "1.0";
const TOKEN_ENDPOINT: &str = "/v1/oauth/token";
/// `MANDATORY_REFRESH_THRESHOLD_IN_SECONDS`: a token this close to its expiry is refreshed first.
const REFRESH_MARGIN_SECS: f64 = 30.0;
const MAX_TOKEN_RESPONSE_BYTES: usize = 1 << 20;
/// Not in the SDK: a refresh that hangs would hold up every Anthropic request (refreshes go one at a
/// time), so it gives up after this.
const REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_ERROR_BODY_CHARS: usize = 2000;
const USER_AGENT: &str = concat!("sasacode/", env!("CARGO_PKG_VERSION"));

/// The environment variables the SDK read here.
const VARS: [&str; 10] = [
    "ANTHROPIC_CONFIG_DIR",
    "APPDATA",
    "USERPROFILE",
    "XDG_CONFIG_HOME",
    "HOME",
    "ANTHROPIC_PROFILE",
    "ANTHROPIC_WORKSPACE_ID",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_FEDERATION_RULE_ID",
    "ANTHROPIC_ORGANIZATION_ID",
];

/// The environment, as `readEnv` saw it (trimmed, empty is unset). Tests build their own.
#[derive(Debug, Clone, Default)]
pub struct Env(HashMap<String, String>);

impl Env {
    /// The process's.
    pub fn process() -> Env {
        Env(VARS
            .iter()
            .filter_map(|n| std::env::var(n).ok().map(|v| (n.to_string(), v)))
            .collect())
    }

    pub fn with(mut self, name: &str, value: impl Into<String>) -> Env {
        self.0.insert(name.to_string(), value.into());
        self
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(|v| v.trim()).filter(|v| !v.is_empty())
    }
}

/// `getRootConfigPath`: `$ANTHROPIC_CONFIG_DIR`, else (Windows) `%APPDATA%\Anthropic` or
/// `%USERPROFILE%\AppData\Roaming\Anthropic`, else `$XDG_CONFIG_HOME/anthropic`, else
/// `$HOME/.config/anthropic`.
pub fn config_dir(env: &Env) -> Option<PathBuf> {
    if let Some(dir) = env.get("ANTHROPIC_CONFIG_DIR") {
        return Some(dir.into());
    }
    if cfg!(windows) {
        return env.get("APPDATA").map(|a| Path::new(a).join("Anthropic")).or_else(|| {
            env.get("USERPROFILE")
                .map(|u| Path::new(u).join("AppData").join("Roaming").join("Anthropic"))
        });
    }
    env.get("XDG_CONFIG_HOME")
        .map(|x| Path::new(x).join("anthropic"))
        .or_else(|| env.get("HOME").map(|h| Path::new(h).join(".config").join("anthropic")))
}

/// `getActiveProfileName`: `$ANTHROPIC_PROFILE`, else the content of `<dir>/active_config`, else
/// `default`.
fn active_profile(env: &Env, dir: &Path) -> anyhow::Result<String> {
    if let Some(name) = env.get("ANTHROPIC_PROFILE") {
        return Ok(name.to_string());
    }
    let path = dir.join("active_config");
    match read_text(&path) {
        Ok(text) => {
            let name = text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
            Ok(if name.is_empty() { "default" } else { name }.to_string())
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok("default".into()),
        Err(e) => bail!("failed to read {}: {}", path.display(), node_error(&e, "open", &path)),
    }
}

/// `validateProfileName`: the name becomes a file name, so no paths and nothing unusual.
pub fn validate_profile_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        bail!("profile name is empty");
    }
    if name == "." || name == ".." {
        bail!("profile name \"{name}\" is not allowed");
    }
    if name.contains('/') || name.contains('\\') {
        bail!("profile name \"{name}\" must not contain path separators");
    }
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)) {
        bail!("profile name \"{name}\" contains disallowed characters (allowed: letters, digits, '_', '.', '-')");
    }
    Ok(())
}

/// Workload identity is not ported; this is the SDK's text for a type it cannot use.
fn unsupported() -> anyhow::Error {
    anyhow!("authentication.type \"oidc_federation\" is not a known authentication type")
}

/// A `user_oauth` profile: where its tokens are kept and how they are refreshed.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub credentials_path: PathBuf,
    /// Empty or absent: the access token is static, never refreshed.
    pub client_id: Value,
    /// The file's `base_url`, else `$ANTHROPIC_BASE_URL`: the token endpoint's host.
    pub base_url: Option<String>,
    /// The file's `workspace_id`, else `$ANTHROPIC_WORKSPACE_ID`.
    pub workspace_id: Option<String>,
}

impl Profile {
    /// Whether a client at `base` may have this profile's token: Anthropic's API, or the host the
    /// profile names (its `base_url`, else `$ANTHROPIC_BASE_URL`). The SDK gave it to any keyless
    /// Anthropic-format client, so another provider without its key (commandcode-anthropic, say) got
    /// the token, and after a 401 there the refresh token went to that host (rust/DECISIONS.md ADR-013).
    pub fn serves(&self, base: &str) -> bool {
        let base = base.trim_end_matches('/');
        base == "https://api.anthropic.com"
            || self
                .base_url
                .as_deref()
                .is_some_and(|b| !b.is_empty() && b.trim_end_matches('/') == base)
    }
}

/// A string field as the SDK used it (other JSON values as `String(v)`).
fn text_of(v: &Value) -> String {
    js_string(Some(v))
}

/// `loadConfigWithSource` and `getCredentialsPath` for the active profile. `None`: there is no
/// profile (no config directory, no `configs/<profile>.json`).
pub fn load(env: &Env) -> anyhow::Result<Option<Profile>> {
    let Some(dir) = config_dir(env) else { return Ok(None) };
    let name = active_profile(env, &dir)?;
    validate_profile_name(&name)?;
    let path = dir.join("configs").join(format!("{name}.json"));
    let raw = match read_text(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            // Without a file, the SDK made a workload-identity config from these two alone.
            if env.get("ANTHROPIC_FEDERATION_RULE_ID").is_some() && env.get("ANTHROPIC_ORGANIZATION_ID").is_some() {
                return Err(unsupported());
            }
            return Ok(None);
        }
        Err(e) => bail!(
            "failed to read config file {}: {}",
            path.display(),
            node_error(&e, "open", &path)
        ),
    };
    let config: Value = serde_json::from_str(&raw)
        .map_err(|e| anyhow!("failed to parse config file {}: {}", path.display(), syntax_error(&e)))?;
    let auth = config
        .get("authentication")
        .filter(|a| js_truthy(a))
        .ok_or_else(|| anyhow!("config file {} is missing \"authentication\"", path.display()))?;
    match auth.get("type").and_then(Value::as_str) {
        Some("user_oauth") => {}
        Some("oidc_federation") => return Err(unsupported()),
        _ => bail!(
            "authentication.type \"{}\" is not a known authentication type",
            js_string(auth.get("type"))
        ),
    }
    // File values win; the environment only fills what the file leaves out (`??=`).
    let filled = |v: Option<&Value>, var: &str| match v.filter(|v| !v.is_null()) {
        Some(v) => Some(text_of(v)),
        None => env.get(var).map(str::to_string),
    };
    let credentials_path = match auth.get("credentials_path").filter(|p| js_truthy(p)) {
        Some(p) => PathBuf::from(text_of(p)),
        None => dir.join("credentials").join(format!("{name}.json")),
    };
    Ok(Some(Profile {
        credentials_path,
        client_id: auth.get("client_id").cloned().unwrap_or(Value::Null),
        base_url: filled(config.get("base_url"), "ANTHROPIC_BASE_URL"),
        workspace_id: filled(config.get("workspace_id"), "ANTHROPIC_WORKSPACE_ID"),
    }))
}

impl Profile {
    /// What goes with the token on every request (`resolveCredentialsFromConfig`).
    pub fn headers(&self) -> Vec<(String, String)> {
        self.workspace_id
            .iter()
            .filter(|w| !w.is_empty())
            .map(|w| ("anthropic-workspace-id".to_string(), w.clone()))
            .collect()
    }

    /// `userOAuthProvider`: the access token while it is fresh, else a refreshed one, written back.
    /// `force`: refresh even a token that looks fresh (the last one got a 401). `client_base`: the
    /// client's base URL, the token endpoint's host when the profile names none.
    pub async fn token(&self, client_base: &str, force: bool) -> anyhow::Result<String> {
        let path = &self.credentials_path;
        check_file_safety(path)?;
        let raw = read_text(path).map_err(|e| {
            anyhow!(
                "Credentials file not found at {}: {}",
                path.display(),
                node_error(&e, "open", path)
            )
        })?;
        let creds: Value = serde_json::from_str(&raw).map_err(|e| {
            anyhow!(
                "Credentials file at {} is not valid JSON: {}",
                path.display(),
                syntax_error(&e)
            )
        })?;
        let access = creds
            .get("access_token")
            .filter(|t| js_truthy(t))
            .ok_or_else(|| anyhow!("Credentials file at {} must include 'access_token'", path.display()))?;
        let expires_at = creds.get("expires_at").filter(|v| !v.is_null());
        if !force && expires_at.is_none_or(|at| now_secs() < number(at) - REFRESH_MARGIN_SECS) {
            return Ok(text_of(access));
        }
        let client_id = Some(&self.client_id).filter(|c| js_truthy(c));
        let refresh_token = creds.get("refresh_token").filter(|t| js_truthy(t));
        let (Some(client_id), Some(refresh_token)) = (client_id, refresh_token) else {
            let state = |set: bool| if set { "set" } else { "empty" };
            bail!(
                "Access token at {} has expired and no refresh is available (client_id {}, refresh_token {})",
                path.display(),
                state(client_id.is_some()),
                state(refresh_token.is_some())
            );
        };
        let base = match self.base_url.as_deref().filter(|b| !b.is_empty()) {
            Some(b) => b,
            None => client_base,
        }
        .trim_end_matches('/');
        require_secure_endpoint(base)?;
        let data = refresh(base, client_id, refresh_token).await?;
        let expires_in = data.get("expires_in").map_or(f64::NAN, number);
        if !expires_in.is_finite() {
            bail!(
                "User OAuth refresh response missing or invalid expires_in: {}",
                stringify(&redact(&data))
            );
        }
        let access = data.get("access_token").cloned().unwrap_or(Value::Null);
        // `{ ...creds, version, type, access_token, expires_at, refresh_token }`: other fields kept.
        let mut out = creds.as_object().cloned().unwrap_or_default();
        out.insert("version".into(), json!(CREDENTIALS_FILE_VERSION));
        out.insert("type".into(), json!("oauth_token"));
        out.insert("access_token".into(), access.clone());
        out.insert(
            "expires_at".into(),
            sasacode_ai::json::js_number(now_secs() + expires_in),
        );
        let rotated = data
            .get("refresh_token")
            .filter(|t| js_truthy(t))
            .unwrap_or(refresh_token);
        out.insert("refresh_token".into(), rotated.clone());
        write_atomic(path, &Value::Object(out))?;
        Ok(text_of(&access))
    }
}

/// The `refresh_token` grant: POST `<base>/v1/oauth/token`, once, its answer checked
/// (`parseTokenResponse`). Error bodies are redacted: they may echo the refresh token.
async fn refresh(base: &str, client_id: &Value, refresh_token: &Value) -> anyhow::Result<Value> {
    let body = json!({ "grant_type": "refresh_token", "refresh_token": refresh_token, "client_id": client_id });
    let resp = sasacode_ai::http::client()
        .post(format!("{base}{TOKEN_ENDPOINT}"))
        .header("content-type", "application/json")
        .header("anthropic-beta", OAUTH_BETA)
        .header("user-agent", USER_AGENT)
        .body(stringify(&body))
        .timeout(REFRESH_TIMEOUT)
        .send()
        .await
        .map_err(|e| {
            anyhow!(
                "User OAuth refresh failed to reach token endpoint: {:#}",
                anyhow::Error::new(e)
            )
        })?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("User OAuth refresh failed (HTTP {status}): {}", redact_text(&text));
    }
    let text = read_limited(resp).await;
    let data: Value = serde_json::from_str(&text)
        .map_err(|_| anyhow!("Token endpoint returned non-JSON response (status {status})"))?;
    if !data.get("access_token").is_some_and(js_truthy) {
        bail!(
            "Token endpoint response missing access_token: {}",
            stringify(&redact(&data))
        );
    }
    if let Some(kind) = data.get("token_type").filter(|t| js_truthy(t))
        && !text_of(kind).eq_ignore_ascii_case("bearer")
    {
        bail!(
            "Token endpoint response: unsupported token_type \"{}\" (want Bearer)",
            text_of(kind)
        );
    }
    Ok(data)
}

/// `requireSecureTokenEndpoint`: a refresh token goes over https, or plain http to this machine only.
fn require_secure_endpoint(base: &str) -> anyhow::Result<()> {
    if base.is_empty() {
        return Ok(());
    }
    let url = url::Url::parse(base).map_err(|_| {
        anyhow!("Invalid token endpoint base URL \"{base}\": TypeError: \"{base}\" cannot be parsed as a URL.")
    })?;
    if url.scheme() == "https" {
        return Ok(());
    }
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if url.scheme() == "http" && matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return Ok(());
    }
    bail!("Refusing to send credential over non-https token endpoint \"{base}\"")
}

/// At most 1 MiB of the body (`readLimitedText`).
async fn read_limited(mut resp: reqwest::Response) -> String {
    let mut buf = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        let room = MAX_TOKEN_RESPONSE_BYTES - buf.len();
        if chunk.len() >= room {
            buf.extend_from_slice(&chunk[..room]);
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// `redactSensitive` of a parsed body: an object keeps only the RFC 6749 error fields, a string is
/// redacted as a body of its own, anything else is `null`.
fn redact(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(
            o.iter()
                .filter(|(k, _)| ["error", "error_description", "error_uri"].contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        Value::String(s) => Value::String(redact_text(s)),
        _ => Value::Null,
    }
}

/// `redactSensitive` of a body's text: JSON is redacted and written again; other text is cut after
/// 2000 characters (UTF-16 units, as JS counts).
fn redact_text(text: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return stringify(&redact(&v));
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= MAX_ERROR_BODY_CHARS {
        return text.to_string();
    }
    format!(
        "{}... <{} more chars>",
        String::from_utf16_lossy(&units[..MAX_ERROR_BODY_CHARS]),
        units.len() - MAX_ERROR_BODY_CHARS
    )
}

/// `checkCredentialsFileSafety`: a credentials file (symlinks followed) that others may read or write
/// is refused. One that cannot be inspected is left to the read that follows. The SDK's warning
/// about a file another user owns is not given.
#[cfg(unix)]
fn check_file_safety(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let Ok(resolved) = std::fs::canonicalize(path) else {
        return Ok(());
    };
    let Ok(meta) = std::fs::metadata(&resolved) else {
        return Ok(());
    };
    let mode = meta.permissions().mode() & 0o777;
    let shown = resolved.display();
    if mode & 0o022 != 0 {
        bail!(
            "Credentials file at {shown} is group/world-writable (mode 0o{mode:o}); this allows other local users to plant tokens. Run `chmod 600 {shown}`."
        );
    }
    if mode & 0o044 != 0 {
        bail!(
            "Credentials file at {shown} is group/world-readable (mode 0o{mode:o}); run `chmod 600 {shown}` before retrying."
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_file_safety(_: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// `writeCredentialsFileAtomic`: the JSON (2-space indent) into a private sibling file, fsynced and
/// renamed over the target, the temp file removed on failure; the directory is made 0700 when
/// missing and fsynced after the rename (best effort).
fn write_atomic(target: &Path, data: &Value) -> anyhow::Result<()> {
    let dir = target
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut mkdir = std::fs::DirBuilder::new();
    mkdir.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut mkdir, 0o700);
    mkdir.create(dir).map_err(|e| node_error(&e, "mkdir", dir))?;
    let tmp = PathBuf::from(format!(
        "{}.{}.{}.tmp",
        target.display(),
        std::process::id(),
        random36()
    ));
    let written = (|| {
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
        let mut file = open.open(&tmp).map_err(|e| node_error(&e, "open", &tmp))?;
        file.write_all(stringify_pretty(data).as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| node_error(&e, "write", &tmp))?;
        drop(file);
        std::fs::rename(&tmp, target).map_err(|e| node_error(&e, "rename", &tmp))
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// `Math.random().toString(36).slice(2)`, near enough: a few base-36 digits for a unique name.
fn random36() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    let mut n = h.finish();
    let mut out = String::new();
    while n > 0 && out.len() < 11 {
        out.push(char::from_digit((n % 36) as u32, 36).unwrap_or('0'));
        n /= 36;
    }
    out
}

/// `nowAsSeconds`: whole seconds since the epoch.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as f64
}

/// `Number(v)` for the JSON values a credentials file or token response may hold.
fn number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => crate::list_models::js_to_number(s),
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Null => 0.0,
        _ => f64::NAN,
    }
}

fn read_text(path: &Path) -> std::io::Result<String> {
    std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// A Node fs error as `${err}` printed it: `Error: ENOENT: no such file or directory, open '<path>'`.
fn node_error(e: &std::io::Error, syscall: &str, path: &Path) -> anyhow::Error {
    let error = sasacode_tools::util::fs_error(e, syscall, &path.display().to_string());
    anyhow!("Error: {error}")
}

fn syntax_error(e: &serde_json::Error) -> String {
    format!("SyntaxError: JSON Parse error: {e}")
}

/// A request's token and headers from the profile `env` selects; `None` without a profile. One at a
/// time, so requests in parallel do not spend the same refresh token twice.
pub async fn bearer(env: &Env, client_base: &str, force: bool) -> anyhow::Result<Option<Bearer>> {
    static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let (env, client_base) = (env.clone(), client_base.to_string());
    // A task of its own: a request dropped mid-refresh must not stop the write-back (the endpoint may
    // have rotated the refresh token). The SDK's refresh ran on after an abort too.
    let task = tokio::spawn(async move {
        let _turn = ONE_AT_A_TIME.lock().await;
        let Some(profile) = load(&env)? else { return Ok(None) };
        if !profile.serves(&client_base) {
            return Ok(None);
        }
        let token = profile.token(&client_base, force).await?;
        Ok(Some(Bearer {
            token,
            headers: profile.headers(),
        }))
    });
    task.await.map_err(|e| anyhow!("{e}"))?
}

/// What `main` installs in the Anthropic client: the process's profile, read again for every request
/// (the TS made a client, and so the SDK read the profile, per request).
pub fn credentials() -> Credentials {
    Arc::new(|client_base: &str, force: bool| {
        let client_base = client_base.to_string();
        async move { bearer(&Env::process(), &client_base, force).await }.boxed()
    })
}

/// The host a model listing goes to when none is chosen (no `baseUrl`, no `ANTHROPIC_BASE_URL`) and
/// the profile authenticates it: the profile's `base_url`, which the SDK then used for the API too.
pub fn listing_base_url(env: &Env) -> Option<String> {
    let base = load(env).ok().flatten()?.base_url?;
    Some(base.trim_end_matches('/').to_string()).filter(|b| !b.is_empty())
}

#[cfg(test)]
#[path = "anthropic_profile_tests.rs"]
mod tests;
