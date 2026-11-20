//! The conversation as the window shows it, built from a run's events or from a session log.
//! Kept free of drawing and of wording, so it can be tested without a window.
//!
//! A conversation is a list of [`Exchange`]s: one message of the user's and everything that
//! answered it. What the events do not say is not made up here: a call that was never started
//! (denied, refused, interrupted) has no `tool_start`, only its result message; a subagent's own
//! calls arrive as lines of its `tool_update`, not as events; a log has no approvals, no durations
//! and no diffs, so a change read back from one is rebuilt from the call's arguments.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use iced::widget::markdown;
use sasacode_protocol::event::{Command, NoticeLevel, UiAsk};
use sasacode_protocol::{
    Content, Event, Message, SessionEntry, StopCause, StreamDelta, ToolCall, ToolResult, Usage, text_of,
};
use serde_json::Value;

use crate::diff::Diff;

/// Tool output longer than this is cut, from the front: the end is what tells how it went.
const MAX_TOOL_OUTPUT: usize = 8_000;

/// What the harness puts in front of a note it sends the model in the user's place (`NOTE_PREFIX`
/// in `crates/sasacode-host/src/notes.rs`). It arrives as a user message, and is not one.
pub const NOTE_PREFIX: &str = "[Reminder from sasacode, not from the user] ";

/// What the agent puts in front of the first message it is given after an interruption
/// (`INTERRUPTED_NOTE` in `crates/sasacode-agent/src/agent.rs`): a part of its own, and not the user's words.
const INTERRUPTED: &str = "[The user interrupted the previous response.]";

/// What the harness answers in the place of a call the user said no to
/// (`crates/sasacode-agent/src/tool_exec.rs`).
const DENIED: &str = "The user denied this tool call.";
/// How the answers begin that stand for a call that was never run: denied by the user, cut off
/// before its turn, or refused by a rule or a plugin. A log keeps nothing else of that.
const NOT_RUN: [&str; 3] = [
    DENIED,
    "Interrupted by the user before this tool ran.",
    "Permission denied (",
];

/// One message of the user's and everything that answered it.
#[derive(Debug, Default)]
pub struct Exchange {
    pub prompt: String,
    pub steps: Vec<Step>,
    /// How the run ended, when it ended in this exchange. A log does not say.
    pub end: Option<End>,
    pub started: Option<Instant>,
    /// From the prompt to the end of the run.
    pub elapsed: Option<Duration>,
    /// When the message was sent, by the clock: as the log has it, or when it was sent here.
    pub at: Option<SystemTime>,
    /// Read from a log: the entry of the log the message is, where a conversation can be branched
    /// off before it. None for one sent here, and for one only a compaction's rewrite has.
    pub entry: Option<usize>,
}

#[derive(Debug)]
pub enum Step {
    Thinking(String),
    /// Boxed: parsed Markdown is several times the size of the other steps.
    Text(Box<Markdown>),
    /// Boxed: a call is several times the size of the other steps.
    Tool(Box<Tool>),
    Approval(Approval),
    Note(Note),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// The harness told the model something in the user's place.
    Harness(String),
    /// A background task reported back.
    Task {
        head: String,
        report: String,
    },
    Error(String),
    ContextFull,
    /// The conversation was summarised to save room: the summary, once it is known.
    Compacted(Option<String>),
    /// A reply was thrown away and is asked again.
    Retrying,
    /// A plugin's warning or error.
    Notice(String),
    /// A text a plugin showed whole.
    Text {
        title: String,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct End {
    pub cause: StopCause,
    /// The plugin that stopped the run.
    pub plugin: Option<String>,
    /// The plugin's reason, or the error.
    pub reason: Option<String>,
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
            content: markdown::Content::parse(&crate::figure::prepare(&crate::figure::alerts(text))),
        }
    }

