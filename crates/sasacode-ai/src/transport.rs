//! What the npm SDKs (`@anthropic-ai/sdk` 0.128, `openai` 7.23) did around each request, done by hand
//! (rust/research/llm-sdks.md): headers, base-URL joining, retries with backoff and server hints, the
//! time-to-headers timeout, the error each failure becomes (status, code, type, param and the exact
//! message text users see), abort, and reading the event stream. The adapters build the bodies.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures::stream::{BoxStream, Stream, StreamExt};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::provider::{Fetch, StreamEvent};
use crate::sse::{SseDecoder, SseEvent, parse_json};

/// Time allowed until the response headers arrive, per attempt (both SDKs' default). The body has no limit.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

const USER_AGENT: &str = concat!("sasacode/", env!("CARGO_PKG_VERSION"));

/// Which SDK's conventions apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Anthropic,
    OpenAi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorKind {
    /// A non-2xx response.
    Status,
    /// An error event inside a 200 stream (no status).
    Stream,
    /// The request could not be sent (`APIConnectionError`).
    Connection,
    /// No response headers within the timeout (`APIConnectionTimeoutError`).
    Timeout,
    /// The signal was cancelled before the response arrived (`APIUserAbortError`).
    UserAbort,
}

/// The SDKs' `APIError`: what sasacode inspects to classify a failure, and the message users see.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub kind: ApiErrorKind,
    pub status: Option<u16>,
    /// The error object the message was made from (Anthropic: the whole body; OpenAI: `body.error`).
    pub error: Option<Value>,
    /// OpenAI `error.code` (a string, or a number on vLLM).
    pub code: Option<Value>,
    pub param: Option<Value>,
    /// OpenAI `error.type`; Anthropic `body.error.type`.
    pub error_type: Option<Value>,
    pub message: String,
    /// What made a connection error (not part of the message, as in the SDKs).
    pub cause: Option<String>,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    fn bare(kind: ApiErrorKind, message: &str) -> Self {
        ApiError {
            kind,
            status: None,
            error: None,
            code: None,
            param: None,
            error_type: None,
            message: message.to_string(),
            cause: None,
        }
    }

    pub fn user_abort() -> Self {
        Self::bare(ApiErrorKind::UserAbort, "Request was aborted.")
    }

    pub fn connection(cause: Option<String>) -> Self {
        ApiError {
            cause,
            ..Self::bare(ApiErrorKind::Connection, "Connection error.")
        }
    }

    pub fn timeout() -> Self {
        Self::bare(ApiErrorKind::Timeout, "Request timed out.")
    }

    /// `new OpenAI.APIError(status, error, message)`: code, param and type come from `error`.
    pub fn openai(status: Option<u16>, error: Option<Value>, message: Option<&str>) -> Self {
        let field = |k: &str| error.as_ref().and_then(|e| e.get(k)).cloned();
        ApiError {
            kind: if status.is_some() {
                ApiErrorKind::Status
            } else {
                ApiErrorKind::Stream
            },
            status,
            code: field("code"),
            param: field("param"),
            error_type: field("type"),
            message: make_message(status, error.as_ref(), message),
            error,
            cause: None,
        }
    }

    /// `new Anthropic.APIError(status, body, message, headers, body.error.type)`.
    pub fn anthropic(status: Option<u16>, body: Option<Value>, message: Option<&str>) -> Self {
        let error_type = body
            .as_ref()
            .and_then(|b| b.get("error"))
            .and_then(|e| e.get("type"))
            .cloned();
        ApiError {
            kind: if status.is_some() {
                ApiErrorKind::Status
            } else {
                ApiErrorKind::Stream
            },
            status,
            code: None,
            param: None,
            error_type,
            message: make_message(status, body.as_ref(), message),
            error: body,
            cause: None,
        }
    }

    /// A non-2xx response with body `text`, as the SDK of `flavor` reports it (`makeStatusError`).
    pub fn from_response(flavor: Flavor, status: u16, text: &str) -> Self {
        let json = parse_json(text).ok();
        // `errJSON ? undefined : errText`
        let message = if json.as_ref().is_some_and(js_truthy) {
            None
        } else {
            Some(text)
        };
        match flavor {
            Flavor::Anthropic => Self::anthropic(Some(status), json, message),
            Flavor::OpenAi => Self::openai_status(status, json, message),
        }
    }

    /// `OpenAI.makeStatusError` + `APIError.generate(status, body, message)`: a body without an `error`
    /// member is wrapped first, then `body.error` is the error object.
    pub fn openai_status(status: u16, body: Option<Value>, message: Option<&str>) -> Self {
        let normalized = match body {
            Some(b @ (Value::Object(_) | Value::Array(_))) if b.get("error").is_none_or(Value::is_null) => {
                let mut m = Map::new();
                m.insert("error".into(), b);
                Some(Value::Object(m))
            }
            other => other,
        };
        let error = normalized.as_ref().and_then(|n| n.get("error")).cloned();
        Self::openai(Some(status), error, message)
    }

    pub fn is_bad_request(&self) -> bool {
        self.kind == ApiErrorKind::Status && self.status == Some(400)
    }
}

