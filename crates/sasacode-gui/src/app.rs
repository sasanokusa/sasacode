//! The window's state and what changes it. How it looks is in `view.rs`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{markdown, operation, text_editor};
use iced::{Event, Subscription, Task, Theme, event, task, theme, window};
use sasacode_protocol::{ControlMessage, Decision, SessionSummary, list_sessions, read_entries, restore};
use tokio::sync::mpsc::UnboundedSender;

use crate::i18n::Strings;
use crate::prefs::Prefs;
use crate::runner::{self, RunSpec, RunUpdate};
use crate::style::{Colors, DARK, LIGHT};
use crate::transcript::{Item, Transcript};

pub const PERMISSIONS: [&str; 4] = ["edits", "ask", "agent", "auto"];
pub const EFFORTS: [&str; 6] = ["off", "low", "medium", "high", "xhigh", "max"];
pub const EDITOR: &str = "editor";
pub const MODEL_ENTRY: &str = "model-entry";
/// How long "Copied" stays under the input.
const FLASH: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct Options {
    pub program: PathBuf,
    pub cwd: PathBuf,
}

/// One choice of a setting: `key` is what the CLI takes, `label` what the window shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub key: String,
    pub label: String,
}

impl Pick {
    /// The model picker's last row: type a name that is not in the list.
    pub const OTHER: &str = "\u{0}other";

    pub fn new(key: &str, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
        }
    }
}

impl std::fmt::Display for Pick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

pub struct App {
    pub(crate) s: Strings,
    pub(crate) c: Colors,
    theme: Theme,
    /// `SASACODE_THEME=light|dark` instead of following the system.
    fixed_theme: bool,
    program: PathBuf,
    pub(crate) cwd: PathBuf,
    home: Option<PathBuf>,
    pub(crate) sessions: Vec<SessionSummary>,
    /// The session the next prompt continues; `None` starts a new one.
    pub(crate) session: Option<String>,
    pub(crate) transcript: Transcript,
    pub(crate) editor: text_editor::Content,
    pub(crate) feedback: String,
    pub(crate) search: String,
    pub(crate) prefs: Prefs,
    /// A model name being typed, while the field for it is shown.
    pub(crate) model_entry: Option<String>,
    /// Steps whose detail is shown, by item.
    pub(crate) open: HashSet<usize>,
    /// Finished turns whose work is shown, by the turn's first item.
    pub(crate) open_work: HashSet<usize>,
    /// Sent during the run and not yet read by the model.
    pub(crate) queued: Vec<String>,
    pub(crate) run: Option<Run>,
    pub(crate) flash: Option<(&'static str, Instant)>,
    pub(crate) now: Instant,
    pub(crate) clock: SystemTime,
}

pub(crate) struct Run {
    control: Option<UnboundedSender<ControlMessage>>,
    _handle: task::Handle,
    stderr: Vec<String>,
    ended: bool,
    pub(crate) started: Instant,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Edit(text_editor::Action),
    Send,
    Stop,
    Run(RunUpdate),
    Answer(Decision),
    Feedback(String),
    NewSession,
    OpenSession(PathBuf),
    Search(String),
    Model(Pick),
    ModelEntry(String),
    ModelCommit,
    Permission(Pick),
    Effort(Pick),
    Toggle(usize),
    ToggleWork(usize),
    Link(markdown::Uri),
    Copy(String),
    ThemeMode(theme::Mode),
    Tick(Instant),
    Key(Shortcut),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    /// Ctrl/⌘+Enter: allow what is asked, else send.
    Confirm,
    /// Ctrl/⌘+N.
    New,
    /// Ctrl/⌘+. stops the run.
    Stop,
    /// Esc leaves the model field.
    Escape,
}

impl App {
    pub fn new(options: Options) -> (Self, Task<Msg>) {
        let home = sasacode_protocol::sasacode_home();
        let prefs = home.as_deref().map(Prefs::load).unwrap_or_default();
        let fixed = match std::env::var("SASACODE_THEME").as_deref() {
            Ok("dark") => Some(DARK),
            Ok("light") => Some(LIGHT),
            _ => None,
        };
        let c = fixed.unwrap_or(LIGHT);
        let mut app = App {
            s: Strings::detect(),
            c,
            theme: c.theme(),
            fixed_theme: fixed.is_some(),
            program: options.program,
            cwd: options.cwd,
            home,
            sessions: Vec::new(),
            session: None,
            transcript: Transcript::default(),
            editor: text_editor::Content::new(),
            feedback: String::new(),
            search: String::new(),
            prefs,
            model_entry: None,
            open: HashSet::new(),
            open_work: HashSet::new(),
            queued: Vec::new(),
            run: None,
            flash: None,
            now: Instant::now(),
            clock: SystemTime::now(),
        };
        app.reload_sessions();
        let start = Task::batch([iced::system::theme().map(Msg::ThemeMode), operation::focus(EDITOR)]);
        (app, start)
    }

    pub fn title(&self) -> String {
        format!("{} - sasacode", self.folder_name())
    }