    /// More of it as it streams. With math in it the whole is parsed again: where its math ends
    /// changes what came before.
    pub fn push(&mut self, delta: &str) {
        self.raw.push_str(delta);
        match crate::figure::prepare(&self.raw) {
            std::borrow::Cow::Borrowed(_) => self.content.push_str(delta),
            std::borrow::Cow::Owned(prepared) => self.content = markdown::Content::parse(&prepared),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    /// The model asked for it; it has not started (it may wait for an approval).
    Waiting,
    Running,
    Done,
    Failed,
    /// It never ran: denied, refused by a rule or a plugin, or interrupted first.
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Read,
    Write,
    Edit,
    Bash,
    Task,
    Plan,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub id: String,
    pub name: String,
    pub input: Value,
    pub summary: String,
    /// What it wrote while it ran. For a subagent: one line per call it made.
    pub stream: String,
    /// Its result.
    pub output: String,
    /// How many lines the whole result has (`output` keeps only the end of a long one).
    pub lines: usize,
    pub state: ToolState,
    /// bash: `None` while it runs and when it was killed.
    pub exit_code: Option<i64>,
    /// bash: `abort` or `timeout`.
    pub killed: Option<String>,
    /// write and edit: what the call changes. Before it ran, and in a log, as its arguments say.
    pub change: Option<Change>,
    /// todo_write.
    pub plan: Option<PlanUpdate>,
    /// What tool-repair mended in the call before it ran.
    pub repaired: Option<String>,
    /// What the agent mode's judge said of it: allowed, or handed to the user, and why.
    pub judged: Option<(bool, String)>,
    /// The call that made this one (a tool calling another tool).
    pub parent: Option<String>,
    pub started: Option<Instant>,
    pub elapsed: Option<Duration>,
    /// The pictures its result holds (`read` of an image, an MCP tool's), each kept as a file to
    /// be drawn from.
    pub images: Vec<std::path::PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// As the call names it.
    pub path: String,
    /// Whether the file was not there before. Known once the call ran; a log does not say.
    pub created: Option<bool>,
    pub diff: Diff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Todo {
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanUpdate {
    pub todos: Vec<Todo>,
    /// No plan stood before this call.
    pub first: bool,
    /// What this call completed that the plan before it had not.
    pub newly_done: Vec<String>,
}

impl PlanUpdate {
    pub fn done(&self) -> usize {
        self.todos.iter().filter(|t| t.status == TodoStatus::Completed).count()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Approval {
    pub id: String,
    pub call_id: String,
    pub tool: String,
    pub args: Value,
    pub reason: String,
    pub cwd: String,
    /// The allow rules "always" adds, which later runs of the conversation are given (`--allow`).
    pub rules: Vec<String>,
    pub answer: Option<Answer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Allowed,
    /// Allowed, and the same call is not asked again in this run.
    Always,
    /// With what the user told the model, if anything.
    Denied(String),
    /// The run was interrupted before it was answered.
    Cancelled,
}

#[derive(Debug, Default)]
pub struct Transcript {
    pub exchanges: Vec<Exchange>,
    /// Messages sent while a run was busy that the model has not been given yet.
    pub queued: Vec<String>,
    pub usage: Usage,
    /// The model responses `usage` counts.
    pub requests: u32,
    /// What the newest reply read: the size of the conversation as the model last saw it.
    pub last_input: u64,
    pub context_window: Option<u64>,
    /// `provider/model` of the newest reply.
    pub model: Option<String>,
    /// Content index of the streaming reply → its step in the last exchange.
    streaming: HashMap<usize, usize>,
    /// Prompts shown when they were sent, so the run's echo of them is not shown twice.
    echoes: Vec<String>,
    /// Call id → what was mended, until the call itself arrives.
    repairs: HashMap<String, String>,
    /// Read from a log: a tool's result message is all there is of its run.
    replay: bool,
    /// While a log is read: the entry being read, when it is a message of its own.
    at_entry: Option<usize>,
    /// The plugins' questions, open until they are answered or the run ends.
    pub asks: Vec<Ask>,
    /// The plugins' status items, by `plugin:key`, as the newest run left them.
    pub status: std::collections::BTreeMap<String, String>,
}

/// How the summary compaction writes begins (`packages/bundled/src/compaction.ts`).
/// What a stderr line warns of, as the CLI writes a warning in either language.
pub fn warning_of(line: &str) -> Option<&str> {
    ["warning: ", "警告: "]
        .iter()
        .find_map(|p| line.strip_prefix(p))
        .map(str::trim)
        .filter(|w| !w.is_empty())
}

pub const SUMMARY: &str = "[Summary of the earlier conversation, compacted to save context]";

/// A plugin's question.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub id: String,
    pub title: String,
    pub ask: UiAsk,
    /// The values ticked so far, when several may be.
    pub chosen: std::collections::BTreeSet<String>,
    /// What is typed so far, when the answer is a text.
    pub typed: String,
}

/// What an event changed beyond the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Effect {
    pub session_id: Option<String>,
    pub ended: Option<StopCause>,
    /// An approval started to wait.
    pub approval: Option<String>,
    /// A plugin asked something.
    pub asked: Option<String>,
    /// A plugin's notice.
    pub notice: Option<(NoticeLevel, String)>,
    /// The plugins' slash commands, as they are now.
    pub commands: Option<Vec<Command>>,
    /// The conversation was summarised: the summary is in its log.
    pub compacted: bool,
}

/// The pictures some content holds, each kept as a file to draw.
fn pictures(content: &[Content]) -> Vec<std::path::PathBuf> {
    content
        .iter()
        .filter_map(|c| match c {
            Content::Image { media_type, data } => crate::picture::kept(media_type, data),
            _ => None,
        })
        .collect()
}

impl Tool {
    fn new(call: &ToolCall) -> Self {
        let mut t = Tool {
            id: call.id.clone(),
            name: call.name.clone(),
            input: call.input.clone(),
            summary: summarize(call),
            stream: String::new(),
            output: String::new(),
            lines: 0,
            state: ToolState::Waiting,
            exit_code: None,
            killed: None,
            change: None,
            plan: None,
            repaired: None,
            judged: None,
            parent: None,
            started: None,
            elapsed: None,
            images: Vec::new(),
        };
        t.change = match t.kind() {
            Kind::Edit => t.arg("path").map(|path| Change {
                path: path.to_owned(),
                created: Some(false),
                // Shown with no control character and none that would reorder a line.
                diff: Diff::replace(
                    &crate::plain::plain(t.arg("old_string").unwrap_or("")),
                    &crate::plain::plain(t.arg("new_string").unwrap_or("")),
                ),
            }),
            Kind::Write => t.arg("path").map(|path| Change {
                path: path.to_owned(),
                created: None,
                diff: Diff::create(&crate::plain::plain(t.arg("content").unwrap_or(""))),
            }),
            _ => None,
        };
        t
    }

    pub fn kind(&self) -> Kind {
        match self.name.as_str() {
            "read" => Kind::Read,
            "write" => Kind::Write,
            "edit" => Kind::Edit,
            "bash" => Kind::Bash,
            "task" => Kind::Task,
            "todo_write" => Kind::Plan,
            _ => Kind::Other,
        }
    }

    /// A string argument of the call.
    pub fn arg(&self, key: &str) -> Option<&str> {
        self.input.get(key).and_then(Value::as_str)
    }

    pub fn finished(&self) -> bool {
        !matches!(self.state, ToolState::Waiting | ToolState::Running)
    }

    /// Whether it was not run because the user said no to it.
    pub fn denied(&self) -> bool {
        self.state == ToolState::Skipped && self.output.starts_with(DENIED)
    }

    /// What it shows as output: the result once there is one, else what it wrote so far.
    pub fn shown_output(&self) -> &str {
        if self.output.is_empty() {
            &self.stream
        } else {
            &self.output
        }
    }

    fn set_output(&mut self, text: &str) {
        self.lines = text.trim_end().lines().count();
        self.output = tail(text);
    }

    fn finish(&mut self, result: &ToolResult, now: Option<Instant>) {
        self.set_output(&text_of(&result.content));
        self.images = pictures(&result.content);
        self.state = if result.is_error {
            ToolState::Failed
        } else {
            ToolState::Done
        };
        self.elapsed = self.started.zip(now).map(|(a, b)| b.saturating_duration_since(a));
        let Some(d) = &result.details else { return };
        self.exit_code = d.get("exitCode").and_then(Value::as_i64);
        self.killed = d.get("killedBy").and_then(Value::as_str).map(str::to_owned);
        match self.kind() {
            Kind::Edit | Kind::Write => {
                if d.get("changed").and_then(Value::as_bool) == Some(false) {
                    self.change = None;
                } else if let Some(change) = &mut self.change {
                    change.created = d.get("created").and_then(Value::as_bool).or(change.created);
                    if let Some(diff) = d.get("diff").and_then(Value::as_str) {
                        change.diff = Diff::parse(diff);
                    }
                }
            }
            Kind::Plan => {
                if let (Some(plan), Some(todos)) = (&mut self.plan, d.get("todos").and_then(todos_of)) {
                    plan.todos = todos;
                }
            }
            _ => {}
        }
    }
}

impl Exchange {
    pub fn tools(&self) -> impl Iterator<Item = &Tool> {
        self.steps.iter().filter_map(|s| match s {
            Step::Tool(t) => Some(&**t),
            _ => None,
        })
    }

    pub fn failed(&self) -> usize {
        self.tools().filter(|t| t.state == ToolState::Failed).count()
    }

    /// The changes that were made, in the order they were made.
    pub fn changes(&self) -> impl Iterator<Item = (&Tool, &Change)> {
        self.tools()
            .filter(|t| t.state == ToolState::Done)
            .filter_map(|t| t.change.as_ref().map(|c| (t, c)))
    }

    /// All the model wrote in the exchange, its texts one after another.
    pub fn answer_text(&self) -> String {
        let texts: Vec<&str> = self
            .steps
            .iter()
            .filter_map(|s| match s {
                Step::Text(md) => Some(md.raw.trim()),
                _ => None,
            })
            .filter(|t| !t.is_empty())
            .collect();
        texts.join("\n\n")
    }

    /// The step that is the reply to the prompt: the text the exchange ends with.
    pub fn answer(&self) -> Option<usize> {
        match self.steps.last() {
            Some(Step::Text(_)) => Some(self.steps.len() - 1),
            _ => None,
        }
    }
}

impl Transcript {
    /// Shows a prompt as soon as it is sent.
    pub fn send(&mut self, text: &str, now: Instant) {
        self.exchanges.push(Exchange {
            prompt: text.to_owned(),
            started: Some(now),
            at: Some(SystemTime::now()),
            ..Exchange::default()
        });
        self.echoes.push(text.to_owned());
    }

    /// A message sent while the run is busy: the model gets it at its next turn.
    pub fn queue(&mut self, text: &str) {
        self.queued.push(text.to_owned());
    }

    pub fn note(&mut self, note: Note) {
        self.current().steps.push(Step::Note(note));
    }

    /// A warning the CLI wrote on stderr, noted once in the conversation: each run says it again.
    pub fn warn(&mut self, text: &str) {
        let seen = self
            .exchanges
            .iter()
            .flat_map(|e| &e.steps)
            .any(|s| matches!(s, Step::Note(Note::Notice(t)) if t == text));
        if !seen {
            self.note(Note::Notice(text.to_owned()));
        }
    }

    /// Whether a compaction's summary is not known yet.
    pub fn summary_missing(&self) -> bool {
        self.exchanges
            .iter()
            .flat_map(|e| &e.steps)
            .any(|s| matches!(s, Step::Note(Note::Compacted(None))))
    }

    /// The newest compaction's summary, read from the log once the run has written it.
    pub fn fill_summary(&mut self, entries: &[SessionEntry]) {
        let Some(summary) = entries.iter().rev().find_map(|e| match e {
            SessionEntry::Replace { messages } => messages.iter().find_map(|m| match m {
                Message::User { content, .. } => said(content).strip_prefix(SUMMARY).map(|s| s.trim().to_owned()),
                _ => None,
            }),
            _ => None,
        }) else {
            return;
        };
        let last = self
            .exchanges
            .iter_mut()
            .rev()
            .flat_map(|e| e.steps.iter_mut().rev())
            .find_map(|s| match s {
                Step::Note(Note::Compacted(text)) => Some(text),
                _ => None,
            });
        if let Some(text @ None) = last {
            *text = Some(summary);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.exchanges.is_empty() && self.queued.is_empty()
    }

    /// The first prompt.
    pub fn title(&self) -> Option<&str> {
        self.exchanges
            .iter()
            .map(|e| e.prompt.as_str())
            .find(|p| !p.trim().is_empty())
    }

    pub fn tools(&self) -> impl Iterator<Item = &Tool> {
        self.exchanges.iter().flat_map(Exchange::tools)
    }

    /// Whether the run waits for the user: an approval, or a plugin's question.
    pub fn waits(&self) -> bool {
        self.pending_approval().is_some() || !self.asks.is_empty()
    }

    pub fn tool(&self, id: &str) -> Option<&Tool> {
        self.tools().find(|t| t.id == id)
    }

    pub fn changes(&self) -> impl Iterator<Item = (&Tool, &Change)> {
        self.exchanges.iter().flat_map(Exchange::changes)
    }

    /// The bash calls that ran or run.
    pub fn commands(&self) -> impl Iterator<Item = &Tool> {
        self.tools()
            .filter(|t| t.kind() == Kind::Bash && !matches!(t.state, ToolState::Waiting | ToolState::Skipped))
    }

    /// The plan as the newest `todo_write` that ran left it.
    pub fn plan(&self) -> Option<&[Todo]> {
        self.exchanges
            .iter()
            .rev()
            .flat_map(|e| e.steps.iter().rev())
            .find_map(|s| match s {
                Step::Tool(t) if !matches!(t.state, ToolState::Failed | ToolState::Skipped) => {
                    t.plan.as_ref().map(|p| p.todos.as_slice())
                }
                _ => None,
            })
    }

    fn approvals(&self) -> impl Iterator<Item = &Approval> {
        self.exchanges.iter().flat_map(|e| &e.steps).filter_map(|s| match s {
            Step::Approval(a) => Some(a),
            _ => None,
        })
    }

    /// The approval that was asked for a call, if one was.
    pub fn approval_of(&self, call_id: &str) -> Option<&Approval> {
        self.approvals().find(|a| a.call_id == call_id)
    }

    /// The approval asked as `id`.
    pub fn approval_by_id(&self, id: &str) -> Option<&Approval> {
        self.approvals().find(|a| a.id == id)
    }

    /// The approval to answer next: the one that has waited longest.
    pub fn pending_approval(&self) -> Option<&Approval> {
        self.approvals().find(|a| a.answer.is_none())
    }

    pub fn pending_approvals(&self) -> usize {
        self.approvals().filter(|a| a.answer.is_none()).count()
    }

    pub fn answer_approval(&mut self, id: &str, answer: Answer) {
        for step in self.exchanges.iter_mut().rev().flat_map(|e| e.steps.iter_mut().rev()) {
            if let Step::Approval(a) = step
                && a.id == id
            {
                a.answer.get_or_insert(answer);
                return;
            }
        }
    }

    /// An approval this window did not answer itself was answered all the same (by a rule made
    /// meanwhile, by another client): what happened to its call says how.
    fn infer_answer(&mut self, call_id: &str, answer: Answer) {
        for step in self.exchanges.iter_mut().rev().flat_map(|e| e.steps.iter_mut().rev()) {
            if let Step::Approval(a) = step
                && a.call_id == call_id
            {
                a.answer.get_or_insert(answer);
                return;
            }
        }
    }

    /// The run's process is gone: nothing waits or runs any more, whatever the events said last.
    pub fn close(&mut self, now: Instant) {
        self.settle(now);
        self.streaming.clear();
        // Nobody is left to hear the answers.
        self.asks.clear();
    }

    pub fn apply(&mut self, event: &Event, now: Instant) -> Effect {
        let mut effect = Effect::default();
        match event {
            Event::Session { id, context_window, .. } => {
                effect.session_id = id.clone();
                if context_window.is_some() {
                    self.context_window = *context_window;
                }
            }
            Event::AgentStart => {
                self.current().started.get_or_insert(now);
            }
            Event::MessageStart => self.streaming.clear(),
            Event::MessageUpdate { event } => self.stream(event),
            Event::MessageEnd { message } => self.message(message, Some(now), true),
            Event::ToolStart {
                call,
                summary,
                parent_call_id,
            } => {
                self.infer_answer(&call.id, Answer::Allowed);
                let tool = self.tool_or_new(call);
                tool.state = ToolState::Running;
                tool.started = Some(now);
                tool.parent = parent_call_id.clone();
                if !summary.is_empty() {
                    tool.summary = first_line(summary);
                }
            }
            Event::ToolUpdate { call, text, .. } => {
                if let Some(tool) = self.tool_mut(&call.id) {
                    tool.stream.push_str(text);
                    tool.stream = tail(&tool.stream);
                }
            }
            Event::ToolEnd { call, result, .. } => self.tool_or_new(call).finish(result, Some(now)),
            Event::Usage { usage, .. } => {
                self.usage.add(usage);
                self.requests += 1;
            }
            Event::ContextLimit { tokens, context_window } => {
                self.last_input = *tokens;
                self.context_window = Some(*context_window);
                self.note(Note::ContextFull);
            }
            Event::Error { error } => self.note(Note::Error(error.clone())),
            Event::PluginError { plugin, hook, error } => self.note(Note::Error(format!("{plugin} ({hook}): {error}"))),
            Event::MessagesReplaced => {
                self.note(Note::Compacted(None));
                effect.compacted = true;
            }
            Event::MessageDiscarded { .. } => {
                // The reply that streamed in is asked again: drop what was shown of it.
                let shown: Vec<usize> = self.streaming.drain().map(|(_, i)| i).collect();
                let steps = &mut self.current().steps;
                let mut i = 0;
                steps.retain(|_| {
                    i += 1;
                    !shown.contains(&(i - 1))
                });
                self.note(Note::Retrying);
            }
            Event::ToolRepaired { call, note, .. } => {
                self.repairs.insert(call.id.clone(), note.clone());
            }
            Event::AgentEnd { cause, stopped, error } => {
                effect.ended = Some(*cause);
                self.settle(now);
                let ex = self.current();
                // The `error` event before this one said the same thing.
                if let (Some(error), Some(Step::Note(Note::Error(shown)))) = (error, ex.steps.last())
                    && shown == error
                {
                    ex.steps.pop();
                }
                ex.elapsed = ex.started.map(|s| now.saturating_duration_since(s));
                ex.end = Some(End {
                    cause: *cause,
                    plugin: stopped.as_ref().map(|s| s.plugin.clone()),
                    reason: stopped.as_ref().map(|s| s.reason.clone()).or_else(|| error.clone()),
                });
            }
            Event::ApprovalRequest {
                id,
                call_id,
                tool,
                args,
                reason,
                cwd,
                rules,
            } => {
                let approval = Step::Approval(Approval {
                    id: id.clone(),
                    call_id: call_id.clone(),
                    tool: tool.clone(),
                    args: args.clone(),
                    reason: reason.clone(),
                    cwd: cwd.clone(),
                    rules: rules.clone(),
                    answer: None,
                });
                // The question comes before the call it is about.
                let steps = &mut self.current().steps;
                let at = steps
                    .iter()
                    .rposition(|s| matches!(s, Step::Tool(t) if t.id == *call_id))
                    .unwrap_or(steps.len());
                steps.insert(at, approval);
                effect.approval = Some(id.clone());
            }
            Event::ApprovalCancelled { id } => self.answer_approval(id, Answer::Cancelled),
            // A call stopped just as it ended: nothing went wrong.
            Event::ControlError { error } if error.starts_with("no running tool call") => {}
            Event::ControlError { error } => self.note(Note::Error(format!("control: {error}"))),
            Event::UiRequest { id, ask, title } => {
                let (chosen, typed) = match ask {
                    UiAsk::SelectMany { selected, .. } => (selected.iter().cloned().collect(), String::new()),
                    UiAsk::Input { initial, .. } => (Default::default(), initial.clone().unwrap_or_default()),
                    _ => Default::default(),
                };
                self.asks.push(Ask {
                    id: id.clone(),
                    title: title.clone(),
                    ask: ask.clone(),
                    chosen,
                    typed,
                });
                effect.asked = Some(id.clone());
            }
            Event::UiCancelled { id } => self.asks.retain(|a| a.id != *id),
            Event::PluginNotify { level, text } => {
                if *level != NoticeLevel::Info {
                    self.note(Note::Notice(text.clone()));
                }
                effect.notice = Some((*level, text.clone()));
            }
            Event::PluginText { title, text, .. } => self.note(Note::Text {
                title: title.clone(),
                text: text.clone(),
            }),
            Event::PluginStatus { key, text } => match text {
                Some(text) => {
                    self.status.insert(key.clone(), text.clone());
                }
                None => {
                    self.status.remove(key);
                }
            },
            Event::Commands { commands } => effect.commands = Some(commands.clone()),
            Event::Judged {
                call_id,
                verdict,
                reason,
                ..
            } => {
                if let Some(tool) = self.tool_mut(call_id) {
                    tool.judged = Some((verdict == "allow", reason.clone()));
                }
            }
            Event::TurnStart { .. } | Event::TurnEnd { .. } | Event::Unknown(_) => {}
        }
        effect
    }

    /// A conversation read back from its session log.
    pub fn from_entries(entries: &[SessionEntry]) -> Self {
        let mut t = Transcript {
            replay: true,
            ..Transcript::default()
        };
        for (k, e) in entries.iter().enumerate() {
            match e {
                SessionEntry::Message { message } => {
                    t.at_entry = Some(k);
                    t.message(message, None, true);
                    t.at_entry = None;
                }
                SessionEntry::Replace { messages } => {
                    // The conversation was rewritten. What its messages used is already counted.
                    t.exchanges.clear();
                    for m in messages {
                        t.message(m, None, false);
                    }
                }
                SessionEntry::ToolRepair { call_id, note } => {
                    if let Some(tool) = t.tool_mut(call_id) {
                        tool.repaired = Some(note.clone());
                    } else {
                        t.repairs.insert(call_id.clone(), note.clone());
                    }
                }
                SessionEntry::Usage { usage, .. } => {
                    t.usage.add(usage);
                    t.requests += 1;
                }
                SessionEntry::Model { model } => t.model = Some(model.clone()),
                SessionEntry::Session { .. } | SessionEntry::PermissionMode { .. } | SessionEntry::Other => {}
            }
        }
        t.replay = false;
        t
    }

    fn current(&mut self) -> &mut Exchange {
        if self.exchanges.is_empty() {
            self.exchanges.push(Exchange::default());
        }
        self.exchanges.last_mut().unwrap()
    }

    /// Nothing more comes for calls that wait or run, and for approvals nobody answered.
    fn settle(&mut self, now: Instant) {
        for step in self.exchanges.iter_mut().flat_map(|e| &mut e.steps) {
            match step {
                Step::Tool(t) if t.state == ToolState::Waiting => t.state = ToolState::Skipped,
                Step::Tool(t) if t.state == ToolState::Running => {
                    t.state = ToolState::Failed;
                    t.elapsed = t.started.map(|s| now.saturating_duration_since(s));
                }
                Step::Approval(a) if a.answer.is_none() => a.answer = Some(Answer::Cancelled),
                _ => {}
            }
        }
    }

    fn stream(&mut self, delta: &StreamDelta) {
        let (index, delta, thinking) = match delta {
            StreamDelta::TextDelta { index, delta } => (*index, delta, false),
            StreamDelta::ThinkingDelta { index, delta } => (*index, delta, true),
            _ => return,
        };
        let known = self.streaming.get(&index).copied();
        let steps = &mut self.current().steps;
        match known.and_then(|i| steps.get_mut(i)) {
            Some(Step::Text(md)) => md.push(delta),
            Some(Step::Thinking(t)) => t.push_str(delta),
            _ => {
                steps.push(if thinking {
                    Step::Thinking(delta.clone())
                } else {
                    Step::Text(Box::new(Markdown::new(delta)))
                });
                let at = steps.len() - 1;
                self.streaming.insert(index, at);
            }
        }
    }

    fn message(&mut self, message: &Message, now: Option<Instant>, count: bool) {
        match message {
            Message::User { content, timestamp } => self.user(content, now, *timestamp),
            Message::Assistant(a) => {
                if count {
                    self.usage.add(&a.usage);
                    self.requests += 1;
                }
                self.last_input = a.usage.input + a.usage.cache_read + a.usage.cache_write;
                if !a.model.is_empty() {
                    self.model = Some(if a.provider.is_empty() {
                        a.model.clone()
                    } else {
                        format!("{}/{}", a.provider, a.model)
                    });
                }
                // A reply that did not stream (or streamed only in part) is shown from the whole message.
                for (index, c) in a.content.iter().enumerate() {
                    if self.streaming.contains_key(&index) {
                        continue;
                    }
                    match c {
                        Content::Text { text } if !text.trim().is_empty() => {
                            self.current().steps.push(Step::Text(Box::new(Markdown::new(text))))
                        }
                        Content::Thinking { thinking, .. } if !thinking.trim().is_empty() => {
                            self.current().steps.push(Step::Thinking(thinking.clone()))
                        }
                        Content::ToolCall(call) => {
                            self.tool_or_new(call);
                        }
                        _ => {}
                    }
                }
                self.streaming.clear();
                if let Some(e) = &a.error_message {
                    self.note(Note::Error(e.clone()));
                }
            }
            Message::Tool(r) => {
                let replay = self.replay;
                let output = text_of(&r.content);
                match self.tool_mut(&r.tool_call_id) {
                    // In a log this message is all there is of the call's run, or of why it had none.
                    Some(tool) if replay => {
                        tool.set_output(&output);
                        tool.images = pictures(&r.content);
                        tool.state = match r.is_error {
                            true if NOT_RUN.iter().any(|said| output.starts_with(said)) => ToolState::Skipped,
                            true => ToolState::Failed,
                            false => ToolState::Done,
                        };
                    }
                    // It was never started: denied, refused or interrupted. The message says which.
                    Some(tool) if tool.state == ToolState::Waiting => {
                        tool.set_output(&output);
                        tool.state = ToolState::Skipped;
                        self.infer_answer(&r.tool_call_id, Answer::Denied(String::new()));
                    }
                    Some(_) => {}
                    None => {
                        let mut tool = Tool::new(&ToolCall {
                            id: r.tool_call_id.clone(),
                            name: r.tool_name.clone(),
                            input: Value::Null,
                            raw_input: None,
                        });
                        tool.set_output(&output);
                        tool.state = if r.is_error {
                            ToolState::Skipped
                        } else {
                            ToolState::Done
                        };
                        self.current().steps.push(Step::Tool(Box::new(tool)));
                    }
                }
            }
            Message::Other => {}
        }
    }

    /// `timestamp`: milliseconds since 1970, as the agent stamps a message; 0 when it has none.
    fn user(&mut self, content: &[Content], now: Option<Instant>, timestamp: f64) {
        if let Some(note) = note_text(content) {
            return self.note(Note::Harness(note));
        }
        let mut text = said(content);
        // What compaction left in place of the earlier messages.
        if let Some(summary) = text.strip_prefix(SUMMARY) {
            return self.note(Note::Compacted(Some(summary.trim().to_owned())));
        }
        if let Some((head, report)) = task_report(&text) {
            return self.note(Note::Task {
                head: head.to_owned(),
                report: report.to_owned(),
            });
        }
        if let Some(pos) = self.queued.iter().position(|q| sent_as(q, &text)) {
            text = self.queued.remove(pos);
        } else if let Some(pos) = self.echoes.iter().position(|e| sent_as(e, &text)) {
            self.echoes.remove(pos);
            return;
        }
        if let (Some(before), Some(now)) = (self.exchanges.last_mut(), now)
            && before.elapsed.is_none()
        {
            before.elapsed = before.started.map(|s| now.saturating_duration_since(s));
        }
        let at = (timestamp.is_finite() && timestamp > 0.0)
            .then(|| SystemTime::UNIX_EPOCH + Duration::from_millis(timestamp as u64));
        self.exchanges.push(Exchange {
            prompt: text,
            started: now,
            at,
            entry: self.at_entry,
            ..Exchange::default()
        });
    }

    /// The row of a call: the one the reply asked for, or a new one (a call a tool made itself).
    fn tool_or_new(&mut self, call: &ToolCall) -> &mut Tool {
        let found = self.exchanges.iter().rev().find_map(|e| {
            e.steps
                .iter()
                .rposition(|s| matches!(s, Step::Tool(t) if t.id == call.id))
        });
        if found.is_none() {
            let mut tool = Tool::new(call);
            tool.repaired = self.repairs.remove(&call.id);
            if tool.kind() == Kind::Plan
                && let Some(todos) = call.input.get("todos").and_then(todos_of)
            {
                let before = self.plan();
                tool.plan = Some(PlanUpdate {
                    first: before.is_none(),
                    newly_done: todos
                        .iter()
                        .filter(|t| t.status == TodoStatus::Completed)
                        .filter(|t| {
                            !before.is_some_and(|b| {
                                b.iter()
                                    .any(|o| o.content == t.content && o.status == TodoStatus::Completed)
                            })
                        })
                        .map(|t| t.content.clone())
                        .collect(),
                    todos,
                });
            }
            self.current().steps.push(Step::Tool(Box::new(tool)));
        }
        self.tool_mut(&call.id).expect("the row was just found or made")
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut Tool> {
        self.exchanges
            .iter_mut()
            .rev()
            .flat_map(|e| e.steps.iter_mut().rev())
            .find_map(|s| match s {
                Step::Tool(t) if t.id == id => Some(&mut **t),
                _ => None,
            })
    }
}

/// Whether `arrived`, a message of the user's as the model was given it, is `sent`. The image
/// plugin puts a label like `[Image 1: /work/shot.png]` where the message named an image it
/// attached (`@shot.png`, or a path dropped in), so what is around the labels has to be the same and
/// in the same order, and what stood in their place is not compared.
pub(crate) fn sent_as(sent: &str, arrived: &str) -> bool {
    if sent == arrived {
        return true;
    }
    let mut around = Vec::new();
    let (mut part, mut from) = (0, 0);
    while let Some(found) = arrived[from..].find("[Image ") {
        let start = from + found;
        match label_len(&arrived[start..]) {
            Some(len) => {
                around.push(&arrived[part..start]);
                part = start + len;
                from = part;
            }
            None => from = start + 1,
        }
    }
    if around.is_empty() {
        return false;
    }
    around.push(&arrived[part..]);
    let labels = around.len() - 1;
    let (first, last) = (around[0], around[labels]);
    let Some(mut left) = sent.strip_prefix(first) else {
        return false;
    };
    for part in &around[1..labels] {
        match left.find(part) {
            Some(at) => left = &left[at + part.len()..],
            None => return false,
        }
    }
    left.ends_with(last)
}

/// `text`, a message of the user's as its log holds it, as it was typed: each label the image plugin
/// put where it attached an image from a file goes back to `@path`, relative to `cwd` inside it. So
/// it reads as it did when it was sent, and sent again, the plugin attaches the image again. A label
/// that names no file (an image pasted into the terminal) is left as it is.
pub(crate) fn as_typed(text: &str, cwd: &Path) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(found) = rest.find("[Image ") {
        out.push_str(&rest[..found]);
        let at = &rest[found..];
        let file = label_len(at).and_then(|len| {
            let name = &at[at.find(": ")? + 2..len - 1];
            Path::new(name).is_absolute().then_some((name, len))
        });
        match file {
            Some((name, len)) => {
                out.push_str(&crate::composer::path_ref(Path::new(name), cwd));
                rest = &at[len..];
                // A word of its own, or the plugin reads what follows as part of the path.
                if rest.starts_with(|c: char| !c.is_whitespace()) {
                    out.push(' ');
                }
            }
            None => {
                out.push_str("[Image ");
                rest = &at["[Image ".len()..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The length of the image plugin's label at the start of `text`, as in `[Image 1: /work/shot.png]`.
fn label_len(text: &str) -> Option<usize> {
    let after = text.strip_prefix("[Image ")?;
    let name = after.trim_start_matches(|c: char| c.is_ascii_digit());
    if name.len() == after.len() {
        return None;
    }
    let name = name.strip_prefix(": ")?;
    let end = name.find([']', '\n'])?;
    name[end..].starts_with(']').then(|| text.len() - name.len() + end + 1)
}

/// What the user wrote in a message: its text without the agent's mark of an interruption. A
/// message that waited while the run was stopped arrives behind that mark, and is still the one
/// that waited.
fn said(content: &[Content]) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } if !text.starts_with(INTERRUPTED) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The text of a harness note, when every part of the message is one (`noteText`).
fn note_text(content: &[Content]) -> Option<String> {
    let mut parts = Vec::new();
    for c in content {
        match c {
            Content::Text { text } => parts.push(text.strip_prefix(NOTE_PREFIX)?),
            _ => return None,
        }
    }
    if parts.is_empty() { None } else { Some(parts.join(" ")) }
}

/// `[task <id> "<description>" ended: <status>]`, a blank line, then the report
/// (`crates/sasacode-bundled/src/subagent.rs`).
fn task_report(text: &str) -> Option<(&str, &str)> {
    let (head, report) = text.split_once("\n\n").unwrap_or((text, ""));
    let head = head.strip_prefix("[task ")?.strip_suffix(']')?;
    head.contains("\" ended: ").then_some((head, report))
}

fn todos_of(v: &Value) -> Option<Vec<Todo>> {
    let list = v.as_array()?;
    Some(
        list.iter()
            .filter_map(|t| {
                Some(Todo {
                    content: t.get("content")?.as_str()?.to_owned(),
                    status: match t.get("status").and_then(Value::as_str) {
                        Some("completed") => TodoStatus::Completed,
                        Some("in_progress") => TodoStatus::InProgress,
                        _ => TodoStatus::Pending,
                    },
                })
            })
            .collect(),
    )
}

/// A one-line description of a call: the argument that says the most, else its JSON.
pub fn summarize(call: &ToolCall) -> String {
    for key in ["command", "path", "file_path", "description", "pattern", "url", "query"] {
        if let Some(v) = call.input.get(key).and_then(|v| v.as_str()) {
            return first_line(v);
        }
    }
    match &call.input {
        Value::Null => String::new(),
        Value::Object(o) if o.is_empty() => String::new(),
        other => first_line(&other.to_string()),
    }
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("");
    let short: String = line.chars().take(160).collect();
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
    use crate::diff::Sign;
    use sasacode_protocol::parse_event;

    fn fixture(name: &str) -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../sasacode-protocol/tests/fixtures")
            .join(name);
        std::fs::read_to_string(p).unwrap()
    }

    /// The run of a fixture, with its prompt sent the way the window sends it.
    fn run(name: &str, prompt: &str) -> (Transcript, Vec<Effect>) {
        let mut t = Transcript::default();
        let now = Instant::now();
        t.send(prompt, now);
        let effects = fixture(name)
            .lines()
            .map(|line| t.apply(&parse_event(line).unwrap(), now))
            .collect();
        (t, effects)
    }

    fn log(name: &str) -> Transcript {
        let entries: Vec<SessionEntry> = fixture(name)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        Transcript::from_entries(&entries)
    }

    fn line(step: &Step) -> String {
        match step {
            Step::Thinking(s) => format!("thinking:{s}"),
            Step::Text(m) => format!("text:{}", m.raw),
            Step::Tool(t) => format!("tool:{}:{}:{:?}", t.name, t.summary, t.state),
            Step::Approval(a) => format!("approval:{}:{:?}", a.tool, a.answer),
            Step::Note(n) => format!("note:{n:?}"),
        }
    }

    fn lines(e: &Exchange) -> Vec<String> {
        e.steps.iter().map(line).collect()
    }

    #[test]
    fn a_picture_a_tool_read_is_kept_to_be_drawn() {
        let log = [
            r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"画像を見て"}],"timestamp":1}}"#,
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"tool_call","id":"c1","name":"read","input":{"path":"shot.png"}}],"api":"openai-chat","provider":"fake","model":"m","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"cost":0},"stopReason":"tool_use","timestamp":2}}"#,
            r#"{"type":"message","message":{"role":"tool","toolCallId":"c1","toolName":"read","content":[{"type":"text","text":"Read image shot.png"},{"type":"image","mediaType":"image/png","data":"iVBORw0KGgo="}],"isError":false,"timestamp":3}}"#,
        ];
        let entries: Vec<SessionEntry> = log.iter().map(|l| serde_json::from_str(l).unwrap()).collect();
        let t = Transcript::from_entries(&entries);
        let tool = t.exchanges[0].tools().next().unwrap();
        assert_eq!(tool.images.len(), 1);
        assert_eq!(std::fs::read(&tool.images[0]).unwrap(), b"\x89PNG\r\n\x1a\n");
        assert_eq!(tool.output, "Read image shot.png");
    }

    #[test]
    fn a_real_run_becomes_the_conversation_once_with_the_sent_prompt_not_repeated() {
        let (t, effects) = run("run.jsonl", "ファイルを作って");
        assert_eq!(t.exchanges.len(), 1);
        assert_eq!(t.exchanges[0].prompt, "ファイルを作って");
        assert_eq!(
            lines(&t.exchanges[0]),
            [
                "thinking:まずファイルを作る。",
                "text:ファイルを作ります。",
                "approval:bash:Some(Allowed)",
                "tool:bash:echo hi > made.txt:Done",
                "note:Error(\"control: a control message is a JSON object\")",
                "text:**完了**しました。`made.txt` を作成しています。",
            ]
        );
        assert_eq!(t.exchanges[0].answer(), Some(5));
        assert_eq!(t.exchanges[0].end.as_ref().map(|e| e.cause), Some(StopCause::Done));
        assert_eq!(t.model.as_deref(), Some("fake/m"));
        assert!(effects[0].session_id.is_some());
        assert!(effects.iter().any(|e| e.approval.as_deref() == Some("approval-1")));
        assert_eq!(effects.last().unwrap().ended, Some(StopCause::Done));
    }

    #[test]
    fn a_working_run_keeps_what_each_call_did() {
        let (t, _) = run("work.jsonl", "再試行を指数バックオフにして");
        // The message sent mid-run starts its own exchange where the model was given it.
        let prompts: Vec<&str> = t.exchanges.iter().map(|e| e.prompt.as_str()).collect();
        assert_eq!(prompts, ["再試行を指数バックオフにして", "テストも足して"]);
        assert_eq!(t.title(), Some("再試行を指数バックオフにして"));
        assert_eq!(
            lines(&t.exchanges[0]),
            [
                "thinking:まず現在の実装を読む。",
                "text:現在の再試行処理を確認します。",
                "tool:read:src/client.ts:Done",
                "approval:bash:Some(Allowed)",
                "tool:bash:grep -n retry src/client.ts README.md:Done",
            ]
        );
        let second = &t.exchanges[1];
        let tools: Vec<String> = second.tools().map(|t| format!("{}:{:?}", t.name, t.state)).collect();
        assert_eq!(
            tools,
            [
                "read:Done",
                "todo_write:Done",
                "write:Done",
                "edit:Done",
                "todo_write:Done",
                "bash:Done",
                "bash:Skipped",
                "bash:Failed",
                "bash:Failed",
                "task:Done",
                "todo_write:Done",
            ]
        );
        assert_eq!(second.failed(), 2);
        assert!(matches!(second.steps.last(), Some(Step::Text(m)) if m.raw.starts_with("**完了**")));
        assert_eq!(second.end.as_ref().map(|e| e.cause), Some(StopCause::Done));

        // The call tool-repair mended says so, under the name it ran as.
        let repaired = second.tools().next().unwrap();
        assert_eq!(repaired.summary, "README.md");
        assert_eq!(
            repaired.repaired.as_deref(),
            Some(r#"tool "read_file" → "read"; argument "file" → "path""#)
        );

        // write: a new file, every line added. edit: the diff the tool reported, with its hunk.
        let changes: Vec<_> = t.changes().collect();
        assert_eq!(changes.len(), 2);
        let (_, write) = changes[0];
        assert_eq!((write.path.as_str(), write.created), ("src/retry.ts", Some(true)));
        assert_eq!((write.diff.added, write.diff.removed), (3, 0));
        let (_, edit) = changes[1];
        assert_eq!((edit.path.as_str(), edit.created), ("src/client.ts", Some(false)));
        assert_eq!((edit.diff.added, edit.diff.removed), (1, 1));
        assert_eq!(edit.diff.lines[0].sign, Sign::Hunk);
        assert!(edit.diff.lines.iter().any(|l| l.sign == Sign::Context));

        // The denied call never ran: its row holds what the model was told.
        let denied = second.tools().find(|t| t.state == ToolState::Skipped).unwrap();
        assert_eq!(denied.summary, "echo two");
        assert_eq!(
            denied.output,
            "The user denied this tool call. User feedback: それは不要"
        );

        // bash: the exit code and the output; the denied one is not a run.
        let commands: Vec<(&str, Option<i64>)> = t.commands().map(|c| (c.summary.as_str(), c.exit_code)).collect();
        assert_eq!(
            commands,
            [
                ("grep -n retry src/client.ts README.md", Some(0)),
                ("echo one", Some(0)),
                ("sh -c 'echo boom; exit 1'", Some(1)),
                ("sh -c 'echo boom; exit 1'", Some(1)),
            ]
        );
        let failing = t.commands().nth(2).unwrap();
        assert_eq!((failing.output.as_str(), failing.lines), ("boom\n[exit code 1]", 2));

        // The subagent's calls are the lines it wrote while it ran; its report is the result.
        let task = second.tools().find(|t| t.kind() == Kind::Task).unwrap();
        assert_eq!(task.stream, "read(src/retry.ts)\n");
        assert_eq!(task.output, "`src/`には`client.ts`と`retry.ts`があります。");

        // The plan: made once, then ticked off.
        let plans: Vec<&PlanUpdate> = second.tools().filter_map(|t| t.plan.as_ref()).collect();
        assert_eq!(plans.len(), 3);
        assert!(plans[0].first && plans[0].newly_done.is_empty());
        assert_eq!(plans[1].newly_done, ["遅延の計算を切り出す", "client.tsを繋ぎ替える"]);
        assert_eq!(plans[2].newly_done, ["動作を確認する"]);
        assert_eq!((plans[2].done(), plans[2].todos.len()), (3, 3));
        assert!(t.plan().unwrap().iter().all(|t| t.status == TodoStatus::Completed));

        // The harness's reminder is a note, not something the user said.
        let notes: Vec<&Note> = second
            .steps
            .iter()
            .filter_map(|s| match s {
                Step::Note(n) => Some(n),
                _ => None,
            })
            .collect();
        assert!(matches!(notes[..], [Note::Harness(text)] if text.starts_with("Your last 5 replies")));

        // 14 replies of the conversation and the subagent's 2.
        assert_eq!(t.requests, 14);
        assert_eq!((t.usage.input, t.usage.output), (1400, 280));
        assert_eq!(t.last_input, 100);

        // Nobody answered here: what happened to each call says how its approval went.
        let answers: Vec<_> = t.approvals().map(|a| a.answer.clone().unwrap()).collect();
        assert_eq!(
            answers,
            [
                Answer::Allowed,
                Answer::Allowed,
                Answer::Denied(String::new()),
                Answer::Allowed
            ]
        );
    }

    #[test]
    fn approvals_are_answered_oldest_first_and_an_answer_stands() {
        let mut t = Transcript::default();
        let now = Instant::now();
        for (id, command) in [("approval-1", "echo one"), ("approval-2", "echo two")] {
            let line = format!(
                r#"{{"type":"approval_request","id":"{id}","callId":"call-{id}","tool":"bash","args":{{"command":"{command}"}},"reason":"mode edits","cwd":"/work/project"}}"#
            );
            assert_eq!(t.apply(&parse_event(&line).unwrap(), now).approval.as_deref(), Some(id));
        }
        assert_eq!(t.pending_approvals(), 2);
        assert_eq!(t.pending_approval().unwrap().id, "approval-1");
        t.answer_approval("approval-1", Answer::Always);
        t.answer_approval("approval-1", Answer::Cancelled);
        assert_eq!(t.approvals().next().unwrap().answer, Some(Answer::Always));
        let next = t.pending_approval().unwrap();
        assert_eq!(
            (next.id.as_str(), next.args["command"].as_str()),
            ("approval-2", Some("echo two"))
        );
        assert_eq!(next.cwd, "/work/project");
        t.close(now);
        assert_eq!(t.pending_approvals(), 0);
    }

    #[test]
    fn a_message_sent_mid_run_waits_until_the_model_is_given_it() {
        let mut t = Transcript::default();
        let now = Instant::now();
        t.send("再試行を指数バックオフにして", now);
        let mut seen = false;
        for l in fixture("work.jsonl").lines() {
            let event = parse_event(l).unwrap();
            if matches!(&event, Event::ToolStart { call, .. } if call.name == "bash") && !seen {
                seen = true;
                t.queue("テストも足して");
                assert_eq!(t.queued, ["テストも足して"]);
            }
            t.apply(&event, now);
        }
        assert!(t.queued.is_empty());
        assert_eq!(t.exchanges.len(), 2);
        assert_eq!(t.exchanges[1].prompt, "テストも足して");
    }

    /// A run stopped while a message waited gives the model that message behind a mark of its own.
    /// It is the message that waited: shown as it was typed, and not sent a second time.
    #[test]
    fn a_message_that_waited_through_an_interruption_is_given_once() {
        let mut t = Transcript::default();
        let now = Instant::now();
        t.send("しばらく待って", now);
        t.queue("やっぱり10秒でいい");
        let given = format!(
            r#"{{"type":"message_end","message":{{"role":"user","content":[{{"type":"text","text":"{INTERRUPTED}"}},{{"type":"text","text":"やっぱり10秒でいい"}}],"timestamp":1}}}}"#
        );
        t.apply(&parse_event(&given).unwrap(), now);
        assert!(t.queued.is_empty());
        assert_eq!(t.exchanges.len(), 2);
        assert_eq!(t.exchanges[1].prompt, "やっぱり10秒でいい");
    }

    /// The image plugin puts a label where the message named an image. It is still the message that
    /// was sent: shown once, as it was typed.
    #[test]
    fn a_message_with_an_image_attached_is_shown_once_as_it_was_typed() {
        let mut t = Transcript::default();
        let now = Instant::now();
        let given = |text: &str| {
            let message = serde_json::json!({"type": "message_end", "message": {"role": "user", "timestamp": 1,
                "content": [{"type": "text", "text": text}, {"type": "image", "mediaType": "image/png", "data": "AA=="}]}});
            parse_event(&message.to_string()).unwrap()
        };
        t.send("@/work/shot.png この画面は何？", now);
        t.apply(&given("[Image 1: /work/shot.png] この画面は何？"), now);
        assert_eq!(t.exchanges.len(), 1);
        assert_eq!(t.exchanges[0].prompt, "@/work/shot.png この画面は何？");

        t.queue("@a.png と @b.png を比べて");
        t.apply(&given("[Image 1: /work/a.png] と [Image 2: /work/b.png] を比べて"), now);
        assert!(t.queued.is_empty());
        assert_eq!(t.exchanges.len(), 2);
        assert_eq!(t.exchanges[1].prompt, "@a.png と @b.png を比べて");
    }

    #[test]
    fn a_message_read_from_its_log_names_its_images_as_they_were_typed() {
        // The plugin writes the absolute path as the system writes it: `C:\…` on Windows.
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        let cwd = Path::new(root).join("work").join("p");
        let other = Path::new(root).join("else").join("b.png");
        let label = |n: usize, path: &Path| format!("[Image {n}: {}]", path.display());
        let cwd = cwd.as_path();
        assert_eq!(
            as_typed(&format!("{} これを見て", label(1, &cwd.join("shot.png"))), cwd),
            "@shot.png これを見て"
        );
        assert_eq!(
            as_typed(
                &format!("{}{}と比べて", label(1, &cwd.join("a.png")), label(2, &other)),
                cwd
            ),
            format!("@a.png @{} と比べて", other.display())
        );
        assert_eq!(as_typed(&label(1, &cwd.join("my shot.png")), cwd), "@\"my shot.png\"");
        // Not a file's label, or not a label: left as it is.
        assert_eq!(as_typed("[Image 1: image] 貼った", cwd), "[Image 1: image] 貼った");
        assert_eq!(as_typed("[Image のこと", cwd), "[Image のこと");
        assert_eq!(as_typed("ふつうの文", cwd), "ふつうの文");
    }

    #[test]
    fn only_the_labels_of_images_are_passed_over_when_a_message_is_matched() {
        assert!(sent_as("@a.png 見て", "[Image 1: /w/a.png] 見て"));
        assert!(sent_as(
            "前 @a.png 中 @b.png 後",
            "前 [Image 1: /w/a.png] 中 [Image 2: /w/b.png] 後"
        ));
        assert!(sent_as("[Image x] そのまま", "[Image x] そのまま"));
        assert!(!sent_as("@a.png 見て", "[Image 1: /w/a.png] 見ないで"));
        assert!(!sent_as("前 @a.png 後", "後 [Image 1: /w/a.png] 前"));
        assert!(!sent_as("[Image 1 見て", "[Image 1: /w/a.png 見て"));
        assert!(!sent_as("別の文", "[Image : /w/a.png] 別の文"));
    }

    #[test]
    fn a_session_log_reads_back_as_the_same_conversation() {
        let t = log("session.jsonl");
        assert_eq!(t.exchanges.len(), 1);
        assert_eq!(
            lines(&t.exchanges[0]),
            [
                "thinking:まずファイルを作る。",
                "text:ファイルを作ります。",
                "tool:bash:echo hi > made.txt:Done",
                "text:**完了**しました。`made.txt` を作成しています。",
            ]
        );
        assert!(t.exchanges[0].end.is_none() && t.exchanges[0].elapsed.is_none());
    }

    #[test]
    fn a_log_rebuilds_changes_from_the_calls_and_keeps_repairs_and_outside_usage() {
        let t = log("work-session.jsonl");
        let prompts: Vec<&str> = t.exchanges.iter().map(|e| e.prompt.as_str()).collect();
        assert_eq!(prompts, ["再試行を指数バックオフにして", "テストも足して"]);
        let changes: Vec<_> = t.changes().collect();
        assert_eq!(changes.len(), 2);
        // No hunk header and no lines around the change: the log has only the call.
        let (_, edit) = changes[1];
        assert_eq!((edit.diff.added, edit.diff.removed), (1, 1));
        assert!(edit.diff.lines.iter().all(|l| matches!(l.sign, Sign::Add | Sign::Del)));
        let repaired = t.exchanges[1].tools().next().unwrap();
        assert!(repaired.repaired.is_some());
        assert_eq!(t.requests, 14);
        assert_eq!(t.pending_approvals(), 0);
        assert!(t.plan().unwrap().iter().all(|t| t.status == TodoStatus::Completed));
        // A log has no approvals; the answer written in the call's place tells a denied call from
        // one that failed, and a write does not say whether the file was new.
        assert_eq!(t.exchanges[1].failed(), 2);
        let denied: Vec<_> = t.tools().filter(|t| t.denied()).map(|t| t.summary.as_str()).collect();
        assert_eq!(denied, ["echo two"]);
        assert_eq!(changes[0].1.created, None);
    }

    #[test]
    fn an_interrupt_ends_the_run_and_marks_what_was_cut_short() {
        let (t, effects) = run("stop.jsonl", "しばらく待って");
        let bash = t.tools().next().unwrap();
        assert_eq!(
            (bash.state, bash.exit_code, bash.killed.as_deref()),
            (ToolState::Failed, None, Some("abort"))
        );
        assert_eq!(effects.last().unwrap().ended, Some(StopCause::Aborted));
        assert_eq!(t.exchanges[0].end.as_ref().unwrap().cause, StopCause::Aborted);

        // Interrupted while an approval waited: the approval is cancelled and the call never ran.
        let (t, _) = run("cancel.jsonl", "ファイルを消して");
        assert_eq!(
            lines(&t.exchanges[0]),
            ["approval:bash:Some(Cancelled)", "tool:bash:rm -f made.txt:Skipped"]
        );
        assert_eq!(t.pending_approvals(), 0);
        assert_eq!(
            t.tools().next().unwrap().output,
            "Interrupted by the user before this tool ran."
        );
    }

    #[test]
    fn an_error_is_told_once() {
        let (t, _) = run("error.jsonl", "こんにちは");
        assert!(t.exchanges[0].steps.is_empty(), "{:?}", lines(&t.exchanges[0]));
        assert_eq!(
            t.exchanges[0].end,
            Some(End {
                cause: StopCause::Error,
                plugin: None,
                reason: Some("500 fake failure 500".into())
            })
        );
    }

    #[test]
    fn a_dropped_reply_is_replaced_by_the_one_asked_again() {
        let (t, _) = run("dropped.jsonl", "こんにちは");
        assert_eq!(lines(&t.exchanges[0]), ["note:Retrying", "text:**やり直し**ました。"]);
        // The reply that was thrown away was paid for too.
        assert_eq!(t.requests, 2);
    }

    #[test]
    fn the_turn_limit_and_a_plugin_stop_say_why_the_run_ended() {
        let (t, _) = run("turns.jsonl", "読んで");
        assert_eq!(t.exchanges[0].end.as_ref().unwrap().cause, StopCause::MaxTurns);

        let (t, _) = run("loop.jsonl", "読んで");
        assert_eq!(
            t.exchanges[0].end,
            Some(End {
                cause: StopCause::Stopped,
                plugin: Some("loop-guard".into()),
                reason: Some("5 turns in a row in which no tool call could run".into())
            })
        );
        // The calls loop-guard refused never ran.
        let refused = t.tools().filter(|t| t.state == ToolState::Skipped).count();
        assert!(refused >= 5, "{refused}");
        assert!(t.tools().all(|t| t.finished()));
    }

    #[test]
    fn a_process_that_dies_leaves_nothing_waiting() {
        let mut t = Transcript::default();
        let now = Instant::now();
        t.send("しばらく待って", now);
        for l in fixture("stop.jsonl").lines() {
            let event = parse_event(l).unwrap();
            if matches!(event, Event::ToolEnd { .. }) {
                break;
            }
            t.apply(&event, now);
        }
        assert_eq!(t.tools().next().unwrap().state, ToolState::Running);
        t.close(now);
        assert_eq!(t.tools().next().unwrap().state, ToolState::Failed);
        assert_eq!(t.pending_approvals(), 0);
    }

    #[test]
    fn harness_notes_and_task_reports_are_not_the_users_words() {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/sasacode-host/src/notes.rs"),
        )
        .unwrap();
        assert!(source.contains(&format!("NOTE_PREFIX: &str = \"{NOTE_PREFIX}\"")));
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/sasacode-agent/src/tool_exec.rs"),
        )
        .unwrap();
        for said in NOT_RUN {
            assert!(source.contains(said), "{said}");
        }
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/sasacode-agent/src/agent.rs"),
        )
        .unwrap();
        assert!(source.contains(&format!("INTERRUPTED_NOTE: &str = \"{INTERRUPTED}\"")));

        let text = |s: &str| vec![Content::Text { text: s.to_owned() }];
        assert_eq!(
            note_text(&text(&format!("{NOTE_PREFIX}keep going"))).as_deref(),
            Some("keep going")
        );
        assert_eq!(note_text(&text("keep going")), None);
        assert_eq!(note_text(&[]), None);
        assert_eq!(
            task_report("[task t1 \"構成を調べる\" ended: done]\n\n報告です"),
            Some(("t1 \"構成を調べる\" ended: done", "報告です"))
        );
        assert_eq!(task_report("[task list]"), None);
    }

