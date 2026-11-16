//! What `sasacode -p … --output jsonl` writes to stdout, one JSON object per line: the agent's
//! events (`packages/agent/src/events.ts`, as `packages/cli/src/headless.ts` serializes them), the
//! `session` line that starts every run, and with `--control stdio` the control lines
//! (`packages/cli/src/control.ts`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::message::{AssistantMessage, Message, ToolCall, Usage};

/// Every `type` an output line can have. A test checks this list against the TypeScript sources,
/// so a new event there fails the Rust build's tests instead of being dropped silently.
pub const EVENT_TYPES: &[&str] = &[
    "session",
    "agent_start",
    "turn_start",
    "message_start",
    "message_update",
    "message_end",
    "tool_start",
    "tool_update",
    "tool_end",
    "turn_end",
    "usage",
    "context_limit",
    "error",
    "plugin_error",
    "messages_replaced",
    "message_discarded",
    "tool_repaired",
    "agent_end",
    "approval_request",
    "approval_cancelled",
    "control_error",
];

/// One line of the output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The first line: the session this run is saved to (`null` with `--no-session`).
    Session {
        id: Option<String>,
        path: Option<String>,
    },
    AgentStart,
    TurnStart {
        turn: u32,
    },
    /// An assistant message starts streaming. The partial message itself is not written.
    MessageStart,
    MessageUpdate {
        event: StreamDelta,
    },
    /// A whole message: the user's prompt, an assistant reply or a tool result.
    MessageEnd {
        message: Message,
    },
    ToolStart {
        call: ToolCall,
        #[serde(default)]
        summary: String,
        #[serde(default, rename = "parentCallId")]
        parent_call_id: Option<String>,
    },
    ToolUpdate {
        call: ToolCall,
        #[serde(default)]
        text: String,
        #[serde(default, rename = "parentCallId")]
        parent_call_id: Option<String>,
    },
    ToolEnd {
        call: ToolCall,
        result: ToolResult,
        #[serde(default, rename = "parentCallId")]
        parent_call_id: Option<String>,
    },
    TurnEnd {
        turn: u32,
    },
    /// A response outside the conversation (a subagent, a side request, a discarded reply).
    Usage {
        #[serde(default)]
        t: f64,
        #[serde(default)]
        model: String,
        #[serde(default)]
        usage: Usage,
        #[serde(default)]
        source: String,
    },
    ContextLimit {
        tokens: u64,
        #[serde(rename = "contextWindow")]
        context_window: u64,
    },
    Error {
        error: String,
    },
    PluginError {
        plugin: String,
        hook: String,
        error: String,
    },
    /// The conversation was rewritten (compaction): what was shown before no longer matches it.
    MessagesReplaced,
    /// A reply that was thrown away (a dropped connection that is asked again, for one).
    MessageDiscarded {
        message: AssistantMessage,
    },
    ToolRepaired {
        call: ToolCall,
        from: RepairedFrom,
        note: String,
    },
    AgentEnd {
        cause: StopCause,
        #[serde(default)]
        stopped: Option<StoppedBy>,
        #[serde(default)]
        error: Option<String>,
    },
    /// `--control stdio`: a call waits for [`crate::ControlMessage::Approval`] with this `id`.
    ApprovalRequest {
        id: String,
        #[serde(rename = "callId")]
        call_id: String,
        tool: String,
        #[serde(default)]
        args: Value,
        #[serde(default)]
        reason: String,
        #[serde(default)]
        cwd: String,
    },
    /// The run was interrupted before this request was answered.
    ApprovalCancelled {
        id: String,
    },
    /// A control line could not be used.
    ControlError {
        error: String,
    },
    /// A line whose `type` this crate does not know. Kept whole so a caller can still show it.
    #[serde(skip)]
    Unknown(Value),
}

/// The streaming step inside `message_update`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamDelta {
    Start,
    TextDelta {
        index: usize,
        delta: String,
    },
    ThinkingDelta {
        index: usize,
        delta: String,
    },
    ToolcallStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolcallDelta {
        index: usize,
        delta: String,
    },
    Done {
        message: AssistantMessage,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolResult {
    pub content: Vec<crate::message::Content>,
    pub is_error: bool,
    /// Structured data for renderers (diffs, exit codes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairedFrom {
    pub name: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub raw_input: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoppedBy {
    pub plugin: String,
    pub reason: String,
}

/// Why a run ended (`StopCause` in `packages/plugin-api`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCause {
    Done,
    Aborted,
    Error,
    Refusal,
    ContextLimit,
    MaxTurns,
    Stopped,
    #[serde(other)]
    Other,
}

/// Parses one output line. A line whose `type` is not in [`EVENT_TYPES`] becomes
/// [`Event::Unknown`]; a known type with the wrong shape is an error.
pub fn parse_event(line: &str) -> Result<Event, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let known = value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| EVENT_TYPES.contains(&t));
    if known {
        serde_json::from_value(value)
    } else {
        Ok(Event::Unknown(value))
    }
}
