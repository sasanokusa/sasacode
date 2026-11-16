//! What a parent writes to the stdin of `sasacode -p … --output jsonl --control stdio`, one JSON
//! object per line (`packages/cli/src/control.ts`).

use serde::{Deserialize, Serialize};

pub const CONTROL_TYPES: &[&str] = &["approval", "message", "abort"];

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
    },
    /// Like typing while the TUI is busy: the model sees it at its next turn. Must not be blank.
    Message { text: String },
    /// Like Ctrl+C: the run ends with `agent_end` cause `aborted`.
    Abort,
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
