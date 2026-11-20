//! Listing models from the providers' Models APIs (`packages/cli/src/list-models.ts`). The TS used the
//! Anthropic and OpenAI SDKs only for this; here it is plain HTTP through `sasacode_ai::transport::ApiClient`,
//! which sends what the SDKs sent (rust/research/llm-sdks.md): the same headers and auth, one retry,
//! Anthropic's pagination (`limit=100`, then `after_id` while `has_more`), OpenAI's single page.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use sasacode_ai::transport::ApiClient;
use sasacode_ai::{Price, ProviderConfig};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedModel {
    pub id: String,
    /// The context the server actually uses for this model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// The most the model supports, when the effective size is not known (Ollama without num_ctx, LM Studio before loading).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u64>,
    /// Endpoints the model answers on, when the provider says (e.g. Command Code: "/messages").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoints: Option<Vec<String>>,
    /// List price in USD per 1M tokens, when the listing reports one (OpenRouter's `pricing`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
}

/// What a provider implementation's own `list_models` returns (no sizes beyond these).
impl From<sasacode_ai::ListedModel> for ListedModel {
    fn from(m: sasacode_ai::ListedModel) -> Self {
        ListedModel {
            id: m.id,
            context_window: m.context_window,
            max_output: m.max_output,
            price: m.price,
            ..Default::default()
        }
    }
}

/// `API_ENDPOINT`: the path each API format is served at.
pub const API_ENDPOINT: &[(&str, &str)] = &[
    ("anthropic", "/messages"),
    ("openai-chat", "/chat/completions"),
    ("openai-responses", "/responses"),
];

/// `API_ENDPOINT[api]`.
pub fn api_endpoint(api: &str) -> Option<&'static str> {
    API_ENDPOINT.iter().find(|(a, _)| *a == api).map(|(_, e)| *e)
}

/// OpenAI-style lists include embedding, audio and image models that cannot drive an agent.
/// `/embed|whisper|tts|dall-e|gpt-image|moderation|transcribe|realtime|audio|search-preview|^babbage|^davinci/i`
/// (ASCII case folding, as a JS regex without the u flag).
fn not_chat(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    [
        "embed",
        "whisper",
        "tts",
        "dall-e",
        "gpt-image",
        "moderation",
        "transcribe",
        "realtime",
        "audio",
        "search-preview",
    ]
    .iter()
    .any(|w| id.contains(w))
        || id.starts_with("babbage")
        || id.starts_with("davinci")
}

/// JS `\s` (and what `trim` removes): WhiteSpace and LineTerminator.
pub(crate) fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// `Number(s)` for a string: trimmed, "" is 0, hex/octal/binary literals, `Infinity`, otherwise a
/// decimal literal or NaN.
pub(crate) fn js_to_number(s: &str) -> f64 {
    let t = s.trim_matches(is_js_space);
    if t.is_empty() {
        return 0.0;
    }
    for (prefix, radix) in [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)] {
        if let Some(rest) = t.strip_prefix(prefix) {
            if rest.is_empty() || !rest.chars().all(|c| c.is_digit(radix)) {
                return f64::NAN;
            }
            return rest
                .chars()
                .filter_map(|c| c.to_digit(radix))
                .fold(0.0, |acc, d| acc * radix as f64 + d as f64);
        }
    }
    let (sign, body) = match t.as_bytes()[0] {
        b'+' => (1.0, &t[1..]),
        b'-' => (-1.0, &t[1..]),
        _ => (1.0, t),
    };
    if body == "Infinity" {
        return sign * f64::INFINITY;
    }
    let starts_ok = body.bytes().next().is_some_and(|b| b.is_ascii_digit() || b == b'.');
    let chars_ok = body
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'));
    if !starts_ok || !chars_ok {
        return f64::NAN;
    }
    body.parse::<f64>().map(|v| sign * v).unwrap_or(f64::NAN)
}

