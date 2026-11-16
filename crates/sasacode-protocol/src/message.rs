//! The provider-neutral conversation format (`packages/ai/src/types.ts`). Sessions store these and
//! `message_end` events carry them.
//!
//! Every type here is lenient on purpose: a field the TypeScript side adds later is ignored, a
//! missing optional field takes its default, and an unknown content or message kind becomes
//! `Other` instead of failing the whole line.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One piece of a message's content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text {
        text: String,
    },
    Image {
        #[serde(rename = "mediaType")]
        media_type: String,
        /// Base64.
        data: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        redacted: bool,
    },
    ToolCall(ToolCall),
    /// A content kind this crate does not know yet.
    #[serde(other)]
    Other,
}

/// A call the model asked for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub input: Value,
    /// Set when the streamed arguments could not be parsed as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<String>,
}

/// What one model response used. `cost` is in USD.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: f64,
}

impl Usage {
    pub fn add(&mut self, other: &Usage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.cost += other.cost;
    }
}

/// One message of the conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User {
        #[serde(default)]
        content: Vec<Content>,
        #[serde(default)]
        timestamp: f64,
    },
    Assistant(AssistantMessage),
    Tool(ToolResultMessage),
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AssistantMessage {
    pub content: Vec<Content>,
    pub api: String,
    pub provider: String,
    pub model: String,
    pub usage: Usage,
    /// `stop`, `tool_use`, `max_tokens`, `refusal`, `aborted`, `stopped` or `error`.
    pub stop_reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub timestamp: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: Vec<Content>,
    pub is_error: bool,
    pub timestamp: f64,
}

/// The text parts of some content, joined. Images, thinking and tool calls are left out.
pub fn text_of(content: &[Content]) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

impl Message {
    pub fn content(&self) -> &[Content] {
        match self {
            Message::User { content, .. } => content,
            Message::Assistant(m) => &m.content,
            Message::Tool(m) => &m.content,
            Message::Other => &[],
        }
    }
}