/// `APIError.makeMessage`.
fn make_message(status: Option<u16>, error: Option<&Value>, message: Option<&str>) -> String {
    let error = error.filter(|e| js_truthy(e));
    let msg = match error {
        Some(e) => match e.get("message").filter(|m| js_truthy(m)) {
            Some(Value::String(s)) => s.clone(),
            Some(m) => js_stringify(m),
            None => js_stringify(e),
        },
        None => message.unwrap_or_default().to_string(),
    };
    match (status, msg.is_empty()) {
        (Some(s), false) => format!("{s} {msg}"),
        (Some(s), true) => format!("{s} status code (no body)"),
        (None, false) => msg,
        (None, true) => "(no status code or body)".to_string(),
    }
}

/// JS truthiness of a JSON value.
pub fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `String(v)` for a JSON value (`undefined` when absent).
pub fn js_string(v: Option<&Value>) -> String {
    match v {
        None => "undefined".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(_)) => "[object Object]".to_string(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| if x.is_null() { String::new() } else { js_string(Some(x)) })
            .collect::<Vec<_>>()
            .join(","),
        Some(other) => js_stringify(other),
    }
}

/// `JSON.stringify(v)`: like serde_json, but whole floats print as integers (`1`, not `1.0`).
pub fn js_stringify(v: &Value) -> String {
    fn has_float(v: &Value) -> bool {
        match v {
            Value::Number(n) => n.is_f64(),
            Value::Array(a) => a.iter().any(has_float),
            Value::Object(o) => o.values().any(has_float),
            _ => false,
        }
    }
    fn normalize(v: &Value) -> Value {
        match v {
            Value::Number(n) if n.is_f64() => crate::json::js_number(n.as_f64().unwrap_or(f64::NAN)),
            Value::Array(a) => Value::Array(a.iter().map(normalize).collect()),
            Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), normalize(v))).collect()),
            other => other.clone(),
        }
    }
    if has_float(v) {
        crate::json::stringify(&normalize(v))
    } else {
        crate::json::stringify(v)
    }
}