/// `typeof v === "number" && v > 0 ? v : undefined`.
fn num(v: Option<&Value>) -> Option<u64> {
    v.and_then(Value::as_f64).filter(|n| *n > 0.0).map(|n| n as u64)
}

/// OpenRouter's `pricing` (per token, decimal strings like "0.0000015") as `ModelInfo.price` (USD per
/// 1M tokens). A zero or missing price is "unknown", not free: returning None keeps it from
/// overriding a price the built-in table or a config override already knows.
pub fn parse_pricing(pricing: Option<&Value>) -> Option<Price> {
    let p = pricing?.as_object()?;
    let per_million = |v: Option<&Value>| -> Option<f64> {
        let n = match v {
            Some(Value::String(s)) => js_to_number(s),
            Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
            _ => f64::NAN,
        };
        (n.is_finite() && n > 0.0).then_some(n * 1e6)
    };
    let input = per_million(p.get("prompt"));
    let output = per_million(p.get("completion"));
    if input.is_none() && output.is_none() {
        return None;
    }
    Some(Price {
        input: input.unwrap_or(0.0),
        output: output.unwrap_or(0.0),
        cache_read: per_million(p.get("input_cache_read")),
        cache_write: per_million(p.get("input_cache_write")),
    })
}

/// A Bun `fetch` given an AbortSignal: rejects when the signal fires. Every signal sasacode passes
/// here is a timeout (`AbortSignal.timeout`), hence the text.
pub(crate) async fn fetch(
    signal: &CancellationToken,
    req: reqwest::RequestBuilder,
) -> anyhow::Result<reqwest::Response> {
    tokio::select! {
        biased;
        _ = signal.cancelled() => Err(timed_out()),
        r = req.send() => Ok(r?),
    }
}

pub(crate) fn timed_out() -> anyhow::Error {
    anyhow::anyhow!("The operation timed out.")
}

/// `await res.text()`, also ended by the signal.
pub(crate) async fn text(signal: &CancellationToken, res: reqwest::Response) -> anyhow::Result<String> {
    tokio::select! {
        biased;
        _ = signal.cancelled() => Err(timed_out()),
        r = res.text() => Ok(r?),
    }
}

/// `await res.json()`, also ended by the signal.
pub(crate) async fn json(signal: &CancellationToken, res: reqwest::Response) -> anyhow::Result<Value> {
    let bytes = tokio::select! {
        biased;
        _ = signal.cancelled() => return Err(timed_out()),
        r = res.bytes() => r?,
    };
    Ok(serde_json::from_slice(&bytes)?)
}

/// A request with these headers set (a `fetch` `headers` record).
pub(crate) fn with_headers<'a>(
    mut req: reqwest::RequestBuilder,
    headers: impl IntoIterator<Item = (&'a String, &'a String)>,
) -> reqwest::RequestBuilder {
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    req
}

/// A signal that fires after `d` (`AbortSignal.timeout`). The timer stops when this is dropped.
pub(crate) struct Deadline {
    pub token: CancellationToken,
    timer: tokio::task::JoinHandle<()>,
}

impl Deadline {
    pub fn after(d: Duration) -> Deadline {
        let token = CancellationToken::new();
        let t = token.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(d).await;
            t.cancel();
        });
        Deadline { token, timer }
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        self.timer.abort();
    }
}

/// `readEnv`: trimmed, absent when empty.
fn read_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The SDKs' `baseURL`: the configured one, else (only when none is configured) the SDK's
/// environment variable, else its default.
fn sdk_base(configured: Option<&str>, env: &str, default: &str) -> String {
    match configured {
        Some(b) if !b.is_empty() => b.to_string(),
        Some(_) => default.to_string(),
        None => read_env(env).unwrap_or_else(|| default.to_string()),
    }
}

