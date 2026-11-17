//! Provider-neutral message format (`packages/ai/src/types.ts`). Sessions store these, so a
//! conversation can move between providers mid-session (FR-P04).
//!
//! JSON compatibility with the TypeScript version (rust/DECISIONS.md ADR-003): field names are the
//! TS ones (camelCase), optional fields are omitted when absent, timestamps are integer
//! milliseconds, and fields are declared in the order `JSON.stringify` wrote them.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A JSON object, keys in insertion order (serde_json `preserve_order`).
pub type JsonObject = Map<String, Value>;

/// Milliseconds since the Unix epoch, like `Date.now()`.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `phase` of a Responses output message: absent, `null`, or a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Commentary,
    FinalAnswer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextContent {
    pub text: String,
    /// Responses phase is per output message, not per response. `Some(None)` is an explicit `null`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::json::double_option"
    )]
    pub phase: Option<Option<Phase>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    pub media_type: String,
    /// Base64. Shared, so cloning a history with images stays cheap.
    pub data: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingContent {
    pub thinking: String,
    /// Opaque provider state needed to replay this block (Anthropic signature, Responses reasoning item).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted: Option<bool>,
    /// Chat-compatible servers use different plaintext reasoning field names: `reasoning` or `reasoning_content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_field: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub input: JsonObject,
    /// Set when the streamed arguments could not be parsed as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<String>,
}

impl ToolCall {
    /// The call as the TS wrote it on its own (events, logs): the content block, `type` first.
    pub fn tagged(&self) -> serde_json::Value {
        serde_json::to_value(AssistantContent::ToolCall(self.clone())).unwrap_or(serde_json::Value::Null)
    }
}

/// `#[serde(serialize_with = "tagged_call")]`: a [`ToolCall`] field written with its `type`.
pub fn tagged_call<S: serde::Serializer>(c: &ToolCall, s: S) -> Result<S::Ok, S::Error> {
    use serde::Serialize;
    AssistantContent::ToolCall(c.clone()).serialize(s)
}

/// Content of user and tool-result messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserContent {
    Text(TextContent),
    Image(ImageContent),
}

/// Content of assistant messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantContent {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(ToolCall),
}

impl UserContent {
    pub fn text(t: impl Into<String>) -> Self {
        UserContent::Text(TextContent {
            text: t.into(),
            phase: None,
        })
    }
    pub fn as_text(&self) -> Option<&str> {
        match self {
            UserContent::Text(t) => Some(&t.text),
            _ => None,
        }
    }
}

impl AssistantContent {
    pub fn text(t: impl Into<String>) -> Self {
        AssistantContent::Text(TextContent {
            text: t.into(),
            phase: None,
        })
    }
    pub fn as_text(&self) -> Option<&str> {
        match self {
            AssistantContent::Text(t) => Some(&t.text),
            _ => None,
        }
    }
    pub fn as_tool_call(&self) -> Option<&ToolCall> {
        match self {
            AssistantContent::ToolCall(c) => Some(c),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    pub content: Vec<UserContent>,
    pub timestamp: i64,
}

/// `stopped`: a plugin ended generation early (e.g. repetition).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    #[default]
    Stop,
    ToolUse,
    MaxTokens,
    Refusal,
    Aborted,
    Stopped,
    Error,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::Stop => "stop",
            StopReason::ToolUse => "tool_use",
            StopReason::MaxTokens => "max_tokens",
            StopReason::Refusal => "refusal",
            StopReason::Aborted => "aborted",
            StopReason::Stopped => "stopped",
            StopReason::Error => "error",
        }
    }
}

/// errorMessage of a reply whose stream ended without the provider saying it was done (no finish
/// reason, no usage): the connection dropped mid-reply. What arrived is kept, with stopReason "error".
pub const INCOMPLETE_STREAM: &str = "the stream ended before the reply was complete";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// USD: what the provider reported, else the price-table estimate, 0 when there is neither.
    #[serde(with = "crate::json::js_f64")]
    pub cost: f64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.cost += o.cost;
    }
    /// input + cacheRead + cacheWrite + output: what the reply's request and answer took of the window.
    pub fn total(&self) -> u64 {
        self.input + self.cache_read + self.cache_write + self.output
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub content: Vec<AssistantContent>,
    pub api: String,
    pub provider: String,
    pub model: String,
    pub usage: Usage,
    pub stop_reason: StopReason,
    pub timestamp: i64,
    /// Digest of API/provider/model/endpoint, never its URL or credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

impl AssistantMessage {
    /// An empty reply from `model`, stopReason "stop", timestamp now (`newAssistant`).
    pub fn new(model: &ModelInfo) -> Self {
        AssistantMessage {
            content: Vec::new(),
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            timestamp: now_ms(),
            replay_scope: None,
            error_message: None,
        }
    }

    /// Marks it as cut off unless the provider said it was done (`ended`) or the user aborted (`markIfCut`).
    pub fn mark_if_cut(&mut self, ended: bool) {
        if !ended && self.stop_reason != StopReason::Aborted {
            self.stop_reason = StopReason::Error;
            self.error_message = Some(INCOMPLETE_STREAM.to_string());
        }
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.content.iter().filter_map(AssistantContent::as_tool_call)
    }

    pub fn text(&self) -> String {
        self.content.iter().filter_map(AssistantContent::as_text).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: Vec<UserContent>,
    pub is_error: bool,
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    Tool(ToolResultMessage),
}

impl Message {
    pub fn user(content: Vec<UserContent>) -> Self {
        Message::User(UserMessage {
            content,
            timestamp: now_ms(),
        })
    }
    pub fn role(&self) -> &'static str {
        match self {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::Tool(_) => "tool",
        }
    }
    pub fn as_assistant(&self) -> Option<&AssistantMessage> {
        match self {
            Message::Assistant(m) => Some(m),
            _ => None,
        }
    }
    pub fn timestamp(&self) -> i64 {
        match self {
            Message::User(m) => m.timestamp,
            Message::Assistant(m) => m.timestamp,
            Message::Tool(m) => m.timestamp,
        }
    }
    /// Text parts joined (`textOf(m.content)`).
    pub fn text(&self) -> String {
        match self {
            Message::User(m) => text_of_user(&m.content),
            Message::Assistant(m) => m.text(),
            Message::Tool(m) => text_of_user(&m.content),
        }
    }
    pub fn has_image(&self) -> bool {
        match self {
            Message::User(m) => m.content.iter().any(|c| matches!(c, UserContent::Image(_))),
            Message::Tool(m) => m.content.iter().any(|c| matches!(c, UserContent::Image(_))),
            Message::Assistant(_) => false,
        }
    }
}

/// The text parts of user or tool content, joined (`textOf`).
pub fn text_of_user(content: &[UserContent]) -> String {
    content.iter().filter_map(UserContent::as_text).collect()
}

/// A tool as the model sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    Off,
    Low,
    Medium,
    #[default]
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    pub const ALL: [ThinkingLevel; 6] = [
        ThinkingLevel::Off,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::Xhigh,
        ThinkingLevel::Max,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            ThinkingLevel::Off => "off",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::Xhigh => "xhigh",
            ThinkingLevel::Max => "max",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.as_str() == s)
    }
}