    pub fn theme(&self) -> Theme {
        self.theme.clone()
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        let mut all = vec![
            iced::system::theme_changes().map(Msg::ThemeMode),
            event::listen_with(shortcut),
        ];
        // The clock runs only while something on screen counts time.
        if self.running() || self.flash.is_some() {
            all.push(iced::time::every(Duration::from_millis(500)).map(Msg::Tick));
        }
        Subscription::batch(all)
    }

    pub(crate) fn folder_name(&self) -> String {
        self.cwd
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.cwd.to_string_lossy().into_owned())
    }

    fn reload_sessions(&mut self) {
        self.sessions = match &self.home {
            Some(home) => list_sessions(home, &self.cwd),
            None => Vec::new(),
        };
        self.clock = SystemTime::now();
    }

    pub(crate) fn running(&self) -> bool {
        self.run.is_some()
    }

    /// Seconds since the run started.
    pub(crate) fn elapsed(&self) -> u64 {
        self.run
            .as_ref()
            .map(|r| self.now.saturating_duration_since(r.started).as_secs())
            .unwrap_or(0)
    }

    /// The models to offer: those used from this window, then those this folder's conversations used.
    pub(crate) fn known_models(&self) -> Vec<String> {
        let mut out = self.prefs.models.clone();
        for m in self.sessions.iter().filter_map(|s| s.model.as_ref()) {
            if !out.contains(m) {
                out.push(m.clone());
            }
        }
        out
    }

    fn control(&self, message: ControlMessage) {
        if let Some(tx) = self.run.as_ref().and_then(|r| r.control.as_ref()) {
            let _ = tx.send(message);
        }
    }

    fn save_prefs(&self) {
        if let Some(home) = &self.home {
            // A mode that allows everything is chosen anew each time the window opens.
            let mut kept = self.prefs.clone();
            if matches!(kept.permission.as_deref(), Some("agent" | "auto")) {
                kept.permission = None;
            }
            kept.save(home);
        }
    }