/// `encodeURIComponent`, for a query value.
fn encode_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The SDKs' `defaultParseResponse`: JSON when the content type says so, else the text.
async fn sdk_body(signal: &CancellationToken, res: reqwest::Response) -> anyhow::Result<Value> {
    let is_json = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| {
            let media = ct.split(';').next().unwrap_or("").trim();
            media.contains("application/json") || media.ends_with("+json")
        })
        .unwrap_or(false);
    if is_json {
        json(signal, res).await
    } else {
        Ok(Value::String(text(signal, res).await?))
    }
}

/// `body.data || []` of a page.
fn page_items(body: &Value) -> Vec<Value> {
    body.get("data").and_then(Value::as_array).cloned().unwrap_or_default()
}

/// The item's id; items without a string id are left out.
fn item_id(x: &Value) -> Option<String> {
    x.get("id").and_then(Value::as_str).map(str::to_string)
}

/// GET <baseUrl>/models (the Models API) for provider `name`. Fails on network or auth errors.
/// `lmstudio`: LM Studio's native list, when it was just fetched (not asked for again).
pub async fn list_models(
    name: &str,
    p: &ProviderConfig,
    api_key: Option<&str>,
    signal: &CancellationToken,
    lmstudio: Option<Vec<LmStudioModel>>,
) -> anyhow::Result<Vec<ListedModel>> {
    let mut out: Vec<ListedModel> = Vec::new();
    if p.api == "anthropic" {
        let mut base = sdk_base(p.base_url.as_deref(), "ANTHROPIC_BASE_URL", "https://api.anthropic.com");
        let key = api_key.filter(|k| !k.is_empty());
        // The Anthropic variables are the anthropic provider's: another provider uses its own apiKeyEnv.
        let env = name == "anthropic";
        // No host chosen and only the `ant auth login` profile to authenticate: the profile's host.
        let chosen = p
            .base_url
            .as_deref()
            .map_or(read_env("ANTHROPIC_BASE_URL").is_some(), |b| !b.is_empty());
        let keyless = key.is_none()
            && (!env
                || ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"]
                    .iter()
                    .all(|v| read_env(v).is_none()));
        if !chosen
            && keyless
            && let Some(profile_base) =
                crate::anthropic_profile::listing_base_url(&crate::anthropic_profile::Env::process())
        {
            base = profile_base;
        }
        let client = ApiClient::anthropic(key, env, &base, 1, p.headers.as_ref(), None);
        let mut path = "/v1/models?limit=100".to_string();
        loop {
            let res = client.get(&path, signal).await?;
            let body = sdk_body(signal, res).await?;
            let items = page_items(&body);
            for x in &items {
                let Some(id) = item_id(x) else { continue };
                out.push(ListedModel {
                    id,
                    context_window: num(x.get("max_input_tokens")),
                    max_output: num(x.get("max_tokens")),
                    endpoints: Some(vec!["/messages".to_string()]),
                    ..Default::default()
                });
            }
            // The SDK's Page: another page only while has_more is true, this one had items and a cursor.
            let has_more = body.get("has_more").is_some_and(sasacode_ai::transport::js_truthy);
            let cursor = body
                .get("last_id")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
                .map(str::to_string);
            match cursor {
                Some(c) if has_more && !items.is_empty() => {
                    path = format!("/v1/models?limit=100&after_id={}", encode_component(&c));
                }
                _ => break,
            }
        }
    } else {
        let base = sdk_base(p.base_url.as_deref(), "OPENAI_BASE_URL", "https://api.openai.com/v1");
        let key = api_key.filter(|k| !k.is_empty()).unwrap_or("none");
        let client = ApiClient::openai(
            key,
            matches!(name, "openai" | "openai-chat"),
            &base,
            1,
            p.headers.as_ref(),
            None,
        );
        let res = client.get("/models", signal).await?;
        let body = sdk_body(signal, res).await?;
        for x in page_items(&body) {
            let Some(id) = item_id(&x) else { continue };
            let top = x.get("top_provider"); // OpenRouter
            let meta = x.get("meta"); // llama.cpp
            let get = |v: Option<&Value>, k: &str| v.and_then(|v| v.get(k)).cloned();
            out.push(ListedModel {
                id,
                max_context: num(get(meta, "n_ctx_train").as_ref()),
                // max_model_len: vLLM's window (input and output together).
                context_window: num(x.get("context_length"))
                    .or_else(|| num(x.get("context_window")))
                    .or_else(|| num(x.get("max_input_tokens")))
                    .or_else(|| num(x.get("max_model_len")))
                    .or_else(|| num(get(meta, "n_ctx").as_ref())),
                max_output: num(get(top, "max_completion_tokens").as_ref()).or_else(|| num(x.get("max_output_tokens"))),
                endpoints: x
                    .get("supported_endpoints")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|e| e.as_str().map(str::to_string)).collect()),
                price: parse_pricing(x.get("pricing")),
            });
        }
    }
    let mut chat: Vec<ListedModel> = out.into_iter().filter(|m| !not_chat(&m.id)).collect();
    if p.ollama == Some(true) {
        let loaded = ollama_loaded(p, signal).await;
        let details = futures::future::join_all(chat.iter().map(|m| ollama_details(p, m, &loaded, signal))).await;
        for (m, d) in chat.iter_mut().zip(details) {
            match d {
                OllamaSize::Context(n) => m.context_window = Some(n),
                OllamaSize::Max(n) => m.max_context = Some(n),
                OllamaSize::Unknown => {}
            }
        }
    }
    if p.llamacpp == Some(true) {
        llamacpp_context(p, &mut chat, signal).await;
    }
    if p.lmstudio == Some(true) {
        let list = match lmstudio {
            Some(l) => Some(l),
            None => lmstudio_list(p, api_key, signal).await,
        };
        lmstudio_context(&mut chat, list.as_deref());
    }
    Ok(chat)
}

