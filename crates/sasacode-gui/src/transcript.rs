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
    Notice {
        text: String,
        error: bool,
    },
    /// A run ended here, after this many seconds.
    Done {
        seconds: u64,
    },
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
    /// The call's arguments: an edit or a write shows its change from them.
    pub input: serde_json::Value,
    pub output: String,
    /// `None` while it runs.
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Approval {
    pub id: String,
    /// The call it is about.
    pub call: String,
    pub tool: String,
    pub detail: String,
    /// The call's arguments: an edit or a write is shown as the change it would make.
    pub args: serde_json::Value,
    pub reason: String,
    /// How it was answered, once it was.
    pub answer: Option<Verdict>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    Always,
    Denied,
    /// The run withdrew the question.
    Cancelled,
}

/// How a turn stands, for its mark on the stalk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    Running,
    /// Waiting for an approval.
    Asking,
    Done,
    Failed,
    /// Ended without an answer: interrupted, or read back from a log that stops there.
    Cut,
}

/// How an open turn is laid out: `folded` rows sit behind one line, `shown` rows are on the sheet.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Layout {
    pub folded: Vec<usize>,
    pub shown: Vec<usize>,
}

/// What a conversation did to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange<'a> {
    pub path: &'a str,
    /// One per edit or write, in order: `-` lines leave, `+` lines come.
    pub hunks: Vec<Vec<(char, String)>>,
    pub added: usize,
    pub removed: usize,
}

/// While a turn has no answer yet, this many of its latest rows stay on the sheet.
const LIVE_ROWS: usize = 5;

/// One line of the agent's task list (`todo_write`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Todo {
    pub text: String,
    pub state: TodoState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoState {
    Pending,
    Active,
    Done,
}

/// What the agent is doing right now, for the line under the input.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Activity {
    #[default]
    Waiting,
    Thinking,
    Writing,
    Tool(String),
}

/// One request and what followed it: `items[start..end]`. Only the first turn of a conversation that
/// was read back mid-way can start with something other than the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Turn {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Default)]
pub struct Transcript {
    pub items: Vec<Item>,
    pub usage: Usage,
    /// The agent's task list as it last wrote it.
    pub todos: Vec<Todo>,
    pub activity: Activity,
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
    /// A message sent during the run reached the model and is now part of the conversation.
    pub delivered: Option<String>,
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