/// `parseFloat`: the longest number at the start (after whitespace), NaN when there is none.
fn parse_float(s: &str) -> f64 {
    let s = s.trim_start();
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    if s[i..].starts_with("Infinity") {
        return if b.first() == Some(&b'-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    let digits_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut mantissa = i > digits_start;
    if i < b.len() && b[i] == b'.' {
        let frac_start = i + 1;
        let mut j = frac_start;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > frac_start || mantissa {
            mantissa = mantissa || j > frac_start;
            i = j;
        }
    }
    if !mantissa {
        return f64::NAN;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let exp_start = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > exp_start {
            i = j;
        }
    }
    s[..i].parse().unwrap_or(f64::NAN)
}

/// `Date.parse(v) - Date.now()` for an HTTP date; NaN when it is not one.
fn ms_until_date(v: &str) -> f64 {
    let parsed =
        chrono::DateTime::parse_from_rfc2822(v.trim()).or_else(|_| chrono::DateTime::parse_from_rfc3339(v.trim()));
    match parsed {
        Ok(d) => (d.timestamp_millis() - crate::now_ms()) as f64,
        Err(_) => f64::NAN,
    }
}

/// A number in [0, 1).
fn random_unit() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_i64(crate::now_ms());
    h.write_u128(std::time::Instant::now().elapsed().as_nanos());
    (h.finish() >> 11) as f64 / (1u64 << 53) as f64
}

/// The configured client of one SDK: base URL, retries, headers below the per-request ones, transport.
#[derive(Clone)]
pub struct ApiClient {
    pub flavor: Flavor,
    pub base_url: String,
    pub max_retries: u32,
    pub timeout: Duration,
    /// Defaults, auth and the model's headers, merged (lower-case names, later wins).
    headers: Vec<(String, String)>,
    fetch: Option<Arc<dyn Fetch>>,
}

impl std::fmt::Debug for ApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiClient")
            .field("flavor", &self.flavor)
            .field("base_url", &self.base_url)
            .field("max_retries", &self.max_retries)
            .finish_non_exhaustive()
    }
}

/// Set a header the way `Headers.set` does: case-insensitive, replacing an earlier value.
fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    let name = name.to_ascii_lowercase();
    match headers.iter_mut().find(|(n, _)| *n == name) {
        Some(h) => h.1 = value.to_string(),
        None => headers.push((name, value.to_string())),
    }
}

/// `readEnv`: trimmed, absent when empty.
fn read_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// `ANTHROPIC_CUSTOM_HEADERS` / `OPENAI_CUSTOM_HEADERS`: `Name: value` lines.
fn custom_headers(env: &str) -> Vec<(String, String)> {
    let Some(text) = read_env(env) else { return Vec::new() };
    text.split('\n')
        .filter_map(|line| {
            line.find(':')
                .map(|i| (line[..i].trim().to_string(), line[i + 1..].trim().to_string()))
        })
        .collect()
}

impl ApiClient {
    fn new(
        flavor: Flavor,
        base_url: &str,
        max_retries: u32,
        defaults: Vec<(String, String)>,
        model_headers: Option<&BTreeMap<String, String>>,
        fetch: Option<Arc<dyn Fetch>>,
    ) -> Self {
        let mut headers = Vec::new();
        for (n, v) in &defaults {
            set_header(&mut headers, n, v);
        }
        let env = if flavor == Flavor::Anthropic {
            "ANTHROPIC_CUSTOM_HEADERS"
        } else {
            "OPENAI_CUSTOM_HEADERS"
        };
        // `{ ...custom, ...defaultHeaders }`: the model's headers win over the environment's.
        for (n, v) in custom_headers(env) {
            set_header(&mut headers, &n, &v);
        }
        for (n, v) in model_headers.into_iter().flatten() {
            set_header(&mut headers, n, v);
        }
        set_header(&mut headers, "content-type", "application/json");
        ApiClient {
            flavor,
            base_url: base_url.to_string(),
            max_retries,
            timeout: DEFAULT_TIMEOUT,
            headers,
            fetch,
        }
    }

