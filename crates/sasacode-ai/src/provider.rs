//! What a provider adapter implements, and the registry the agent looks adapters up in.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::types::{
    AssistantMessage, Message, ModelInfo, Price, ProviderConfig, SamplingOptions, ThinkingLevel, ToolSpec,
};

/// Replaces the HTTP client's send, e.g. for a provider that wraps another and must adjust the
/// request on the wire (the openai-codex plugin drops fields its backend rejects), or to observe
/// requests (diagnostics). A `before_request` hook composes one around the previous one.
pub trait Fetch: Send + Sync {
    fn send(&self, req: reqwest::Request) -> BoxFuture<'static, anyhow::Result<reqwest::Response>>;
}

/// The plain transport: the shared client for model APIs (redirects not followed).
pub struct DefaultFetch;

impl Fetch for DefaultFetch {
    fn send(&self, req: reqwest::Request) -> BoxFuture<'static, anyhow::Result<reqwest::Response>> {
        Box::pin(async move { Ok(crate::http::api_client().execute(req).await?) })
    }
}

/// Send through `fetch` when one is set, else the shared client.
pub async fn send(fetch: Option<&Arc<dyn Fetch>>, req: reqwest::Request) -> anyhow::Result<reqwest::Response> {
    match fetch {
        Some(f) => f.send(req).await,
        None => DefaultFetch.send(req).await,
    }
}

#[derive(Clone)]
pub struct Request {
    pub model: ModelInfo,
    pub api_key: Option<String>,
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub thinking: Option<ThinkingLevel>,
    pub max_tokens: Option<u64>,
    pub max_retries: Option<u32>,
    pub sampling: Option<SamplingOptions>,
    /// Aborts the request (AbortSignal).
    pub signal: CancellationToken,
    pub fetch: Option<Arc<dyn Fetch>>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("model", &self.model.spec())
            .field("messages", &self.messages.len())
            .field("tools", &self.tools.len())
            .finish_non_exhaustive()
    }
}

/// One step of a streamed reply. `partial` is the reply so far (shared: cheap to pass around).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    Start {
        partial: Arc<AssistantMessage>,
    },
    TextDelta {
        index: usize,
        delta: String,
        partial: Arc<AssistantMessage>,
    },
    ThinkingDelta {
        index: usize,
        delta: String,
        partial: Arc<AssistantMessage>,
    },
    ToolcallStart {
        index: usize,
        id: String,
        name: String,
        partial: Arc<AssistantMessage>,
    },
    ToolcallDelta {
        index: usize,
        delta: String,
        partial: Arc<AssistantMessage>,
    },
    Done {
        message: AssistantMessage,
    },
}

impl StreamEvent {
    pub fn partial(&self) -> Option<&Arc<AssistantMessage>> {
        match self {
            StreamEvent::Start { partial }
            | StreamEvent::TextDelta { partial, .. }
            | StreamEvent::ThinkingDelta { partial, .. }
            | StreamEvent::ToolcallStart { partial, .. }
            | StreamEvent::ToolcallDelta { partial, .. } => Some(partial),
            StreamEvent::Done { .. } => None,
        }
    }
}

/// A model the provider's Models API listed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
}

/// A provider turns a request into a stream of events ending with `Done`. Errors end the stream as
/// an `Err` item (TS: thrown); a context overflow is an [`ContextOverflowError`] inside the anyhow error.
#[async_trait]
pub trait Provider: Send + Sync {
    fn api(&self) -> &str;
    fn stream(&self, req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>>;
    /// The models this provider offers, for /model. `None`: not implemented here, so the
    /// provider's Models API is asked (GET /models for the OpenAI and Anthropic formats).
    async fn list_models(
        &self,
        _config: &ProviderConfig,
        _api_key: Option<&str>,
        _signal: CancellationToken,
    ) -> Option<anyhow::Result<Vec<ListedModel>>> {
        None
    }
}

/// The request did not fit in the model's context window.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ContextOverflowError(pub String);

/// Is this error a context overflow (anywhere in its chain)?
pub fn is_context_overflow(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<ContextOverflowError>())
}

/// The connection broke while the reply was streaming in: a cut reply to ask again for, not an
/// error the provider sent (`droppedMidReply`). Decided by the error's type, not its text.
pub fn dropped_mid_reply(e: &anyhow::Error) -> bool {
    crate::transport::is_cut_reply(e)
}

type Registry = RwLock<HashMap<String, Arc<dyn Provider>>>;

fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register the adapter for an api name (a plugin may replace a built-in one).
pub fn register_provider(api: &str, provider: Arc<dyn Provider>) {
    registry()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(api.to_string(), provider);
}

/// The adapter for `api`, built-ins included (`getProvider`).
pub fn get_provider(api: &str) -> anyhow::Result<Arc<dyn Provider>> {
    crate::register_builtins();
    registry()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(api)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no provider registered for api \"{api}\""))
}

pub(crate) fn register_provider_if_absent(api: &str, provider: Arc<dyn Provider>) {
    registry()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .entry(api.to_string())
        .or_insert(provider);
}
