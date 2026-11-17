//! What speaks at a URL: Anthropic- or OpenAI-style, and which local server (`packages/cli/src/detect.ts`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use sasacode_ai::ProviderConfig;

use crate::list_models::{fetch, json, lmstudio_models, with_headers};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedEndpoint {
    /// "openai-chat" or "anthropic".
    pub api: String,
    /// Normalized for that API's client: ".../v1" for OpenAI-style, the root for Anthropic.
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llamacpp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lmstudio: Option<bool>,
}

impl DetectedEndpoint {
    fn new(api: &str, base_url: &str) -> Self {
        DetectedEndpoint {
            api: api.to_string(),
            base_url: base_url.to_string(),
            ollama: None,
            llamacpp: None,
            lmstudio: None,
        }
    }
}

/// `.replace(/\/+$/, "")`.
fn trim_slashes(s: &str) -> &str {
    s.trim_end_matches('/')
}

/// Work out what speaks at `url` from its Models API. Anthropic lists look like
/// `{ data: [{ type: "model", … }], has_more }`; OpenAI-style ones (OpenAI, llama.cpp, vLLM,
/// LM Studio, Ollama …) like `{ object: "list", data: [{ object: "model", … }] }`.
/// OpenAI-style servers get Chat Completions, the format they all implement.
pub async fn detect_endpoint(
    url: &str,
    api_key: Option<&str>,
    signal: &CancellationToken,
) -> anyhow::Result<DetectedEndpoint> {
    let base = trim_slashes(url);
    let root = base.strip_suffix("/v1").unwrap_or(base);
    let mut headers: Vec<(String, String)> = vec![("anthropic-version".into(), "2023-06-01".into())];
    if let Some(k) = api_key.filter(|k| !k.is_empty()) {
        headers.push(("authorization".into(), format!("Bearer {k}")));
        headers.push(("x-api-key".into(), k.to_string()));
    }
    // The standard location first, so the stored base URL ends in /v1 where the server has one.
    let mut candidates = vec![format!("{root}/v1/models")];
    let second = format!("{base}/models");
    if !candidates.contains(&second) {
        candidates.push(second);
    }
    let mut last_error = "no response".to_string();
    for list_url in &candidates {
        let req = with_headers(
            sasacode_ai::http::client().get(list_url.as_str()),
            headers.iter().map(|(k, v)| (k, v)),
        );
        let res = match fetch(signal, req).await {
            Ok(r) => r,
            Err(e) => {
                last_error = format!("{e:#}");
                continue;
            }
        };
        let status = res.status().as_u16();
        if status == 401 || status == 403 {
            anyhow::bail!("{list_url} needs an API key (HTTP {status})");
        }
        if !res.status().is_success() {
            last_error = format!("{list_url}: HTTP {status}");
            continue;
        }
        let body = json(signal, res).await.ok();
        let Some((body, data)) = body
            .as_ref()
            .and_then(|b| Some((b.as_object()?, b.get("data")?.as_array()?)))
        else {
            last_error = format!("{list_url}: not a model list");
            continue;
        };
        let list_root = list_url.strip_suffix("/models").unwrap_or(list_url);
        let first = data.first();
        if body.contains_key("has_more") || first.and_then(|m| m.get("type")).and_then(Value::as_str) == Some("model") {
            return Ok(DetectedEndpoint::new(
                "anthropic",
                list_root.strip_suffix("/v1").unwrap_or(list_root),
            ));
        }
        let mut endpoint = DetectedEndpoint::new("openai-chat", list_root);
        let owner = first
            .and_then(|m| m.get("owned_by"))
            .and_then(Value::as_str)
            .unwrap_or("");
        // `meta.n_ctx_train !== undefined`: present at all, even null.
        let n_ctx_train = first
            .and_then(|m| m.get("meta"))
            .and_then(|m| m.get("n_ctx_train"))
            .is_some();
        if owner == "llamacpp" || n_ctx_train {
            endpoint.llamacpp = Some(true);
        } else if owner == "library" || is_ollama(root, signal).await {
            endpoint.ollama = Some(true);
        }
        // LM Studio's /v1/models looks like any other; its native list (0.3.6+) is what tells it apart.
        else if lmstudio_models(root, &headers, signal).await.is_some() {
            endpoint.lmstudio = Some(true);
        }
        return Ok(endpoint);
    }
    anyhow::bail!("could not find a model list at {url} ({last_error})")
}

async fn is_ollama(root: &str, signal: &CancellationToken) -> bool {
    let req = sasacode_ai::http::client().get(format!("{root}/api/version"));
    let Ok(res) = fetch(signal, req).await else {
        return false;
    };
    if !res.status().is_success() {
        return false;
    }
    json(signal, res)
        .await
        .is_ok_and(|v| v.get("version").is_some_and(Value::is_string))
}

/// Fill in `api` (and the server flavour) for a provider configured with only a URL:
/// `{ ...p, api, baseUrl, ...flags }`.
pub fn apply_detected(p: &ProviderConfig, d: &DetectedEndpoint) -> ProviderConfig {
    let mut out = p.clone();
    out.api = d.api.clone();
    out.base_url = Some(d.base_url.clone());
    if d.ollama == Some(true) {
        out.ollama = Some(true);
    }
    if d.llamacpp == Some(true) {
        out.llamacpp = Some(true);
    }
    if d.lmstudio == Some(true) {
        out.lmstudio = Some(true);
    }
    out
}

/// The provider entry `endpoint add` writes to config.json, in the TS key order:
/// `{ api, baseUrl, <flag>: true, apiKeyEnv }`.
pub fn detected_entry(d: &DetectedEndpoint, key_env: Option<&str>) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("api".into(), Value::String(d.api.clone()));
    m.insert("baseUrl".into(), Value::String(d.base_url.clone()));
    for (name, on) in [("ollama", d.ollama), ("llamacpp", d.llamacpp), ("lmstudio", d.lmstudio)] {
        if on == Some(true) {
            m.insert(name.into(), Value::Bool(true));
        }
    }
    if let Some(k) = key_env.filter(|k| !k.is_empty()) {
        m.insert("apiKeyEnv".into(), Value::String(k.to_string()));
    }
    m
}