    /// `new Anthropic({ apiKey?, baseURL, maxRetries, defaultHeaders, fetch })`. Without a key the SDK
    /// read `ANTHROPIC_API_KEY` (sent as `x-api-key`) and `ANTHROPIC_AUTH_TOKEN` (a bearer token).
    pub fn anthropic(
        api_key: Option<&str>,
        base_url: &str,
        max_retries: u32,
        model_headers: Option<&BTreeMap<String, String>>,
        fetch: Option<Arc<dyn Fetch>>,
    ) -> Self {
        let mut defaults = vec![
            ("accept".to_string(), "application/json".to_string()),
            ("user-agent".to_string(), USER_AGENT.to_string()),
            ("x-stainless-timeout".to_string(), DEFAULT_TIMEOUT.as_secs().to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ];
        let key = api_key
            .filter(|k| !k.is_empty())
            .map(str::to_string)
            .or_else(|| read_env("ANTHROPIC_API_KEY"));
        if let Some(k) = key {
            defaults.push(("x-api-key".to_string(), k));
        }
        if let Some(t) = read_env("ANTHROPIC_AUTH_TOKEN") {
            defaults.push(("authorization".to_string(), format!("Bearer {t}")));
        }
        Self::new(Flavor::Anthropic, base_url, max_retries, defaults, model_headers, fetch)
    }

    /// `new OpenAI({ apiKey, baseURL, maxRetries, defaultHeaders, fetch })`.
    pub fn openai(
        api_key: &str,
        base_url: &str,
        max_retries: u32,
        model_headers: Option<&BTreeMap<String, String>>,
        fetch: Option<Arc<dyn Fetch>>,
    ) -> Self {
        let mut defaults = vec![
            ("accept".to_string(), "application/json".to_string()),
            ("user-agent".to_string(), USER_AGENT.to_string()),
        ];
        if let Some(org) = read_env("OPENAI_ORG_ID") {
            defaults.push(("openai-organization".to_string(), org));
        }
        if let Some(project) = read_env("OPENAI_PROJECT_ID") {
            defaults.push(("openai-project".to_string(), project));
        }
        defaults.push(("authorization".to_string(), format!("Bearer {api_key}")));
        Self::new(Flavor::OpenAi, base_url, max_retries, defaults, model_headers, fetch)
    }

    /// `buildURL`: base + path, one `/` dropped when both have it.
    pub fn url(&self, path: &str) -> anyhow::Result<url::Url> {
        let joined = if self.base_url.ends_with('/') && path.starts_with('/') {
            format!("{}{}", self.base_url, &path[1..])
        } else {
            format!("{}{}", self.base_url, path)
        };
        url::Url::parse(&joined).map_err(|_| anyhow::anyhow!("\"{joined}\" cannot be parsed as a URL."))
    }

    /// POST `body` as JSON to `path`, with `extra` headers on top. Retries (before any response
    /// headers only) on 408/409/429/5xx, `x-should-retry: true` and transport errors. A non-2xx
    /// answer becomes an [`ApiError`].
    pub async fn post(
        &self,
        path: &str,
        body: &Value,
        extra: &[(&str, &str)],
        signal: &CancellationToken,
    ) -> anyhow::Result<reqwest::Response> {
        let body = Bytes::from(js_stringify(body));
        self.request(reqwest::Method::POST, path, Some(body), extra, signal)
            .await
    }

    /// GET `path` (for the Models API), same retries and errors as [`ApiClient::post`].
    pub async fn get(&self, path: &str, signal: &CancellationToken) -> anyhow::Result<reqwest::Response> {
        self.request(reqwest::Method::GET, path, None, &[], signal).await
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Bytes>,
        extra: &[(&str, &str)],
        signal: &CancellationToken,
    ) -> anyhow::Result<reqwest::Response> {
        let url = self.url(path)?;
        let mut headers = self.headers.clone();
        if body.is_none() {
            headers.retain(|(n, _)| n != "content-type");
        }
        for (n, v) in extra {
            set_header(&mut headers, n, v);
        }
        if self.flavor == Flavor::Anthropic
            && !headers
                .iter()
                .any(|(n, v)| (n == "x-api-key" || n == "authorization") && !v.is_empty())
        {
            anyhow::bail!(
                "Could not resolve authentication method. Expected one of apiKey, authToken, credentials, config, or profile to be set. Or for one of the \"X-Api-Key\" or \"Authorization\" headers to be explicitly omitted"
            );
        }
        let mut header_map = reqwest::header::HeaderMap::new();
        for (n, v) in &headers {
            let name = reqwest::header::HeaderName::from_bytes(n.as_bytes())
                .map_err(|_| anyhow::anyhow!("Invalid header name: \"{n}\""))?;
            let value = reqwest::header::HeaderValue::from_str(v)
                .map_err(|_| anyhow::anyhow!("Invalid header value for \"{n}\""))?;
            header_map.insert(name, value);
        }

        let max = self.max_retries;
        let mut remaining = max;
        loop {
            if signal.is_cancelled() {
                return Err(ApiError::user_abort().into());
            }
            let mut req = reqwest::Request::new(method.clone(), url.clone());
            *req.headers_mut() = header_map.clone();
            req.headers_mut().insert(
                "x-stainless-retry-count",
                reqwest::header::HeaderValue::from(max - remaining),
            );
            if let Some(b) = &body {
                *req.body_mut() = Some(b.clone().into());
            }
            let sent = tokio::select! {
                biased;
                _ = signal.cancelled() => return Err(ApiError::user_abort().into()),
                r = tokio::time::timeout(self.timeout, crate::provider::send(self.fetch.as_ref(), req)) => r,
            };
            let resp = match sent {
                Ok(Ok(resp)) => resp,
                failed => {
                    let (timed_out, cause) = match failed {
                        Err(_) => (true, None),
                        Ok(Err(e)) => {
                            let text = format!("{e:#}");
                            (timed_out_text(&text), Some(text))
                        }
                        Ok(Ok(_)) => (false, None),
                    };
                    if remaining > 0 {
                        self.sleep(self.backoff_ms(remaining), signal).await?;
                        remaining -= 1;
                        continue;
                    }
                    return Err(if timed_out {
                        ApiError::timeout()
                    } else {
                        ApiError::connection(cause)
                    }
                    .into());
                }
            };
            let status = resp.status();
            if status.is_success() {
                return Ok(resp);
            }
            let retry = should_retry(&resp);
            if remaining > 0 && retry {
                let wait = self.retry_wait_ms(resp.headers(), remaining);
                drop(resp);
                self.sleep(wait, signal).await?;
                remaining -= 1;
                continue;
            }
            let text = tokio::select! {
                biased;
                _ = signal.cancelled() => return Err(ApiError::user_abort().into()),
                t = resp.text() => t.unwrap_or_else(|e| e.to_string()),
            };
            return Err(ApiError::from_response(self.flavor, status.as_u16(), &text).into());
        }
    }

    async fn sleep(&self, ms: f64, signal: &CancellationToken) -> anyhow::Result<()> {
        let d = Duration::from_secs_f64((ms / 1000.0).clamp(0.0, 4_294_967.0));
        tokio::select! {
            biased;
            _ = signal.cancelled() => Err(ApiError::user_abort().into()),
            _ = tokio::time::sleep(d) => Ok(()),
        }
    }

    /// `calculateDefaultRetryTimeoutMillis`: 0.5 s doubling up to 8 s, minus up to 25% jitter.
    fn backoff_ms(&self, remaining: u32) -> f64 {
        let n = (self.max_retries - remaining) as i32;
        let secs = (0.5 * 2f64.powi(n)).min(8.0);
        secs * (1.0 - random_unit() * 0.25) * 1000.0
    }

    /// How long to wait before the next attempt: `retry-after-ms`, then `retry-after` (seconds or an
    /// HTTP date), within each SDK's bounds, else the backoff.
    fn retry_wait_ms(&self, headers: &reqwest::header::HeaderMap, remaining: u32) -> f64 {
        let get = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).filter(|v| !v.is_empty());
        let mut ms: Option<f64> = None;
        if let Some(v) = get("retry-after-ms") {
            let x = parse_float(v);
            if !x.is_nan() {
                ms = Some(x);
            }
        }
        let use_retry_after = match self.flavor {
            // `!timeoutMillis`: 0 falls through too.
            Flavor::Anthropic => ms.is_none_or(|m| m == 0.0),
            Flavor::OpenAi => ms.is_none(),
        };
        if use_retry_after && let Some(v) = get("retry-after") {
            let secs = parse_float(v);
            ms = Some(if secs.is_nan() { ms_until_date(v) } else { secs * 1000.0 });
        }
        let usable = match (self.flavor, ms) {
            (Flavor::Anthropic, Some(m)) => m > 0.0 && m <= 2_147_483_647.0,
            (Flavor::OpenAi, Some(m)) => m.is_finite() && (0.0..=60_000.0).contains(&m),
            (_, None) => false,
        };
        match ms {
            Some(m) if usable => m,
            _ => self.backoff_ms(remaining),
        }
    }
}

