//! The conversation as the window shows it, built from a run's events or from a session log.
//! Kept free of drawing so it can be tested without a window.

use std::collections::HashMap;

use iced::widget::markdown;
use sasacode_protocol::{Content, Event, Message, StopCause, StreamDelta, ToolCall, Usage, text_of};

use crate::i18n::Strings;

/// Tool output longer than this is cut, from the front: the end is what tells how it went.
const MAX_TOOL_OUTPUT: usize = 8_000;

#[derive(Debug)]
pub enum Item {
    User(String),
    Assistant(Markdown),
    Thinking(String),
    Tool(Tool),
    Approval(Approval),
    Notice { text: String, error: bool },
}

/// Markdown that can grow while it streams in.
#[derive(Debug, Default)]
pub struct Markdown {
    pub raw: String,
    pub content: markdown::Content,
}

impl Markdown {
    pub fn new(text: &str) -> Self {
        Self {
            raw: text.to_owned(),
            content: markdown::Content::parse(text),
        }
    }

    pub fn push(&mut self, delta: &str) {
        self.raw.push_str(delta);
        self.content.push_str(delta);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub output: String,
    /// `None` while it runs.
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Approval {
    pub id: String,
    pub tool: String,
    pub detail: String,
    pub reason: String,
    /// How it was answered, once it was.
    pub answer: Option<String>,
}

#[derive(Debug, Default)]
pub struct Transcript {
    pub items: Vec<Item>,
    pub usage: Usage,
    /// Content index of the streaming reply → its item.
    streaming: HashMap<usize, usize>,
    /// Prompts shown when they were sent, so the run's echo of them is not shown twice.
    echoes: Vec<String>,
}

/// What an event changed beyond the items.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Effect {
    pub session_id: Option<String>,
    pub ended: Option<StopCause>,
    pub approval: Option<String>,
}

impl Transcript {
    /// Shows a prompt as soon as it is sent.
    pub fn send(&mut self, text: &str) {
        self.items.push(Item::User(text.to_owned()));
        self.echoes.push(text.to_owned());
    }

    pub fn notice(&mut self, text: impl Into<String>, error: bool) {
        self.items.push(Item::Notice {
            text: text.into(),
            error,
        });
    }

    pub fn pending_approval(&self) -> Option<&Approval> {
        self.items.iter().rev().find_map(|i| match i {
            Item::Approval(a) if a.answer.is_none() => Some(a),
            _ => None,
        })
    }

    pub fn answer_approval(&mut self, id: &str, answer: String) {
        for item in self.items.iter_mut().rev() {
            if let Item::Approval(a) = item
                && a.id == id
            {
                a.answer = Some(answer);
                return;
            }
        }
    }

    pub fn apply(&mut self, event: &Event, s: &Strings) -> Effect {
        let mut effect = Effect::default();
        match event {
            Event::Session { id, .. } => effect.session_id = id.clone(),
            Event::MessageStart => self.streaming.clear(),
            Event::MessageUpdate { event } => self.stream(event),
            Event::MessageEnd { message } => self.message_end(message),
            Event::ToolStart { call, summary, .. } => self.items.push(Item::Tool(Tool {
                id: call.id.clone(),
                name: call.name.clone(),
                summary: if summary.is_empty() {
                    summarize(call)
                } else {
                    summary.clone()
                },
                output: String::new(),
                is_error: None,
            })),
            Event::ToolUpdate { call, text, .. } => {
                if let Some(tool) = self.tool_mut(&call.id) {
                    tool.output.push_str(text);
                    tool.output = tail(&tool.output);
                }
            }
            Event::ToolEnd { call, result, .. } => {
                let output = tail(&text_of(&result.content));
                match self.tool_mut(&call.id) {
                    Some(tool) => {
                        tool.output = output;
                        tool.is_error = Some(result.is_error);
                    }
                    None => self.items.push(Item::Tool(Tool {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        summary: summarize(call),
                        output,
                        is_error: Some(result.is_error),
                    })),
                }
            }
            Event::Usage { usage, .. } => self.usage.add(usage),
            Event::ContextLimit { .. } => self.notice(s.context_full, true),
            Event::Error { error } => self.notice(error.clone(), true),
            Event::PluginError { plugin, hook, error } => self.notice(format!("{plugin} ({hook}): {error}"), true),
            Event::MessagesReplaced => self.notice(s.compacted, false),
            Event::MessageDiscarded { .. } => {
                // The reply that streamed in is asked again: drop what was shown of it.
                let shown: Vec<usize> = self.streaming.drain().map(|(_, i)| i).collect();
                let mut i = 0;
                self.items.retain(|_| {
                    i += 1;
                    !shown.contains(&(i - 1))
                });
                self.notice(s.retrying, false);
            }
            Event::ToolRepaired { call, note, .. } => self.notice(format!("↻ {}: {note}", call.name), false),
            Event::AgentEnd { cause, stopped, error } => {
                effect.ended = Some(*cause);
                if let Some(error) = error {
                    self.notice(error.clone(), true);
                }
                if let Some(by) = stopped {
                    self.notice(format!("{}: {}", by.plugin, by.reason), false);
                } else if *cause != StopCause::Done {
                    self.notice(s.stopped(*cause), *cause == StopCause::Error);
                }
            }
            Event::ApprovalRequest {
                id, tool, args, reason, ..
            } => {
                self.items.push(Item::Approval(Approval {
                    id: id.clone(),
                    tool: tool.clone(),
                    detail: describe(args),
                    reason: reason.clone(),
                    answer: None,
                }));
                effect.approval = Some(id.clone());
            }
            Event::ApprovalCancelled { id } => self.answer_approval(id, s.cancelled.into()),
            Event::ControlError { error } => self.notice(format!("control: {error}"), true),
            Event::AgentStart | Event::TurnStart { .. } | Event::TurnEnd { .. } | Event::Unknown(_) => {}
        }
        effect
    }

