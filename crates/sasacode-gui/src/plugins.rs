//! What the plugins of a run ask and say through `--control stdio`: their questions, answered in
//! the dock; a tool call stopped on its own; a call's arguments edited before it is allowed.

use iced::Task;
use iced::widget::{operation, text_editor};
use sasacode_protocol::event::UiAsk;
use sasacode_protocol::{ControlMessage, Decision};
use serde_json::Value;

use crate::app::{App, Msg, Overlay};
use crate::i18n::fill;

/// The text of a call's arguments being edited.
pub const ARGS: &str = "args";

impl App {
    /// Answers the shown conversation's question `id`; `Value::Null` dismisses it.
    fn reply_to(&mut self, id: &str, value: Value) -> Task<Msg> {
        let asks = &mut self.chat.transcript.asks;
        if !asks.iter().any(|a| a.id == id) {
            return Task::none();
        }
        asks.retain(|a| a.id != id);
        self.control(ControlMessage::UiResponse {
            id: id.to_owned(),
            value,
        });
        Task::none()
    }

    /// What the question `id` is answered with as it stands: the text typed, or what is ticked.
    fn answer_of(&self, id: &str) -> Option<Value> {
        let ask = self.chat.transcript.asks.iter().find(|a| a.id == id)?;
        Some(match &ask.ask {
            UiAsk::Input { .. } => Value::String(ask.typed.clone()),
            UiAsk::SelectMany { options, .. } => Value::Array(
                // In the order offered, not the order ticked.
                options
                    .iter()
                    .filter(|o| ask.chosen.contains(&o.value))
                    .map(|o| Value::String(o.value.clone()))
                    .collect(),
            ),
            UiAsk::Confirm { .. } | UiAsk::Select { .. } => return None,
        })
    }

    /// The plugins' messages; any other comes back.
    pub(crate) fn update_plugins(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let task = match msg {
            Msg::AskAnswer(id, value) => self.reply_to(&id, value),
            Msg::AskToggle(id, value) => {
                if let Some(ask) = self.chat.transcript.asks.iter_mut().find(|a| a.id == id)
                    && !ask.chosen.remove(&value)
                {
                    ask.chosen.insert(value);
                }
                Task::none()
            }
            Msg::AskType(id, typed) => {
                if let Some(ask) = self.chat.transcript.asks.iter_mut().find(|a| a.id == id) {
                    ask.typed = typed;
                }
                Task::none()
            }
            Msg::AskSubmit(id) => match self.answer_of(&id) {
                Some(value) => self.reply_to(&id, value),
                None => Task::none(),
            },
            Msg::StopCall(call) => {
                if self.running() {
                    self.control(ControlMessage::Abort { call_id: Some(call) });
                }
                Task::none()
            }
            Msg::ArgsOpen => {
                let Some(approval) = self.chat.transcript.pending_approval() else {
                    return Ok(Task::none());
                };
                let json = serde_json::to_string_pretty(&approval.args).unwrap_or_default();
                self.editing_args = Some((approval.id.clone(), text_editor::Content::with_text(&json)));
                self.overlay = Some(Overlay::Args);
                operation::focus(ARGS)
            }
            Msg::ArgsEdit(action) => {
                if let Some((_, content)) = &mut self.editing_args {
                    content.perform(action);
                }
                Task::none()
            }
            Msg::ArgsSend => {
                let Some((id, content)) = &self.editing_args else {
                    return Ok(Task::none());
                };
                // Only for the approval it was opened for: it may have been answered meanwhile.
                if self.chat.transcript.pending_approval().map(|a| &a.id) != Some(id) {
                    self.editing_args = None;
                    self.overlay = None;
                    return Ok(Task::none());
                }
                match serde_json::from_str::<Value>(&content.text()) {
                    Ok(args @ Value::Object(_)) => {
                        self.editing_args = None;
                        self.overlay = None;
                        let task = self.answer_with(Decision::Allow, Some(args));
                        Task::batch([task, operation::focus(crate::app::INPUT)])
                    }
                    Ok(_) => {
                        self.toast = Some(fill(self.s.w().args_bad, "error", "{ … }"));
                        Task::none()
                    }
                    Err(e) => {
                        self.toast = Some(fill(self.s.w().args_bad, "error", e));
                        Task::none()
                    }
                }
            }
            Msg::ForgetAllowed(rule) => {
                let Some(id) = self.chat.session.clone() else {
                    return Ok(Task::none());
                };
                self.set(|p| {
                    if let Some(kept) = p.more.allowed.get_mut(&id) {
                        kept.retain(|r| *r != rule);
                        if kept.is_empty() {
                            p.more.allowed.remove(&id);
                        }
                    }
                })
            }
            other => return Err(other),
        };
        Ok(task)
    }
}