/// `/timed? ?out/i` on the failure.
fn timed_out_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["timed out", "timedout", "time out", "timeout"]
        .iter()
        .any(|p| lower.contains(p))
}

/// `shouldRetry`: the server's `x-should-retry` first, then 408, 409, 429 and 5xx.
fn should_retry(resp: &reqwest::Response) -> bool {
    match resp.headers().get("x-should-retry").and_then(|v| v.to_str().ok()) {
        Some("true") => return true,
        Some("false") => return false,
        _ => {}
    }
    let s = resp.status().as_u16();
    s == 408 || s == 409 || s == 429 || s >= 500
}

/// The body of a 2xx response broke while it was being read: the connection dropped mid-reply.
/// The adapters keep what arrived (`droppedMidReply`), instead of failing the reply.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct BodyError(#[source] pub reqwest::Error);

/// Is this a reply cut by the connection (a body read error), rather than an error to report?
pub fn is_cut_reply(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<BodyError>())
}

/// A body read error, told apart by kind (the TS matched the message): a broken connection — hyper's
/// own errors, or an io error from a plugin's transport — is a [`BodyError`], a cut reply; any other
/// error a plugin's body stream raised is thrown, with that error's message.
fn body_error(e: reqwest::Error) -> anyhow::Error {
    // A read timeout of a plugin's own client (the shared one has none): the TS counted ETIMEDOUT as a cut.
    if e.is_timeout() {
        return BodyError(e).into();
    }
    let mut raised = None;
    let mut cause = std::error::Error::source(&e);
    while let Some(c) = cause {
        if c.is::<std::io::Error>() || c.is::<hyper::Error>() {
            return BodyError(e).into();
        }
        // reqwest wraps a stream's error once more: the message is the first one not its own.
        if raised.is_none() && !c.is::<reqwest::Error>() {
            raised = Some(c.to_string());
        }
        cause = c.source();
    }
    match raised {
        Some(message) => anyhow::Error::new(e).context(message),
        None => BodyError(e).into(),
    }
}