/// `.replace(/\/v1\/?$/, "")`.
pub(crate) fn strip_v1(s: &str) -> &str {
    s.strip_suffix("/v1/").or_else(|| s.strip_suffix("/v1")).unwrap_or(s)
}

/// llama-server serves one model; /props has the context each request slot gets.
async fn llamacpp_context(p: &ProviderConfig, models: &mut [ListedModel], signal: &CancellationToken) {
    let root = strip_v1(p.base_url.as_deref().unwrap_or(""));
    let req = with_headers(
        sasacode_ai::http::api_client().get(format!("{root}/props")),
        p.headers.iter().flatten(),
    );
    let Ok(res) = fetch(signal, req).await else { return };
    if !res.status().is_success() {
        return;
    }
    let Ok(props) = json(signal, res).await else { return };
    let n_ctx = props
        .get("default_generation_settings")
        .and_then(|s| s.get("n_ctx"))
        .and_then(Value::as_f64);
    if let Some(n) = n_ctx.filter(|n| *n > 0.0) {
        for m in models.iter_mut() {
            m.context_window = Some(n as u64);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LmStudioModel {
    pub id: String,
    pub max_context_length: u64,
    /// Only while the model is loaded (or loading).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loaded_context_length: Option<u64>,
}

/// LM Studio's native model list (0.3.6+), or None when `root` is not LM Studio.
pub async fn lmstudio_models(
    root: &str,
    headers: &[(String, String)],
    signal: &CancellationToken,
) -> Option<Vec<LmStudioModel>> {
    let req = with_headers(
        sasacode_ai::http::api_client().get(format!("{root}/api/v0/models")),
        headers.iter().map(|(k, v)| (k, v)),
    );
    let res = fetch(signal, req).await.ok()?;
    if !res.status().is_success() {
        return None;
    }
    let body = json(signal, res).await.ok()?;
    let data = body.get("data")?.as_array()?;
    // Every entry carries max_context_length: an OpenAI-style list served at any path has none.
    if data.is_empty()
        || !data
            .iter()
            .all(|m| m.get("max_context_length").is_some_and(Value::is_number))
    {
        return None;
    }
    Some(
        data.iter()
            .map(|m| LmStudioModel {
                id: m.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
                max_context_length: m.get("max_context_length").and_then(Value::as_f64).unwrap_or(0.0) as u64,
                loaded_context_length: m.get("loaded_context_length").and_then(Value::as_f64).map(|n| n as u64),
            })
            .collect(),
    )
}

/// [`lmstudio_models`] for a configured provider.
pub async fn lmstudio_list(
    p: &ProviderConfig,
    api_key: Option<&str>,
    signal: &CancellationToken,
) -> Option<Vec<LmStudioModel>> {
    let mut headers: BTreeMap<String, String> = p.headers.clone().unwrap_or_default();
    if let Some(k) = api_key.filter(|k| !k.is_empty()) {
        headers.insert("authorization".to_string(), format!("Bearer {k}"));
    }
    let headers: Vec<(String, String)> = headers.into_iter().collect();
    lmstudio_models(strip_v1(p.base_url.as_deref().unwrap_or("")), &headers, signal).await
}

/// LM Studio's /v1/models has no sizes; /api/v0/models has each model's maximum and, while it is
/// loaded, the context it was loaded with. A model is loaded with LM Studio's own default length
/// (often 4096), not its maximum, so before that the maximum is only shown, as for Ollama without
/// num_ctx, and the size is asked again once the model has answered.
fn lmstudio_context(models: &mut [ListedModel], list: Option<&[LmStudioModel]>) {
    let native: HashMap<&str, &LmStudioModel> = list.into_iter().flatten().map(|m| (m.id.as_str(), m)).collect();
    for m in models.iter_mut() {
        match native.get(m.id.as_str()) {
            Some(n) if n.loaded_context_length.is_some_and(|l| l != 0) => m.context_window = n.loaded_context_length,
            Some(n) => m.max_context = Some(n.max_context_length),
            None => {}
        }
    }
}

fn ollama_root(p: &ProviderConfig) -> String {
    strip_v1(p.base_url.as_deref().unwrap_or("http://localhost:11434/v1")).to_string()
}

/// Models Ollama has in memory, with the context they were loaded with (OLLAMA_CONTEXT_LENGTH included).
async fn ollama_loaded(p: &ProviderConfig, signal: &CancellationToken) -> HashMap<String, u64> {
    let req = sasacode_ai::http::client().get(format!("{}/api/ps", ollama_root(p)));
    let Ok(res) = fetch(signal, req).await else {
        return HashMap::new();
    };
    let body = if res.status().is_success() {
        match json(signal, res).await {
            Ok(b) => b,
            Err(_) => return HashMap::new(),
        }
    } else {
        Value::Object(Default::default())
    };
    let mut out = HashMap::new();
    for m in body.get("models").and_then(Value::as_array).into_iter().flatten() {
        let ctx = m.get("context_length").and_then(Value::as_f64).filter(|n| *n != 0.0);
        if let Some(ctx) = ctx {
            let name = m
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| m.get("model").and_then(Value::as_str))
                .unwrap_or("");
            out.insert(name.to_string(), ctx as u64);
        }
    }
    out
}

enum OllamaSize {
    Context(u64),
    Max(u64),
    Unknown,
}

/// `/^num_ctx\s+(\d+)/m`: at the start of any line (JS line terminators), JS whitespace, ASCII digits.
fn num_ctx(parameters: &str) -> Option<&str> {
    let mut starts = vec![0];
    starts.extend(
        parameters
            .char_indices()
            .filter(|(_, c)| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
            .map(|(i, c)| i + c.len_utf8()),
    );
    for s in starts {
        let Some(rest) = parameters[s..].strip_prefix("num_ctx") else {
            continue;
        };
        let after_space = rest.trim_start_matches(is_js_space);
        if after_space.len() == rest.len() {
            continue;
        }
        let digits = after_space.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            return Some(&after_space[..digits]);
        }
    }
    None
}

/// Ollama's /v1/models has no sizes; its native /api/show has the trained context length and,
/// when the Modelfile sets it, num_ctx (what requests actually get). A loaded model's size in
/// /api/ps is what it really runs with, whatever set it.
async fn ollama_details(
    p: &ProviderConfig,
    m: &ListedModel,
    loaded: &HashMap<String, u64>,
    signal: &CancellationToken,
) -> OllamaSize {
    if let Some(running) = loaded.get(&m.id) {
        return OllamaSize::Context(*running);
    }
    let root = ollama_root(p);
    let body = sasacode_ai::json::stringify(&serde_json::json!({ "model": m.id }));
    // A string body: what Bun's fetch labels it.
    let req = sasacode_ai::http::client()
        .post(format!("{root}/api/show"))
        .header("content-type", "text/plain;charset=UTF-8")
        .body(body);
    let Ok(res) = fetch(signal, req).await else {
        return OllamaSize::Unknown;
    };
    if !res.status().is_success() {
        return OllamaSize::Unknown;
    }
    let Ok(info) = json(signal, res).await else {
        return OllamaSize::Unknown;
    };
    let trained = info
        .get("model_info")
        .and_then(Value::as_object)
        .and_then(|o| o.iter().find(|(k, _)| k.ends_with(".context_length")).map(|(_, v)| v));
    let parameters = info.get("parameters").and_then(Value::as_str).unwrap_or("");
    if let Some(n) = num_ctx(parameters) {
        OllamaSize::Context(n.parse::<f64>().unwrap_or(0.0) as u64)
    } else if let Some(t) = trained.and_then(Value::as_f64) {
        OllamaSize::Max(t as u64)
    } else {
        OllamaSize::Unknown
    }
}

#[cfg(test)]
#[path = "test_server.rs"]
pub(crate) mod test_server;

#[cfg(test)]
mod tests {
    use super::test_server::{Res, TestServer};
    use super::*;
    use serde_json::json;

    // list-models.test.ts

    #[tokio::test]
    async fn a_models_listing_carries_the_providers_pricing() {
        // The TS replaced `fetch` for any URL; a local server answers the same body.
        let body = json!({
            "object": "list",
            "data": [
                {
                    "id": "openai/gpt-5.5",
                    "object": "model",
                    "created": 0,
                    "owned_by": "openai",
                    "context_length": 400_000,
                    "pricing": { "prompt": "0.0000015", "completion": "0.000006" },
                },
                { "id": "meta/llama-3.3-70b:free", "object": "model", "created": 0, "owned_by": "meta", "pricing": { "prompt": "0", "completion": "0" } },
            ],
        });
        let server = TestServer::start(move |_| Res::json(body.clone()));
        let p = ProviderConfig {
            api: "openai-chat".into(),
            base_url: Some(format!("{}/api/v1", server.url())),
            ..Default::default()
        };
        let models = list_models("openrouter", &p, Some("or-test"), &CancellationToken::new(), None)
            .await
            .unwrap();
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["openai/gpt-5.5", "meta/llama-3.3-70b:free"]
        );
        assert_eq!(
            models[0].price,
            Some(Price {
                input: 1.5,
                output: 6.0,
                cache_read: None,
                cache_write: None
            })
        );
        assert_eq!(models[0].context_window, Some(400_000));
        assert_eq!(models[1].price, None);
    }

    #[test]
    fn listing_prices_are_per_token_as_strings_the_table_is_usd_per_1m_tokens() {
        assert_eq!(
            parse_pricing(Some(&json!({ "prompt": "0.0000015", "completion": "0.000006" }))),
            Some(Price {
                input: 1.5,
                output: 6.0,
                cache_read: None,
                cache_write: None
            })
        );
        assert_eq!(
            parse_pricing(Some(&json!({
                "prompt": "0.0000025",
                "completion": "0.00001",
                "input_cache_read": "0.00000025",
                "input_cache_write": "0.000003125",
            }))),
            Some(Price {
                input: 2.5,
                output: 10.0,
                cache_read: Some(0.25),
                cache_write: Some(3.125)
            })
        );
    }

    #[test]
    fn a_zero_price_is_unknown_not_free_it_never_overrides_a_known_one() {
        assert_eq!(parse_pricing(Some(&json!({ "prompt": "0", "completion": "0" }))), None);
        assert_eq!(parse_pricing(Some(&json!({ "prompt": "0" }))), None);
        assert_eq!(parse_pricing(None), None);
    }

    #[tokio::test]
    async fn anthropic_listing_pages_with_after_id_and_sends_the_sdk_headers() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        let server = TestServer::start(move |req| {
            log.lock().unwrap().push((
                req.path.clone(),
                req.query.clone(),
                req.header("x-api-key").map(str::to_string),
                req.header("anthropic-version").map(str::to_string),
            ));
            if req.query.contains("after_id=a%2Fb") {
                Res::json(json!({ "data": [{ "type": "model", "id": "c" }], "has_more": false, "last_id": "c" }))
            } else {
                Res::json(
                    json!({ "data": [{ "type": "model", "id": "a/b", "max_tokens": 64000 }], "has_more": true, "last_id": "a/b" }),
                )
            }
        });
        let p = ProviderConfig {
            api: "anthropic".into(),
            base_url: Some(server.url()),
            ..Default::default()
        };
        let models = list_models("p", &p, Some("k"), &CancellationToken::new(), None)
            .await
            .unwrap();
        assert_eq!(models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["a/b", "c"]);
        assert_eq!(models[0].max_output, Some(64000));
        assert_eq!(models[0].endpoints, Some(vec!["/messages".to_string()]));
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, "/v1/models");
        assert_eq!(seen[0].1, "limit=100");
        assert_eq!(seen[1].1, "limit=100&after_id=a%2Fb");
        assert_eq!(seen[0].2.as_deref(), Some("k"));
        assert_eq!(seen[0].3.as_deref(), Some("2023-06-01"));
    }

    #[test]
    fn js_number_parsing() {
        assert_eq!(js_to_number("0.0000015"), 0.0000015);
        assert_eq!(js_to_number(" 12 "), 12.0);
        assert_eq!(js_to_number(""), 0.0);
        assert_eq!(js_to_number("0x10"), 16.0);
        assert!(js_to_number("12px").is_nan());
        assert!(js_to_number("+-1").is_nan());
        assert!(js_to_number("inf").is_nan());
        assert_eq!(js_to_number("-Infinity"), f64::NEG_INFINITY);
        assert_eq!(js_to_number(".5"), 0.5);
        assert_eq!(js_to_number("5."), 5.0);
    }

    #[test]
    fn num_ctx_matches_at_line_starts_only() {
        assert_eq!(
            num_ctx("num_ctx                        32768\nstop <eos>"),
            Some("32768")
        );
        assert_eq!(num_ctx("stop <eos>\r\nnum_ctx 8192"), Some("8192"));
        assert_eq!(num_ctx("x num_ctx 8192"), None);
        assert_eq!(num_ctx("num_ctx8192"), None);
        assert_eq!(num_ctx(""), None);
    }

    #[test]
    fn chat_filter_and_v1() {
        assert!(not_chat("text-embedding-3-small"));
        assert!(not_chat("Babbage-002"));
        assert!(!not_chat("gpt-5.5"));
        assert_eq!(strip_v1("http://h/v1/"), "http://h");
        assert_eq!(strip_v1("http://h/v1"), "http://h");
        assert_eq!(strip_v1("http://h/v2"), "http://h/v2");
    }
}
