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
    "ui_request",
    "ui_cancelled",
    "plugin_notify",
    "plugin_text",
    "plugin_status",
    "commands",
    "judged",
];

/// One line of the output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The first line: the session this run is saved to (`null` with `--no-session`).
    Session {
        id: Option<String>,
        path: Option<String>,
        /// How many tokens the model takes in, as the CLI knows it at the start of the run. A CLI
        /// before 0.8 does not say.
        #[serde(default, rename = "contextWindow")]
        context_window: Option<u64>,
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
        /// The allow rules an `always` answer adds; each can be given to a later run with
        /// `--allow`. A CLI before 0.8 does not say.
        #[serde(default)]
        rules: Vec<String>,
    },
    /// The run was interrupted before this request was answered.
    ApprovalCancelled {
        id: String,
    },
    /// A control line could not be used.
    ControlError {
        error: String,
    },
    /// `--control stdio`: a plugin asks the user something, and waits for
    /// [`crate::ControlMessage::UiResponse`] with this `id` until it is answered or dismissed.
    UiRequest {
        id: String,
        #[serde(flatten)]
        ask: UiAsk,
        #[serde(default)]
        title: String,
    },
    /// The run was interrupted before this question was answered.
    UiCancelled {
        id: String,
    },
    /// A plugin's notice.
    PluginNotify {
        #[serde(default)]
        level: NoticeLevel,
        text: String,
    },
    /// A text a plugin shows whole (a report, a list).
    PluginText {
        #[serde(default)]
        title: String,
        text: String,
        /// `markdown` or `text`.
        #[serde(default)]
        format: String,
    },
    /// A plugin's status item, keyed `plugin:key`; `None` when it went away.
    PluginStatus {
        key: String,
        #[serde(default)]
        text: Option<String>,
    },
    /// The slash commands the plugins offer now: at the start, then whenever they change.
    Commands {
        #[serde(default)]
        commands: Vec<Command>,
    },
    /// The agent permission mode's judge on one call: allowed, or handed to the user.
    Judged {
        #[serde(rename = "callId")]
        call_id: String,
        tool: String,
        verdict: String,
        #[serde(default)]
        reason: String,
    },
    /// A line whose `type` this crate does not know. Kept whole so a caller can still show it.
    #[serde(skip)]
    Unknown(Value),
}

/// What a plugin's question is, and what it offers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiAsk {
    /// Answered with `true` or `false`.
    Confirm {
        #[serde(default)]
        message: Option<String>,
    },
    /// Answered with one option's `value`.
    Select { options: Vec<UiOption> },
    /// Answered with a string.
    Input {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        placeholder: Option<String>,
        #[serde(default)]
        initial: Option<String>,
    },
    /// Answered with the `value`s chosen.
    SelectMany {
        options: Vec<UiOption>,
        #[serde(default)]
        selected: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiOption {
    pub value: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    #[default]
    Info,
    Warning,
    Error,
}

/// A slash command a plugin offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub argument_hint: Option<String>,
    #[serde(default)]
    pub owner: String,
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