    #[test]
    fn long_tool_output_keeps_its_end() {
        let long = "あ".repeat(MAX_TOOL_OUTPUT);
        let cut = tail(&long);
        assert!(cut.starts_with('…') && cut.len() <= MAX_TOOL_OUTPUT + '…'.len_utf8());
    }

    #[test]
    fn a_compactions_summary_is_a_note_to_open_not_a_message_from_the_user() {
        let entry = |line: &str| -> SessionEntry { serde_json::from_str(line).unwrap() };
        let summary = format!("{SUMMARY}\n\nThe user asked for retries; done in src/net.rs.");
        let replace = entry(
            &serde_json::json!({"type": "replace", "messages": [
                {"role": "user", "timestamp": 1, "content": [{"type": "text", "text": summary}]},
                {"role": "user", "timestamp": 2, "content": [{"type": "text", "text": "次はテスト"}]}
            ]})
            .to_string(),
        );
        let t = Transcript::from_entries(std::slice::from_ref(&replace));
        assert_eq!(t.exchanges.len(), 2);
        assert!(t.exchanges[0].prompt.is_empty());
        assert!(matches!(&t.exchanges[0].steps[0],
            Step::Note(Note::Compacted(Some(s))) if s == "The user asked for retries; done in src/net.rs."));
        assert_eq!(t.exchanges[1].prompt, "次はテスト");
        // In a run, the summary is read from the log once the run says it compacted.
        let mut live = Transcript::default();
        live.apply(&sasacode_protocol::Event::MessagesReplaced, Instant::now());
        assert!(live.summary_missing());
        live.fill_summary(&[replace]);
        assert!(!live.summary_missing());
    }
}
