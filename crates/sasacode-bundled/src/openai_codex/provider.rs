//! The ChatGPT-plan backend (chatgpt.com/backend-api/codex) speaks the Responses API, so this
//! provider (`openai-codex/provider.ts`) hands requests to the core openai-responses implementation
//! and only adapts the wire: the plan's token and account header, and the fields this backend rejects.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::{self, BoxStream};
use futures::{FutureExt, StreamExt};
use regex::Regex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use sasacode_ai::transport::ApiError;
use sasacode_ai::{DefaultFetch, Fetch, ListedModel, Provider, ProviderConfig, Request, StreamEvent, get_provider};
use sasacode_plugin_api::sasacode_home;

use super::auth::{CodexTokens, codex_tokens};

/// The backend hides models from clients older than their release, so it is told the latest Codex
/// CLI version (npm @openai/codex), looked up once per process. When npm does not answer, the
/// version found last time is used, and this one only when there has never been an answer.
pub const CODEX_CLIENT_VERSION: &str = "0.162.1";

/// Where the last version npm reported is kept.
pub fn codex_version_path() -> PathBuf {
    sasacode_home().join("codex-version")
}

static RELEASE: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    Regex::new(r"^\d+\.\d+\.\d+$").expect("fixed pattern")
});

/// The latest Codex CLI release on npm, else the one last seen there, else CODEX_CLIENT_VERSION.
pub async fn latest_codex_version(fetch: Option<&Arc<dyn Fetch>>, path: &Path) -> String {
    let registry = std::env::var("SASACODE_NPM_REGISTRY").unwrap_or_else(|_| "https://registry.npmjs.org".into());
    let registry = registry.strip_suffix('/').unwrap_or(&registry);
    let asked = async {
        let req = sasacode_ai::http::client()
            .get(format!("{registry}/-/package/@openai/codex/dist-tags"))
            .build()
            .ok()?;
        let res = sasacode_ai::send(fetch, req).await.ok()?;
        if !res.status().is_success() {
            return None;
        }
        let body: Value = res.json().await.ok()?;
        body.get("latest").and_then(Value::as_str).map(str::to_string)
    };
    let latest = tokio::time::timeout(Duration::from_secs(3), asked).await.ok().flatten();
    if let Some(latest) = latest.filter(|l| RELEASE.is_match(l)) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, format!("{latest}\n"));
        return latest;
    }
    if let Ok(last) = std::fs::read_to_string(path) {
        let last = last.trim();
        if RELEASE.is_match(last) {
            return last.to_string();
        }
    }
    CODEX_CLIENT_VERSION.to_string()
}

static LOOKED_UP: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();

fn client_version() -> BoxFuture<'static, String> {
    async {
        LOOKED_UP
            .get_or_init(|| async { latest_codex_version(None, &codex_version_path()).await })
            .await
            .clone()
    }
    .boxed()
}

/// Shown when signed in but the backend's model list cannot be fetched.
pub const FALLBACK_MODELS: [&str; 8] = [
    "gpt-6.1-sol",
    "gpt-6-astra",
    "gpt-6-sol",
    "gpt-6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
];

/// Fields the public Responses API takes but this backend refuses (Codex never sends them).
const UNSUPPORTED: [&str; 5] = ["max_output_tokens", "temperature", "top_p", "metadata", "user"];

const NOT_SIGNED_IN: &str = "Not signed in to a ChatGPT plan. Run: sasacode login";

fn headers(
    tokens: &CodexTokens,
    base: Option<&BTreeMap<String, String>>,
    session_id: &str,
    version: &str,
) -> anyhow::Result<BTreeMap<String, String>> {
    let Some(account) = tokens.account_id.as_deref() else {
        anyhow::bail!("The ChatGPT login has no account id. Run: sasacode login");
    };
    let mut h = base.cloned().unwrap_or_default();
    h.insert("chatgpt-account-id".into(), account.into());
    h.insert("originator".into(), "sasacode".into());
    h.insert("OpenAI-Beta".into(), "responses=experimental".into());
    // The backend gates models on /responses by this too, not only on /models.
    h.insert("version".into(), version.into());
    h.insert("session-id".into(), session_id.into());
    Ok(h)
}

/// Rewrites the JSON body on its way out: drops unsupported fields, keys the prompt cache by session.
pub struct CodexFetch {
    pub session_id: String,
    pub inner: Arc<dyn Fetch>,
}

impl Fetch for CodexFetch {
    fn send(&self, mut req: reqwest::Request) -> BoxFuture<'static, anyhow::Result<reqwest::Response>> {
        let inner = self.inner.clone();
        let body = req.body().and_then(|b| b.as_bytes()).map(<[u8]>::to_vec);
        let rewritten = body.map(|bytes| -> anyhow::Result<Vec<u8>> {
            let mut body: Value = serde_json::from_slice(&bytes)?;
            if let Some(o) = body.as_object_mut() {
                for k in UNSUPPORTED {
                    o.shift_remove(k);
                }
                o.insert("store".into(), Value::Bool(false));
                o.insert("prompt_cache_key".into(), Value::String(self.session_id.clone()));
            }
            Ok(serde_json::to_vec(&body)?)
        });
        Box::pin(async move {
            if let Some(bytes) = rewritten {
                *req.body_mut() = Some(bytes?.into());
                req.headers_mut().remove(reqwest::header::CONTENT_LENGTH);
            }
            inner.send(req).await
        })
    }
}