    /// How many approvals wait for an answer.
    pub fn pending_approvals(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i, Item::Approval(a) if a.answer.is_none()))
            .count()
    }

    pub fn answer_approval(&mut self, id: &str, answer: Verdict) {
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
            Event::MessageStart => {
                self.streaming.clear();
                self.activity = Activity::Waiting;
            }
            Event::MessageUpdate { event } => self.stream(event),
            Event::MessageEnd { message } => effect.delivered = self.message_end(message),
            Event::ToolStart { call, summary, .. } => {
                self.activity = Activity::Tool(call.name.clone());
                self.read_todos(call);
                self.items.push(Item::Tool(Tool {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    summary: if summary.is_empty() {
                        summarize(call)
                    } else {
                        summary.clone()
                    },
                    input: call.input.clone(),
                    output: String::new(),
                    is_error: None,
                }))
            }
            Event::ToolUpdate { call, text, .. } => {
                if let Some(tool) = self.tool_mut(&call.id) {
                    tool.output.push_str(text);
                    tool.output = tail(&tool.output);
                }
            }
            Event::ToolEnd { call, result, .. } => {
                self.activity = Activity::Waiting;
                let output = tail(&text_of(&result.content));
                let refused = self.refused(&call.id);
                match self.tool_mut(&call.id) {
                    Some(tool) => {
                        tool.output = output;
                        tool.is_error = Some(result.is_error);
                    }
                    // A call that was refused never started: the refusal is its row.
                    None if refused => {}
                    None => self.items.push(Item::Tool(Tool {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        summary: summarize(call),
                        input: call.input.clone(),
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
            Event::ToolRepaired { call, note, .. } => self.notice(s.repaired(&call.name, note), false),
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
                id,
                call_id,
                tool,
                args,
                reason,
                ..
            } => {
                self.items.push(Item::Approval(Approval {
                    id: id.clone(),
                    call: call_id.clone(),
                    tool: tool.clone(),
                    detail: describe(args),
                    args: args.clone(),
                    reason: reason.clone(),
                    answer: None,
                }));
                effect.approval = Some(id.clone());
            }
            Event::ApprovalCancelled { id } => self.answer_approval(id, Verdict::Cancelled),
            Event::ControlError { error } => self.notice(format!("control: {error}"), true),
            Event::AgentStart | Event::TurnStart { .. } | Event::TurnEnd { .. } | Event::Unknown(_) => {}
        }
        effect
    }

    fn stream(&mut self, delta: &StreamDelta) {
        match delta {
            StreamDelta::TextDelta { index, delta } => {
                self.activity = Activity::Writing;
                self.stream_text(*index, delta)
            }
            StreamDelta::ThinkingDelta { index, delta } => {
                self.activity = Activity::Thinking;
                self.stream_thinking(*index, delta)
            }
            _ => {}
        }
    }

    fn stream_text(&mut self, index: usize, delta: &str) {
        match self.streaming.get(&index) {
            Some(&i) => {
                if let Some(Item::Assistant(md)) = self.items.get_mut(i) {
                    md.push(delta);
                }
            }
            None => {
                self.streaming.insert(index, self.items.len());
                self.items.push(Item::Assistant(Markdown::new(delta)));
            }
        }
    }

    fn stream_thinking(&mut self, index: usize, delta: &str) {
        match self.streaming.get(&index) {
            Some(&i) => {
                if let Some(Item::Thinking(t)) = self.items.get_mut(i) {
                    t.push_str(delta);
                }
            }
            None => {
                self.streaming.insert(index, self.items.len());
                self.items.push(Item::Thinking(delta.to_owned()));
            }
        }
    }

    /// Returns the text of a user message that was not shown yet: one sent while the run was busy.
    fn message_end(&mut self, message: &Message) -> Option<String> {
        match message {
            Message::User { content, .. } => {
                let text = text_of(content);
                if let Some(pos) = self.echoes.iter().position(|e| *e == text) {
                    self.echoes.remove(pos);
                } else {
                    self.items.push(Item::User(text.clone()));
                    return Some(text);
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
        None
    }

    fn read_todos(&mut self, call: &ToolCall) {
        if call.name != "todo_write" {
            return;
        }
        let Some(list) = call.input.get("todos").and_then(|v| v.as_array()) else {
            return;
        };
        self.todos = list
            .iter()
            .filter_map(|t| {
                Some(Todo {
                    text: t.get("content")?.as_str()?.to_owned(),
                    state: match t.get("status").and_then(|s| s.as_str()) {
                        Some("completed") => TodoState::Done,
                        Some("in_progress") => TodoState::Active,
                        _ => TodoState::Pending,
                    },
                })
            })
            .collect();
    }

    /// The conversation cut at each request.
    pub fn turns(&self) -> Vec<Turn> {
        let mut out: Vec<Turn> = Vec::new();
        for (i, item) in self.items.iter().enumerate() {
            match out.last_mut() {
                Some(turn) if !matches!(item, Item::User(_)) => turn.end = i + 1,
                _ => out.push(Turn { start: i, end: i + 1 }),
            }
        }
        out
    }

    /// The reply a turn ended with: its last text, when only notices follow it. A turn that was cut
    /// short (an error, an interrupt before any text) has none.
    pub fn answer(&self, turn: Turn) -> Option<usize> {
        for i in (turn.start..turn.end).rev() {
            match &self.items[i] {
                Item::Notice { .. } | Item::Done { .. } => {}
                Item::Assistant(_) => return Some(i),
                _ => return None,
            }
        }
        None
    }

    /// The files a turn's edits and writes changed, each once, in the order they were first changed.
    pub fn changed_files(&self, turn: Turn) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for item in &self.items[turn.start..turn.end] {
            if let Item::Tool(t) = item
                && matches!(t.name.as_str(), "edit" | "write")
                && t.is_error == Some(false)
                && let Some(path) = t.input.get("path").and_then(|p| p.as_str())
                && !out.contains(&path)
            {
                out.push(path);
            }
        }
        out
    }

    /// Whether an item gets a row of its own in an open turn. The task list has its own place, a
    /// request heads the turn, and of approvals only a refusal is a row: one that waits is asked at
    /// the foot of the sheet, and one that was allowed is the step that follows it.
    fn is_row(&self, i: usize) -> bool {
        match &self.items[i] {
            Item::Tool(t) => t.name != "todo_write",
            Item::Approval(a) => matches!(a.answer, Some(Verdict::Denied | Verdict::Cancelled)),
            Item::User(_) | Item::Done { .. } => false,
            Item::Assistant(_) | Item::Thinking(_) | Item::Notice { .. } => true,
        }
    }

    /// A finished turn shows its answer and folds the work before it. A turn without an answer (still
    /// running, or cut short) shows its latest rows and folds what scrolled past.
    pub fn layout(&self, turn: Turn, live: bool) -> Layout {
        let from = turn.start + usize::from(matches!(self.items[turn.start], Item::User(_)));
        let rows: Vec<usize> = (from..turn.end).filter(|i| self.is_row(*i)).collect();
        let cut = match self.answer(turn) {
            Some(answer) if !live => rows.iter().position(|i| *i == answer).unwrap_or(0),
            // One row is not worth a line that says "1 more".
            _ => Some(rows.len().saturating_sub(LIVE_ROWS))
                .filter(|n| *n > 1)
                .unwrap_or(0),
        };
        Layout {
            folded: rows[..cut].to_vec(),
            shown: rows[cut..].to_vec(),
        }
    }

    pub fn standing(&self, turn: Turn, live: bool) -> Standing {
        let items = &self.items[turn.start..turn.end];
        if live {
            let asking = items
                .iter()
                .any(|i| matches!(i, Item::Approval(a) if a.answer.is_none()));
            return if asking { Standing::Asking } else { Standing::Running };
        }
        if items.iter().any(|i| matches!(i, Item::Notice { error: true, .. })) {
            Standing::Failed
        } else if self.answer(turn).is_some() {
            Standing::Done
        } else {
            Standing::Cut
        }
    }

    /// The first line of a turn's answer as plain words, for the one line a closed turn gets.
    pub fn preview(&self, turn: Turn) -> Option<String> {
        let Item::Assistant(md) = &self.items[self.answer(turn)?] else {
            return None;
        };
        // What is in a code block or a table is not a sentence.
        let mut fenced = false;
        let lines: Vec<&str> = md
            .raw
            .lines()
            .filter(|line| {
                let line = line.trim_start();
                let fence = line.starts_with("```");
                fenced ^= fence;
                !(fenced || fence || line.starts_with('|'))
            })
            .collect();
        // A sentence says more than a heading; an answer that is only headings still gets one.
        let words = |heading: bool| {
            lines
                .iter()
                .filter(|l| l.trim_start().starts_with('#') == heading)
                .map(|l| plain(l))
                .find(|l| !l.is_empty())
        };
        words(false).or_else(|| words(true))
    }

    /// The request a turn starts with.
    pub fn request(&self, turn: Turn) -> Option<&str> {
        match &self.items[turn.start] {
            Item::User(text) => Some(text),
            _ => None,
        }
    }

    /// Everything the conversation's edits and writes did, by file, in the order the files were
    /// first changed.
    pub fn changes(&self) -> Vec<FileChange<'_>> {
        let mut out: Vec<FileChange<'_>> = Vec::new();
        for item in &self.items {
            let Item::Tool(t) = item else { continue };
            if t.is_error != Some(false) {
                continue;
            }
            let (Some(path), Some(lines)) = (t.input.get("path").and_then(|p| p.as_str()), change(&t.name, &t.input))
            else {
                continue;
            };
            let added = lines.iter().filter(|(sign, _)| *sign == '+').count();
            let removed = lines.len() - added;
            match out.iter_mut().find(|f| f.path == path) {
                Some(file) => {
                    file.hunks.push(lines);
                    file.added += added;
                    file.removed += removed;
                }
                None => out.push(FileChange {
                    path,
                    hunks: vec![lines],
                    added,
                    removed,
                }),
            }
        }
        out
    }

    fn refused(&self, call: &str) -> bool {
        self.items
            .iter()
            .any(|i| matches!(i, Item::Approval(a) if a.call == call && a.answer == Some(Verdict::Denied)))
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
                            Content::ToolCall(call) => {
                                t.read_todos(call);
                                t.items.push(Item::Tool(Tool {
                                    id: call.id.clone(),
                                    name: call.name.clone(),
                                    summary: summarize(call),
                                    input: call.input.clone(),
                                    output: String::new(),
                                    is_error: None,
                                }))
                            }
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
    brief(&call.input)
}

/// The same for bare arguments, as an approval request carries them.
pub fn brief(input: &serde_json::Value) -> String {
    for key in ["command", "path", "file_path", "pattern", "url", "query"] {
        if let Some(v) = input.get(key).and_then(|v| v.as_str()) {
            return first_line(v);
        }
    }
    first_line(&input.to_string())
}

fn describe(args: &serde_json::Value) -> String {
    match args.get("command").and_then(|v| v.as_str()) {
        Some(command) => command.to_owned(),
        None => serde_json::to_string_pretty(args).unwrap_or_default(),
    }
}

/// The change an `edit` or a `write` makes, read from its arguments: `-` lines leave, `+` lines come.
/// Lines an edit keeps at both ends are left out. `None` for any other tool.
pub fn change(tool: &str, input: &serde_json::Value) -> Option<Vec<(char, String)>> {
    let field = |key: &str| input.get(key).and_then(|v| v.as_str());
    match tool {
        "write" => Some(field("content")?.lines().map(|l| ('+', l.to_owned())).collect()),
        "edit" => {
            let old: Vec<&str> = field("old_string")?.lines().collect();
            let new: Vec<&str> = field("new_string")?.lines().collect();
            let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
            let tail = old[head..]
                .iter()
                .rev()
                .zip(new[head..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            let gone = old[head..old.len() - tail].iter().map(|l| ('-', (*l).to_owned()));
            let come = new[head..new.len() - tail].iter().map(|l| ('+', (*l).to_owned()));
            Some(gone.chain(come).collect())
        }
        _ => None,
    }
}

/// A line of Markdown as plain words: without the marks that open a heading, a list item or a quote,
/// and without the marks around code and emphasis.
fn plain(line: &str) -> String {
    let line = line.trim().trim_start_matches(['#', '>', '-', '*', '+']).trim_start();
    line.chars()
        .filter(|c| !matches!(c, '`' | '*'))
        .collect::<String>()
        .trim()
        .to_owned()
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
                Item::Done { seconds } => format!("done:{seconds}"),
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
    fn a_conversation_is_cut_at_each_request_and_a_turn_knows_its_answer_and_its_changed_files() {
        let call = |id: &str, name: &str, path: &str| {
            let input = serde_json::json!({ "path": path, "old_string": "a", "new_string": "b" });
            Item::Tool(Tool {
                id: id.into(),
                name: name.into(),
                summary: path.into(),
                input,
                output: String::new(),
                is_error: Some(false),
            })
        };
        let items = vec![
            Item::User("直して".into()),
            Item::Assistant(Markdown::new("見てみます。")),
            call("1", "read", "src/a.rs"),
            call("2", "edit", "src/a.rs"),
            call("3", "edit", "src/a.rs"),
            call("4", "write", "src/b.rs"),
            Item::Assistant(Markdown::new("直しました。")),
            Item::Done { seconds: 12 },
            Item::User("もう一度".into()),
            call("5", "bash", "x"),
            Item::Notice {
                text: "中断しました".into(),
                error: false,
            },
        ];
        let t = Transcript {
            items,
            ..Transcript::default()
        };
        let turns = t.turns();
        assert_eq!(turns, [Turn { start: 0, end: 8 }, Turn { start: 8, end: 11 }]);
        assert_eq!(t.answer(turns[0]), Some(6));
        assert_eq!(t.changed_files(turns[0]), ["src/a.rs", "src/b.rs"]);
        // Cut short before any text: there is no answer to set apart.
        assert_eq!(t.answer(turns[1]), None);
    }

    #[test]
    fn an_edit_is_shown_as_the_lines_that_leave_and_come_without_the_lines_it_keeps() {
        let input =
            serde_json::json!({ "path": "a", "old_string": "fn f() {\n  1\n}", "new_string": "fn f() {\n  2\n  3\n}" });
        let lines = change("edit", &input).unwrap();
        assert_eq!(
            lines,
            [
                ('-', "  1".to_owned()),
                ('+', "  2".to_owned()),
                ('+', "  3".to_owned())
            ]
        );
        let written = change("write", &serde_json::json!({ "path": "a", "content": "x\ny" })).unwrap();
        assert_eq!(written, [('+', "x".to_owned()), ('+', "y".to_owned())]);
        assert_eq!(change("bash", &serde_json::json!({ "command": "ls" })), None);
    }

    #[test]
    fn the_task_list_follows_the_last_todo_write_and_a_message_sent_mid_run_joins_when_delivered() {
        let s = Strings::get("ja");
        let mut t = Transcript::default();
        let start = r#"{"type":"tool_start","call":{"id":"t1","name":"todo_write","input":{"todos":[{"content":"調べる","status":"completed"},{"content":"直す","status":"in_progress"}]}}}"#;
        t.apply(&parse_event(start).unwrap(), &s);
        assert_eq!(t.todos.len(), 2);
        assert_eq!(t.todos[1].state, TodoState::Active);
        assert_eq!(t.activity, Activity::Tool("todo_write".into()));
        let echo =
            r#"{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"テストも足して"}]}}"#;
        let effect = t.apply(&parse_event(echo).unwrap(), &s);
        assert_eq!(effect.delivered.as_deref(), Some("テストも足して"));
        assert!(matches!(t.items.last(), Some(Item::User(u)) if u == "テストも足して"));
    }

    fn tool(id: &str, name: &str, input: serde_json::Value, is_error: Option<bool>) -> Item {
        Item::Tool(Tool {
            id: id.into(),
            name: name.into(),
            summary: brief(&input),
            input,
            output: String::new(),
            is_error,
        })
    }

    fn asked(id: &str, call: &str, answer: Option<Verdict>) -> Item {
        Item::Approval(Approval {
            id: id.into(),
            call: call.into(),
            tool: "bash".into(),
            detail: "bun test".into(),
            args: serde_json::json!({ "command": "bun test" }),
            reason: "mode ask".into(),
            answer,
        })
    }

    #[test]
    fn a_finished_turn_folds_its_work_behind_the_answer_and_a_running_one_keeps_its_latest_rows() {
        let run = || serde_json::json!({ "command": "bun test" });
        let mut t = Transcript {
            items: vec![
                Item::User("直して".into()),
                Item::Thinking("まず読む。".into()),
                tool("1", "read", serde_json::json!({ "path": "a.rs" }), Some(false)),
                Item::Assistant(Markdown::new("直します。")),
                tool("2", "edit", serde_json::json!({ "path": "a.rs" }), Some(false)),
                tool("3", "todo_write", serde_json::json!({ "todos": [] }), Some(false)),
                tool("4", "bash", run(), Some(false)),
            ],
            ..Transcript::default()
        };
        let live = |t: &Transcript| t.layout(t.turns()[0], true);
        // The task list is not a row. Five rows are all on the sheet, and so are six: a line that
        // says one more is hidden would take the room of the row it hides.
        assert_eq!(live(&t).shown, [1, 2, 3, 4, 6]);
        t.items.push(tool("5", "bash", run(), Some(false)));
        assert_eq!(live(&t).folded, [] as [usize; 0]);
        t.items.push(tool("6", "bash", run(), None));
        let seven = Layout {
            folded: vec![1, 2],
            shown: vec![3, 4, 6, 7, 8],
        };
        assert_eq!(live(&t), seven);

        t.items.push(Item::Assistant(Markdown::new("直しました。")));
        t.items.push(Item::Done { seconds: 9 });
        let turn = t.turns()[0];
        let done = Layout {
            folded: vec![1, 2, 3, 4, 6, 7, 8],
            shown: vec![9],
        };
        assert_eq!(t.layout(turn, false), done);
        assert_eq!(t.standing(turn, false), Standing::Done);
        assert_eq!(t.standing(turn, true), Standing::Running);
    }

    #[test]
    fn a_turn_stands_by_what_it_waits_for_and_how_it_ended() {
        let stands = |rest: Vec<Item>, live: bool| {
            let mut items = vec![Item::User("直して".into())];
            items.extend(rest);
            let t = Transcript {
                items,
                ..Transcript::default()
            };
            t.standing(t.turns()[0], live)
        };
        assert_eq!(stands(vec![asked("a", "1", None)], true), Standing::Asking);
        assert_eq!(
            stands(vec![asked("a", "1", Some(Verdict::Allowed))], true),
            Standing::Running
        );
        let failed = Item::Notice {
            text: "401".into(),
            error: true,
        };
        assert_eq!(
            stands(vec![Item::Assistant(Markdown::new("途中です。")), failed], false),
            Standing::Failed
        );
        assert_eq!(stands(vec![Item::Thinking("考える。".into())], false), Standing::Cut);
    }

    #[test]
    fn a_closed_turn_is_told_by_the_first_sentence_of_its_answer() {
        let preview = |answer: &str| {
            let t = Transcript {
                items: vec![Item::User("直して".into()), Item::Assistant(Markdown::new(answer))],
                ..Transcript::default()
            };
            t.preview(t.turns()[0])
        };
        assert_eq!(
            preview("## 結果\n\n型チェックは**通りました**。").as_deref(),
            Some("型チェックは通りました。")
        );
        assert_eq!(
            preview("`sum()` が 1 を足していました。").as_deref(),
            Some("sum() が 1 を足していました。")
        );
        assert_eq!(preview("```\nls\n```\n- 一覧です").as_deref(), Some("一覧です"));
        assert_eq!(preview("# 結果").as_deref(), Some("結果"));
        assert_eq!(preview("| a | b |"), None);
    }

    #[test]
    fn the_changed_files_are_each_listed_once_with_every_change_that_was_made() {
        let edit = |old: &str, new: &str| serde_json::json!({ "path": "a.rs", "old_string": old, "new_string": new });
        let t = Transcript {
            items: vec![
                Item::User("直して".into()),
                tool("1", "edit", edit("1", "2\n3"), Some(false)),
                tool(
                    "2",
                    "write",
                    serde_json::json!({ "path": "b.rs", "content": "x" }),
                    Some(false),
                ),
                tool("3", "edit", edit("3", "4"), Some(false)),
                // A change that failed, and one that has not been made yet, changed nothing.
                tool("4", "edit", edit("5", "6"), Some(true)),
                tool("5", "edit", edit("7", "8"), None),
            ],
            ..Transcript::default()
        };
        let files = t.changes();
        assert_eq!(files.iter().map(|f| f.path).collect::<Vec<_>>(), ["a.rs", "b.rs"]);
        assert_eq!((files[0].hunks.len(), files[0].added, files[0].removed), (2, 3, 2));
        assert_eq!((files[1].hunks.len(), files[1].added, files[1].removed), (1, 1, 0));
    }

    #[test]
    fn a_refused_call_is_one_row_and_an_allowed_one_is_only_its_step() {
        let s = Strings::get("ja");
        let mut t = Transcript::default();
        t.send("テストして");
        let ask = |id: &str, call: &str| {
            format!(
                r#"{{"type":"approval_request","id":"{id}","callId":"{call}","tool":"bash","args":{{"command":"bun test"}},"reason":"mode ask","cwd":"/x"}}"#
            )
        };
        let end = |call: &str, error: bool| {
            format!(
                r#"{{"type":"tool_end","call":{{"type":"tool_call","id":"{call}","name":"bash","input":{{"command":"bun test"}}}},"result":{{"content":[{{"type":"text","text":"x"}}],"isError":{error}}}}}"#
            )
        };
        t.apply(&parse_event(&ask("a1", "c1")).unwrap(), &s);
        assert_eq!(t.pending_approvals(), 1);
        // While it waits it is asked at the foot of the sheet, not in the conversation.
        assert_eq!(t.layout(t.turns()[0], true).shown, [] as [usize; 0]);
        t.answer_approval("a1", Verdict::Denied);
        t.apply(&parse_event(&end("c1", true)).unwrap(), &s);
        assert_eq!(kinds(&t)[1..], ["approval:bash:bun test:Some(Denied)"]);
        assert_eq!(t.layout(t.turns()[0], true).shown, [1]);

        t.apply(&parse_event(&ask("a2", "c2")).unwrap(), &s);
        t.answer_approval("a2", Verdict::Allowed);
        t.apply(&parse_event(&end("c2", false)).unwrap(), &s);
        assert_eq!(t.pending_approval(), None);
        assert_eq!(t.layout(t.turns()[0], true).shown, [1, 3]);
    }

    #[test]
    fn long_tool_output_keeps_its_end() {
        let long = "あ".repeat(MAX_TOOL_OUTPUT);
        let cut = tail(&long);
        assert!(cut.starts_with('…') && cut.len() <= MAX_TOOL_OUTPUT + '…'.len_utf8());
    }
}