    fn stream(&mut self, delta: &StreamDelta) {
        match delta {
            StreamDelta::TextDelta { index, delta } => match self.streaming.get(index) {
                Some(&i) => {
                    if let Some(Item::Assistant(md)) = self.items.get_mut(i) {
                        md.push(delta);
                    }
                }
                None => {
                    self.streaming.insert(*index, self.items.len());
                    self.items.push(Item::Assistant(Markdown::new(delta)));
                }
            },
            StreamDelta::ThinkingDelta { index, delta } => match self.streaming.get(index) {
                Some(&i) => {
                    if let Some(Item::Thinking(t)) = self.items.get_mut(i) {
                        t.push_str(delta);
                    }
                }
                None => {
                    self.streaming.insert(*index, self.items.len());
                    self.items.push(Item::Thinking(delta.clone()));
                }
            },
            _ => {}
        }
    }

    fn message_end(&mut self, message: &Message) {
        match message {
            Message::User { content, .. } => {
                let text = text_of(content);
                if let Some(pos) = self.echoes.iter().position(|e| *e == text) {
                    self.echoes.remove(pos);
                } else {
                    self.items.push(Item::User(text));
                }
            }
            Message::Assistant(a) => {
                self.usage.add(&a.usage);
                // A reply that did not stream (or streamed only in part) is shown from the whole message.
                for (index, c) in a.content.iter().enumerate() {
                    if self.streaming.contains_key(&index) {
                        continue;
                    }
                    match c {
                        Content::Text { text } if !text.trim().is_empty() => {
                            self.items.push(Item::Assistant(Markdown::new(text)))
                        }
                        Content::Thinking { thinking, .. } if !thinking.trim().is_empty() => {
                            self.items.push(Item::Thinking(thinking.clone()))
                        }
                        _ => {}
                    }
                }
                self.streaming.clear();
                if let Some(e) = &a.error_message {
                    self.notice(e.clone(), true);
                }
            }
            Message::Tool(_) | Message::Other => {}
        }
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut Tool> {
        self.items.iter_mut().rev().find_map(|i| match i {
            Item::Tool(t) if t.id == id => Some(t),
            _ => None,
        })
    }

    /// A conversation read back from its session log.
    pub fn from_messages(messages: &[Message]) -> Self {
        let mut t = Transcript::default();
        for m in messages {
            match m {
                Message::User { content, .. } => t.items.push(Item::User(text_of(content))),
                Message::Assistant(a) => {
                    t.usage.add(&a.usage);
                    for c in &a.content {
                        match c {
                            Content::Text { text } if !text.trim().is_empty() => {
                                t.items.push(Item::Assistant(Markdown::new(text)))
                            }
                            Content::Thinking { thinking, .. } if !thinking.trim().is_empty() => {
                                t.items.push(Item::Thinking(thinking.clone()))
                            }
                            Content::ToolCall(call) => t.items.push(Item::Tool(Tool {
                                id: call.id.clone(),
                                name: call.name.clone(),
                                summary: summarize(call),
                                output: String::new(),
                                is_error: None,
                            })),
                            _ => {}
                        }
                    }
                }
                Message::Tool(r) => {
                    if let Some(tool) = t.tool_mut(&r.tool_call_id) {
                        tool.output = tail(&text_of(&r.content));
                        tool.is_error = Some(r.is_error);
                    }
                }
                Message::Other => {}
            }
        }
        t
    }
}

/// A one-line description of a call: the argument that says the most, else its JSON.
pub fn summarize(call: &ToolCall) -> String {
    for key in ["command", "path", "file_path", "pattern", "url", "query"] {
        if let Some(v) = call.input.get(key).and_then(|v| v.as_str()) {
            return first_line(v);
        }
    }
    first_line(&call.input.to_string())
}

fn describe(args: &serde_json::Value) -> String {
    match args.get("command").and_then(|v| v.as_str()) {
        Some(command) => command.to_owned(),
        None => serde_json::to_string_pretty(args).unwrap_or_default(),
    }
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("");
    let short: String = line.chars().take(120).collect();
    if short.len() < s.len() {
        format!("{short}…")
    } else {
        short
    }
}

fn tail(s: &str) -> String {
    if s.len() <= MAX_TOOL_OUTPUT {
        return s.to_owned();
    }
    let mut start = s.len() - MAX_TOOL_OUTPUT;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &s[start..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use sasacode_protocol::parse_event;

    fn fixture(name: &str) -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../sasacode-protocol/tests/fixtures")
            .join(name);
        std::fs::read_to_string(p).unwrap()
    }

    fn kinds(t: &Transcript) -> Vec<String> {
        t.items
            .iter()
            .map(|i| match i {
                Item::User(s) => format!("user:{s}"),
                Item::Assistant(m) => format!("assistant:{}", m.raw),
                Item::Thinking(s) => format!("thinking:{s}"),
                Item::Tool(t) => format!("tool:{}:{}:{:?}", t.name, t.summary, t.is_error),
                Item::Approval(a) => format!("approval:{}:{}:{:?}", a.tool, a.detail, a.answer),
                Item::Notice { text, error } => format!("notice:{error}:{text}"),
            })
            .collect()
    }

    #[test]
    fn a_real_run_becomes_the_conversation_once_with_the_sent_prompt_not_repeated() {
        let s = Strings::get("ja");
        let mut t = Transcript::default();
        t.send("ファイルを作って");
        let mut effects = Vec::new();
        for line in fixture("run.jsonl").lines() {
            effects.push(t.apply(&parse_event(line).unwrap(), &s));
        }
        let control_error = "notice:true:control: a control message is a JSON object";
        assert_eq!(
            kinds(&t),
            [
                "user:ファイルを作って",
                "thinking:まずファイルを作る。",
                "assistant:ファイルを作ります。",
                "approval:bash:echo hi > made.txt:None",
                control_error,
                "tool:bash:echo hi > made.txt:Some(false)",
                "assistant:**完了**しました。`made.txt` を作成しています。",
            ]
        );
        assert!(effects[0].session_id.is_some());
        assert!(effects.iter().any(|e| e.approval.as_deref() == Some("approval-1")));
        assert_eq!(effects.last().unwrap().ended, Some(StopCause::Done));
    }

    #[test]
    fn a_session_log_reads_back_as_the_same_conversation() {
        let entries: Vec<sasacode_protocol::SessionEntry> = fixture("session.jsonl")
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let t = Transcript::from_messages(&sasacode_protocol::restore(&entries).messages);
        assert_eq!(
            kinds(&t),
            [
                "user:ファイルを作って",
                "thinking:まずファイルを作る。",
                "assistant:ファイルを作ります。",
                "tool:bash:echo hi > made.txt:Some(false)",
                "assistant:**完了**しました。`made.txt` を作成しています。",
            ]
        );
    }

    #[test]
    fn long_tool_output_keeps_its_end() {
        let long = "あ".repeat(MAX_TOOL_OUTPUT);
        let cut = tail(&long);
        assert!(cut.starts_with('…') && cut.len() <= MAX_TOOL_OUTPUT + '…'.len_utf8());
    }
}