/// What reading the next event gave.
#[derive(Debug)]
pub enum Next<T> {
    Item(T),
    /// The body (or the `[DONE]` sentinel) ended the stream.
    End,
    /// The signal was cancelled while waiting.
    Aborted,
}

/// The events of a 2xx response.
pub struct EventSource {
    body: BoxStream<'static, reqwest::Result<Bytes>>,
    decoder: SseDecoder,
    queue: std::collections::VecDeque<SseEvent>,
    eof: bool,
    flush_at_eof: bool,
}

impl EventSource {
    /// `flush_at_eof`: dispatch a last event that lacked its blank line (openai), or drop it (anthropic).
    pub fn new(resp: reqwest::Response, flush_at_eof: bool) -> Self {
        EventSource {
            body: resp.bytes_stream().boxed(),
            decoder: SseDecoder::new(),
            queue: Default::default(),
            eof: false,
            flush_at_eof,
        }
    }

    /// The next raw event. A broken connection is a [`BodyError`] (see `body_error`).
    pub async fn next(&mut self, signal: &CancellationToken) -> anyhow::Result<Next<SseEvent>> {
        loop {
            if let Some(ev) = self.queue.pop_front() {
                return Ok(Next::Item(ev));
            }
            if self.eof {
                return Ok(Next::End);
            }
            if signal.is_cancelled() {
                return Ok(Next::Aborted);
            }
            let chunk = tokio::select! {
                biased;
                _ = signal.cancelled() => return Ok(Next::Aborted),
                c = self.body.next() => c,
            };
            match chunk {
                Some(Ok(bytes)) => self.queue.extend(self.decoder.feed(&bytes)),
                Some(Err(e)) => return Err(body_error(e)),
                None => {
                    self.eof = true;
                    self.queue.extend(self.decoder.finish(self.flush_at_eof));
                }
            }
        }
    }