/// USD per 1M tokens.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Price {
    pub input: f64,
    pub output: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

/// The local server behind an OpenAI-style endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LocalServer {
    Ollama,
    Llamacpp,
    Lmstudio,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    pub api: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub context_window: u64,
    pub max_output: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<LocalServer>,
}

impl ModelInfo {
    /// `<provider>/<id>`.
    pub fn spec(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

/// Sampling knobs. Each adapter sends what its API understands; `extra_body` is merged into the request as-is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SamplingOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    /// Provider-specific fields, e.g. vLLM's `repetition_penalty`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<JsonObject>,
}

impl SamplingOptions {
    /// `{ ...self, ...other }`: fields set in `other` win.
    pub fn merged(&self, other: &SamplingOptions) -> SamplingOptions {
        SamplingOptions {
            temperature: other.temperature.or(self.temperature),
            top_p: other.top_p.or(self.top_p),
            frequency_penalty: other.frequency_penalty.or(self.frequency_penalty),
            presence_penalty: other.presence_penalty.or(self.presence_penalty),
            extra_body: other.extra_body.clone().or_else(|| self.extra_body.clone()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    pub api: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Environment variable holding the API key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// Shown in the generated ~/.sasacode/.env.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Model used when nothing is configured and this provider's key is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llamacpp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lmstudio: Option<bool>,
}

pub fn empty_usage() -> Usage {
    Usage::default()
}

/// Price-table estimate in USD (`computeCost`).
pub fn compute_cost(model: &ModelInfo, u: &Usage) -> f64 {
    let Some(p) = &model.price else { return 0.0 };
    (u.input as f64 * p.input
        + u.output as f64 * p.output
        + u.cache_read as f64 * p.cache_read.unwrap_or(p.input * 0.1)
        + u.cache_write as f64 * p.cache_write.unwrap_or(p.input * 1.25))
        / 1e6
}

/// Cost the provider reported alongside its usage (OpenRouter's `usage.cost`). Zero, negative or
/// non-numeric means "not reported" (`reportedCost`).
pub fn reported_cost(usage: Option<&Value>) -> Option<f64> {
    let c = usage?.get("cost")?.as_f64()?;
    (c.is_finite() && c > 0.0).then_some(c)
}

/// Parse streamed tool-call JSON. Returns the raw text when it is not a JSON object (`parseToolInput`).
pub fn parse_tool_input(json: &str) -> (JsonObject, Option<String>) {
    if json.trim().is_empty() {
        return (JsonObject::new(), None);
    }
    match serde_json::from_str::<Value>(json) {
        Ok(Value::Object(o)) => (o, None),
        _ => (JsonObject::new(), Some(json.to_string())),
    }
}

/// Is `url` on exactly this origin? Fields only one vendor's own endpoint takes are sent nowhere else.
pub fn has_origin(url: Option<&str>, origin: &str) -> bool {
    url.and_then(|u| url::Url::parse(u).ok())
        .map(|u| u.origin().ascii_serialization() == origin)
        .unwrap_or(false)
}

/// Common sampling fields in snake_case, plus extraBody. Unset fields are omitted (`samplingFields`).
pub fn sampling_fields(s: Option<&SamplingOptions>, keys: &[&str]) -> JsonObject {
    let mut out = JsonObject::new();
    let Some(s) = s else { return out };
    for k in keys {
        let v = match *k {
            "temperature" => s.temperature,
            "top_p" => s.top_p,
            "frequency_penalty" => s.frequency_penalty,
            "presence_penalty" => s.presence_penalty,
            _ => None,
        };
        if let Some(v) = v {
            out.insert((*k).to_string(), crate::json::js_number(v));
        }
    }
    if let Some(extra) = &s.extra_body {
        for (k, v) in extra {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}