    fn clear_conversation(&mut self) {
        self.transcript = Transcript::default();
        self.open.clear();
        self.open_work.clear();
        self.queued.clear();
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Edit(action) => self.editor.perform(action),
            Msg::Send => return self.send(),
            Msg::Stop => self.control(ControlMessage::Abort),
            Msg::Run(update) => self.on_run(update),
            Msg::Answer(decision) => self.answer(decision),
            Msg::Feedback(text) => self.feedback = text,
            Msg::NewSession => {
                if !self.running() {
                    self.session = None;
                    self.clear_conversation();
                    return operation::focus(EDITOR);
                }
            }
            Msg::OpenSession(path) => {
                if !self.running()
                    && let Ok(entries) = read_entries(&path)
                {
                    let restored = restore(&entries);
                    self.clear_conversation();
                    self.transcript = Transcript::from_messages(&restored.messages);
                    self.session = self.sessions.iter().find(|s| s.path == path).map(|s| s.id.clone());
                    return operation::focus(EDITOR);
                }
            }
            Msg::Search(text) => self.search = text,
            Msg::Model(pick) => {
                if pick.key == Pick::OTHER {
                    self.model_entry = Some(String::new());
                    return operation::focus(MODEL_ENTRY);
                }
                self.prefs.model = (!pick.key.is_empty()).then_some(pick.key);
                self.save_prefs();
            }
            Msg::ModelEntry(text) => self.model_entry = Some(text),
            Msg::ModelCommit => {
                if let Some(name) = self.model_entry.take() {
                    let name = name.trim();
                    if !name.is_empty() {
                        self.prefs.model = Some(name.to_owned());
                        self.prefs.remember_model(name);
                        self.save_prefs();
                    }
                }
                return operation::focus(EDITOR);
            }
            Msg::Permission(pick) => {
                self.prefs.permission = Some(pick.key);
                self.save_prefs();
            }
            Msg::Effort(pick) => {
                self.prefs.effort = (!pick.key.is_empty()).then_some(pick.key);
                self.save_prefs();
            }
            Msg::Toggle(i) => {
                if !self.open.remove(&i) {
                    self.open.insert(i);
                }
            }
            Msg::ToggleWork(i) => {
                if !self.open_work.remove(&i) {
                    self.open_work.insert(i);
                }
            }
            Msg::Link(uri) => open_link(&uri),
            Msg::Copy(text) => {
                self.flash = Some((self.s.copied, Instant::now()));
                return iced::clipboard::write(text);
            }
            Msg::ThemeMode(_) if self.fixed_theme => {}
            Msg::ThemeMode(mode) => {
                self.c = if mode == theme::Mode::Dark { DARK } else { LIGHT };
                self.theme = self.c.theme();
            }
            Msg::Tick(now) => {
                self.now = now;
                if self
                    .flash
                    .is_some_and(|(_, at)| now.saturating_duration_since(at) > FLASH)
                {
                    self.flash = None;
                }
            }
            Msg::Key(Shortcut::Confirm) => {
                if self.transcript.pending_approval().is_some() {
                    self.answer(Decision::Allow);
                } else {
                    return self.send();
                }
            }
            Msg::Key(Shortcut::New) => return self.update(Msg::NewSession),
            Msg::Key(Shortcut::Stop) => self.control(ControlMessage::Abort),
            Msg::Key(Shortcut::Escape) => {
                if self.model_entry.take().is_some() {
                    return operation::focus(EDITOR);
                }
            }
        }
        Task::none()
    }

    fn answer(&mut self, decision: Decision) {
        let Some(approval) = self.transcript.pending_approval() else {
            return;
        };
        let id = approval.id.clone();
        let feedback =
            (decision == Decision::Deny && !self.feedback.trim().is_empty()).then(|| self.feedback.trim().to_owned());
        let said = match decision {
            Decision::Allow => self.s.allowed,
            Decision::Always => self.s.always_allowed,
            Decision::Deny => self.s.denied,
        };
        self.control(ControlMessage::Approval {
            id: id.clone(),
            decision,
            feedback,
        });
        self.transcript.answer_approval(&id, said.into());
        self.feedback.clear();
    }

    fn send(&mut self) -> Task<Msg> {
        let prompt = self.editor.text().trim().to_owned();
        if prompt.is_empty() {
            return Task::none();
        }
        self.editor = text_editor::Content::new();
        if self.running() {
            // Like typing while the TUI is busy: it reaches the model at its next turn, and joins
            // the conversation when the run says it did.
            self.queued.push(prompt.clone());
            self.control(ControlMessage::Message { text: prompt });
            return Task::none();
        }
        self.transcript.send(&prompt);
        if let Some(model) = self.prefs.model.clone() {
            self.prefs.remember_model(&model);
            self.save_prefs();
        }
        let spec = RunSpec {
            program: self.program.clone(),
            cwd: self.cwd.clone(),
            prompt,
            resume: self.session.clone(),
            model: self.prefs.model.clone(),
            permission: self.prefs.permission.clone(),
            thinking: self.prefs.effort.clone(),
        };
        let (task, handle) = Task::run(runner::run(spec), Msg::Run).abortable();
        self.now = Instant::now();
        self.run = Some(Run {
            control: None,
            _handle: handle.abort_on_drop(),
            stderr: Vec::new(),
            ended: false,
            started: self.now,
        });
        task
    }

    fn on_run(&mut self, update: RunUpdate) {
        let seconds = self.elapsed();
        let Some(run) = self.run.as_mut() else { return };
        match update {
            RunUpdate::Started(control) => run.control = Some(control),
            RunUpdate::Event(event) => {
                let effect = self.transcript.apply(&event, &self.s);
                if let Some(id) = effect.session_id {
                    self.session = Some(id);
                }
                if let Some(text) = effect.delivered
                    && let Some(at) = self.queued.iter().position(|q| *q == text)
                {
                    self.queued.remove(at);
                }
                if effect.ended.is_some() {
                    run.ended = true;
                    self.transcript.items.push(Item::Done { seconds });
                }
            }
            RunUpdate::Stderr(line) => {
                run.stderr.push(line);
                if run.stderr.len() > 40 {
                    run.stderr.remove(0);
                }
            }
            RunUpdate::Failed(error) => {
                self.transcript.notice(format!("{}\n{error}", self.s.not_found), true);
                self.run = None;
                self.return_queued();
            }
            RunUpdate::Exited(code) => {
                // A run that never got to its end says why only on stderr.
                if !run.ended {
                    let code = code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
                    let mut text = self.s.exited.replace("{code}", &code);
                    let tail = run.stderr.join("\n");
                    if !tail.trim().is_empty() {
                        text = format!("{text}\n{tail}");
                    }
                    self.transcript.notice(text, true);
                }
                self.run = None;
                self.return_queued();
                self.reload_sessions();
            }
        }
    }

    /// What was sent during the run and never read goes back to the input, so it is not lost.
    fn return_queued(&mut self) {
        if self.queued.is_empty() {
            return;
        }
        let mut text = std::mem::take(&mut self.queued).join("\n\n");
        let draft = self.editor.text();
        if !draft.trim().is_empty() {
            text = format!("{text}\n\n{}", draft.trim_end());
        }
        self.editor = text_editor::Content::with_text(&text);
        self.transcript.notice(self.s.returned, false);
    }
}

/// Keys that work wherever the focus is. Plain Enter belongs to the input (`view.rs`).
fn shortcut(event: Event, _status: event::Status, _window: window::Id) -> Option<Msg> {
    let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    let shortcut = match key.as_ref() {
        Key::Named(Named::Escape) => Shortcut::Escape,
        Key::Named(Named::Enter) if modifiers.command() => Shortcut::Confirm,
        Key::Character("n") if modifiers.command() => Shortcut::New,
        Key::Character(".") if modifiers.command() => Shortcut::Stop,
        _ => return None,
    };
    Some(Msg::Key(shortcut))
}

/// Opens a link from a reply in the system's browser. Only web links: a reply is model output.
fn open_link(uri: &str) {
    if !(uri.starts_with("https://") || uri.starts_with("http://")) {
        return;
    }
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    let _ = cmd.arg(uri).spawn();
}