    /// The openai SDK's `Stream`: `[DONE]` ends it; an `event: error`, or any event whose `error` is
    /// truthy, is an [`ApiError`] without status; other names are ignored; data must be JSON.
    pub async fn next_openai(&mut self, signal: &CancellationToken) -> anyhow::Result<Next<Value>> {
        loop {
            let sse = match self.next(signal).await? {
                Next::Item(sse) => sse,
                Next::End => return Ok(Next::End),
                Next::Aborted => return Ok(Next::Aborted),
            };
            if sse.data == "[DONE]" {
                self.eof = true;
                self.queue.clear();
                return Ok(Next::End);
            }
            if sse.event.as_deref().is_some_and(|e| e.starts_with("thread.")) {
                continue;
            }
            let data = parse_json(&sse.data)
                .map_err(|_| anyhow::anyhow!("Error reading response: malformed server-sent event JSON."))?;
            if sse.event.as_deref() == Some("error") {
                // `data?.error ?? data`
                let error = data.get("error").filter(|e| !e.is_null()).cloned().unwrap_or(data);
                return Err(ApiError::openai(None, Some(error), None).into());
            }
            if let Some(error) = data.get("error").filter(|e| js_truthy(e)) {
                return Err(ApiError::openai(None, Some(error.clone()), None).into());
            }
            return Ok(Next::Item(data));
        }
    }

    /// The anthropic SDK's `Stream`: only the Messages event names are read, `ping` and unknown names
    /// (and events without a name) are skipped, `event: error` is an [`ApiError`] without status.
    pub async fn next_anthropic(&mut self, signal: &CancellationToken) -> anyhow::Result<Next<Value>> {
        const NAMES: [&str; 8] = [
            "completion",
            "message_start",
            "message_delta",
            "message_stop",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message",
        ];
        loop {
            let sse = match self.next(signal).await? {
                Next::Item(sse) => sse,
                Next::End => return Ok(Next::End),
                Next::Aborted => return Ok(Next::Aborted),
            };
            match sse.event.as_deref() {
                Some(name) if NAMES.contains(&name) => {
                    let data = parse_json(&sse.data).map_err(|e| anyhow::anyhow!("JSON Parse error: {e}"))?;
                    return Ok(Next::Item(data));
                }
                Some("error") => {
                    // `safeJSON(data) ?? data`
                    let body = parse_json(&sse.data)
                        .ok()
                        .filter(|v| !v.is_null())
                        .unwrap_or(Value::String(sse.data));
                    return Err(ApiError::anthropic(None, Some(body), None).into());
                }
                _ => continue,
            }
        }
    }
}

/// Lets an adapter written as straight-line async code hand out events one at a time, like the TS
/// async generators: the body runs only while the consumer asks for the next event, and dropping
/// the stream drops the request.
pub struct Yielder {
    slot: Arc<Mutex<Option<StreamEvent>>>,
}

impl Yielder {
    /// `yield ev`: hand the event over and wait until the consumer asks for the next one.
    pub async fn emit(&self, ev: StreamEvent) {
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(ev);
        YieldNow(false).await
    }
}

struct YieldNow(bool);

impl Future for YieldNow {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        // No waker needed: the generator returns the event, and the consumer polls again for more.
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            Poll::Pending
        }
    }
}

type Body = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;

struct Generator {
    body: Option<Body>,
    slot: Arc<Mutex<Option<StreamEvent>>>,
}