/// Where the provider gets the session id, the plan's tokens and the client version.
pub type SessionIdFn = Arc<dyn Fn() -> String + Send + Sync>;
pub type TokensFn = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<Option<CodexTokens>>> + Send + Sync>;
pub type VersionFn = Arc<dyn Fn() -> BoxFuture<'static, String> + Send + Sync>;

/// What [`codex_provider_with`] uses instead of the stored login, the network and npm (tests).
#[derive(Clone, Default)]
pub struct ProviderDeps {
    pub tokens: Option<TokensFn>,
    pub fetch: Option<Arc<dyn Fetch>>,
    pub version: Option<VersionFn>,
}

#[derive(Clone)]
pub struct CodexProvider {
    session_id: SessionIdFn,
    tokens: TokensFn,
    fetch: Option<Arc<dyn Fetch>>,
    version: VersionFn,
}

pub fn codex_provider(session_id: SessionIdFn) -> CodexProvider {
    codex_provider_with(session_id, ProviderDeps::default())
}

pub fn codex_provider_with(session_id: SessionIdFn, deps: ProviderDeps) -> CodexProvider {
    CodexProvider {
        session_id,
        tokens: deps.tokens.unwrap_or_else(|| Arc::new(codex_tokens)),
        fetch: deps.fetch,
        version: deps.version.unwrap_or_else(|| Arc::new(client_version)),
    }
}

/// A 401 or 403 from the backend: the login is no good any more.
fn rejected(e: anyhow::Error) -> anyhow::Error {
    let status = e
        .chain()
        .find_map(|c| c.downcast_ref::<ApiError>())
        .and_then(|a| a.status);
    match status {
        Some(status @ (401 | 403)) => {
            anyhow::anyhow!("ChatGPT rejected the login ({status}): {e}. Run: sasacode login")
        }
        _ => e,
    }
}

fn failed(e: anyhow::Error) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
    stream::once(async move { Err(e) }).boxed()
}

#[async_trait]
impl Provider for CodexProvider {
    fn api(&self) -> &str {
        "openai-codex"
    }

    fn stream(&self, req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        let this = self.clone();
        let start = async move {
            let t = match (this.tokens)().await {
                Ok(Some(t)) => t,
                Ok(None) => return failed(anyhow::anyhow!(NOT_SIGNED_IN)),
                Err(e) => return failed(e),
            };
            let id = (this.session_id)();
            let version = (this.version)().await;
            let headers = match headers(&t, req.model.headers.as_ref(), &id, &version) {
                Ok(h) => h,
                Err(e) => return failed(e),
            };
            let inner = req
                .fetch
                .clone()
                .or_else(|| this.fetch.clone())
                .unwrap_or_else(|| Arc::new(DefaultFetch));
            let mut model = req.model.clone();
            model.headers = Some(headers);
            let next = Request {
                api_key: Some(t.access.clone()),
                model,
                fetch: Some(Arc::new(CodexFetch { session_id: id, inner })),
                // A usage limit is not fixed by retrying for minutes.
                max_retries: Some(req.max_retries.unwrap_or(2).min(2)),
                ..req
            };
            match get_provider("openai-responses") {
                Ok(p) => p.stream(next).map(|item| item.map_err(rejected)).boxed(),
                Err(e) => failed(e),
            }
        };
        stream::once(start).flatten().boxed()
    }

    async fn list_models(
        &self,
        config: &ProviderConfig,
        _api_key: Option<&str>,
        _signal: CancellationToken,
    ) -> Option<anyhow::Result<Vec<ListedModel>>> {
        Some(self.models(config).await)
    }
}

fn listed(id: &str) -> ListedModel {
    ListedModel {
        id: id.to_string(),
        context_window: None,
        max_output: None,
        price: None,
    }
}

impl CodexProvider {
    async fn models(&self, config: &ProviderConfig) -> anyhow::Result<Vec<ListedModel>> {
        // Not signed in: nothing to offer (the models would only fail).
        let t = (self.tokens)().await.ok().flatten();
        let Some((access, account)) = t.and_then(|t| t.account_id.clone().map(|a| (t.access, a))) else {
            return Ok(Vec::new());
        };
        let base = config.base_url.as_deref().unwrap_or("undefined");
        let version = (self.version)().await;
        let req = sasacode_ai::http::client()
            .get(format!("{base}/models?client_version={version}"))
            .header("Authorization", format!("Bearer {access}"))
            .header("chatgpt-account-id", account)
            .header("originator", "sasacode")
            .build()?;
        let res = sasacode_ai::send(self.fetch.as_ref(), req).await?;
        if !res.status().is_success() {
            return Ok(FALLBACK_MODELS.iter().map(|id| listed(id)).collect());
        }
        let body: Value = res.json().await?;
        let models: Vec<ListedModel> = body
            .get("models")
            .and_then(Value::as_array)
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            // Utility models (e.g. automatic review) are marked hidden.
            .filter(|m| {
                let visibility = m.get("visibility").and_then(Value::as_str);
                visibility != Some("hide") && visibility != Some("hidden")
            })
            .filter_map(|m| {
                let slug = m.get("slug").and_then(Value::as_str).filter(|s| !s.is_empty())?;
                let mut model = listed(slug);
                model.context_window = m.get("context_window").and_then(Value::as_f64).map(|w| w as u64);
                Some(model)
            })
            .collect();
        if models.is_empty() {
            Ok(FALLBACK_MODELS.iter().map(|id| listed(id)).collect())
        } else {
            Ok(models)
        }
    }
}
