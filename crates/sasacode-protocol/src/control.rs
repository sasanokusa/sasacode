//! What a parent writes to the stdin of `sasacode -p … --output jsonl --control stdio`, one JSON
//! object per line (`packages/cli/src/control.ts`).

use serde::{Deserialize, Serialize};

pub const CONTROL_TYPES: &[&str] = &["approval", "message", "abort", "ui_response"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// Answers an [`crate::Event::ApprovalRequest`] by its `id`.
    Approval {
        id: String,
        decision: Decision,
        /// Reaches the model with a `deny`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
        /// The call as the user edited it, with `allow` or `always`: it runs with these, checked
        /// again as the model's are.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<serde_json::Value>,
    },
    /// Like typing while the TUI is busy: the model sees it at its next turn. Must not be blank.
    Message { text: String },
    /// Like Ctrl+C: the run ends with `agent_end` cause `aborted`. With `call_id`, that tool call
    /// alone ends, as an error, and the run goes on.
    Abort {
        #[serde(default, rename = "callId", skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
    },
    /// Answers an [`crate::Event::UiRequest`] by its `id`: `true`/`false`, a string, the values
    /// chosen, or `null` to dismiss it.
    UiResponse { id: String, value: serde_json::Value },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// This call only.
    Allow,
    /// This call, and a rule that allows the same call until the process ends.
    Always,
    Deny,
}

impl ControlMessage {
    /// The message as one line, newline included, ready to write to the child's stdin.
    pub fn to_line(&self) -> String {
        let mut line = serde_json::to_string(self).expect("a control message always serializes");
        line.push('\n');
        line
    }
}