impl Stream for Generator {
    type Item = anyhow::Result<StreamEvent>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        let Some(body) = this.body.as_mut() else {
            return Poll::Ready(None);
        };
        let r = body.as_mut().poll(cx);
        if let Some(ev) = this.slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
            if r.is_ready() {
                this.body = None;
            }
            return Poll::Ready(Some(Ok(ev)));
        }
        match r {
            Poll::Ready(result) => {
                this.body = None;
                Poll::Ready(result.err().map(Err))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// A stream of the events `body` emits through its [`Yielder`]; an `Err` it returns is the last item.
pub fn generator<F, Fut>(body: F) -> BoxStream<'static, anyhow::Result<StreamEvent>>
where
    F: FnOnce(Yielder) -> Fut,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let slot = Arc::new(Mutex::new(None));
    let fut = body(Yielder { slot: slot.clone() });
    Box::pin(Generator {
        body: Some(Box::pin(fut)),
        slot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messages_are_the_sdks() {
        let anthropic = ApiError::from_response(
            Flavor::Anthropic,
            401,
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
        );
        assert_eq!(
            anthropic.message,
            r#"401 {"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#
        );
        assert_eq!(anthropic.error_type, Some(json!("authentication_error")));
        let openai = |status, body: &str| ApiError::from_response(Flavor::OpenAi, status, body).message;
        assert_eq!(
            openai(429, r#"{"error":{"message":"Rate limit reached","type":"x"}}"#),
            "429 Rate limit reached"
        );
        assert_eq!(
            openai(404, r#"{"error":"model 'x' not found"}"#),
            r#"404 "model 'x' not found""#
        );
        assert_eq!(
            openai(404, r#"{"detail":"Not Found"}"#),
            r#"404 {"detail":"Not Found"}"#
        );
        assert_eq!(openai(500, "Internal Server Error"), "500 Internal Server Error");
        assert_eq!(openai(502, ""), "502 status code (no body)");
        assert_eq!(openai(400, r#"{"error":{"message":"x","code":1.0}}"#), "400 x");
        let e = ApiError::from_response(
            Flavor::OpenAi,
            400,
            r#"{"error":{"message":"m","code":400,"param":null}}"#,
        );
        assert_eq!(
            (e.code, e.param, e.status, e.kind),
            (Some(json!(400)), Some(Value::Null), Some(400), ApiErrorKind::Status)
        );
        assert_eq!(
            ApiError::openai(None, Some(json!({"message": "in stream"})), None).message,
            "in stream"
        );
        assert_eq!(ApiError::openai(None, None, None).message, "(no status code or body)");
        assert_eq!(
            ApiError::anthropic(
                None,
                Some(json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}})),
                None
            )
            .message,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
        );
    }

    #[test]
    fn js_helpers() {
        assert_eq!(parse_float("1.5abc"), 1.5);
        assert_eq!(parse_float(" 120"), 120.0);
        assert_eq!(parse_float(".5"), 0.5);
        assert_eq!(parse_float("3e2"), 300.0);
        assert!(parse_float("Wed, 21 Oct 2015 07:28:00 GMT").is_nan());
        assert!(parse_float("").is_nan());
        assert!(ms_until_date("Wed, 21 Oct 2015 07:28:00 GMT") < 0.0);
        assert!(ms_until_date("soon").is_nan());
        assert_eq!(js_string(Some(&json!(400))), "400");
        assert_eq!(js_string(None), "undefined");
        assert_eq!(js_stringify(&json!({"a": 1.0, "b": [2.5]})), r#"{"a":1,"b":[2.5]}"#);
        let r = random_unit();
        assert!((0.0..1.0).contains(&r));
    }

    #[test]
    fn url_joining_drops_one_slash() {
        let c = ApiClient::openai("k", "https://h/v1/", 0, None, None);
        assert_eq!(
            c.url("/chat/completions").unwrap().as_str(),
            "https://h/v1/chat/completions"
        );
        let c = ApiClient::openai("k", "https://h/v1", 0, None, None);
        assert_eq!(c.url("/responses").unwrap().as_str(), "https://h/v1/responses");
        assert!(ApiClient::openai("k", "not a url", 0, None, None).url("/x").is_err());
    }
}
