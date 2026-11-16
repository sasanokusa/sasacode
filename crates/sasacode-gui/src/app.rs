//! The window's state and what changes it. Drawing is in `view`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{markdown, text_editor};
use iced::{Element, Size, Subscription, Task, Theme, event, task, theme, window};
use sasacode_protocol::{ControlMessage, Decision, SessionSummary, StopCause, list_sessions, read_entries};
use tokio::sync::mpsc::UnboundedSender;

use crate::i18n::{Strings, fill};
use crate::runner::{self, RunSpec, RunUpdate};
use crate::theme::{DARK, LIGHT, Tokens};
use crate::transcript::{Answer, Note, Transcript};

/// The input's name among the widgets: what the keyboard is given to when the window opens and
/// when a conversation is opened.
pub const INPUT: &str = "input";

/// `--permission`, as the pickers list them. The first entry of each list is "what the config says".
pub const PERMISSIONS: [&str; 4] = ["edits", "ask", "agent", "auto"];
/// `--thinking`.
pub const EFFORTS: [&str; 6] = ["off", "low", "medium", "high", "xhigh", "max"];

/// Under this width the conversation list gives way while the inspector is open.
pub const WIDE: f32 = 1180.0;

#[derive(Debug, Clone)]
pub struct Options {
    pub program: PathBuf,
    pub cwd: PathBuf,
    /// The conversation to open with, named as `sasacode -r` takes it.
    pub session: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Changes,
    Plan,
    Commands,
    Usage,
}

/// What the conversation is doing, for the pill next to its title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Blank,
    Running,
    Waiting,
    Done,
    Aborted,
    Error,
    MaxTurns,
    Plugin,
    Loaded,
}

pub struct App {
    pub(crate) s: Strings,
    pub(crate) t: Tokens,
    theme: Theme,
    program: PathBuf,
    pub(crate) cwd: PathBuf,
    home: Option<PathBuf>,
    pub(crate) sessions: Vec<SessionSummary>,
    /// The session the next prompt continues; `None` starts a new one.
    pub(crate) session: Option<String>,
    /// The conversation shown was read from its log, and nothing ran here since.
    pub(crate) loaded: bool,
    pub(crate) transcript: Transcript,
    pub(crate) editor: text_editor::Content,
    pub(crate) model: String,
    pub(crate) permission: Option<&'static str>,
    pub(crate) effort: Option<&'static str>,
    run: Option<Run>,
    /// Sent as soon as the run that is ending is gone.
    pub(crate) next: Option<String>,
    pub(crate) filter: String,
    pub(crate) side: bool,
    pub(crate) insp: bool,
    pub(crate) tab: Tab,
    /// Exchanges whose steps were opened or closed by hand.
    pub(crate) folds: HashMap<usize, bool>,
    /// Rows opened by hand: (exchange, step).
    pub(crate) open: HashSet<(usize, usize)>,
    /// Cards of the inspector closed by hand, by call id.
    pub(crate) closed: HashSet<String>,
    pub(crate) now: Instant,
    pub(crate) width: f32,
}

struct Run {
    control: Option<UnboundedSender<ControlMessage>>,
    /// Stops the process when the run is dropped.
    _handle: Option<task::Handle>,
    stderr: Vec<String>,
    /// `agent_end` arrived: the run is over and its process is closing.
    ended: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Edit(text_editor::Action),
    /// Enter in the input, or its button.
    Send,
    /// The chord: allow the waiting approval when the input is empty, else stop what runs and
    /// send the input next.
    Chord,
    Stop,
    Continue,
    Run(RunUpdate),
    Answer(Decision),
    NewSession,
    OpenSession(PathBuf),
    Filter(String),
    Model(String),
    Permission(&'static str),
    Effort(&'static str),
    ToggleSide,
    ToggleInspector,
    Tab(Tab),
    Fold(usize),
    Open(usize, usize),
    Card(String),
    Link(markdown::Uri),
    ThemeMode(theme::Mode),
    Tick(Instant),
    Resized(Size),
}

impl App {
    pub fn new(options: Options) -> (Self, Task<Msg>) {
        let wanted = options.session.clone();
        let mut app = App::blank(options, Strings::detect());
        app.reload_sessions();
        // One that is not there is not an error: a conversation nobody has said anything in has an
        // id and no log yet. The window opens on a new one.
        if let Some(wanted) = wanted
            && !app.open_named(&wanted)
        {
            eprintln!(
                "sasacode-gui: no session {wanted} in {}: starting a new conversation",
                app.cwd.display()
            );
        }
        (
            app,
            Task::batch([iced::system::theme().map(Msg::ThemeMode), focus_input()]),
        )
    }

    /// A window with no conversation and no list of sessions.
    pub(crate) fn blank(options: Options, s: Strings) -> Self {
        App {
            s,
            t: LIGHT,
            theme: LIGHT.theme(),
            program: options.program,
            cwd: options.cwd,
            home: sasacode_protocol::sasacode_home(),
            sessions: Vec::new(),
            session: None,
            loaded: false,
            transcript: Transcript::default(),
            editor: text_editor::Content::new(),
            model: String::new(),
            permission: None,
            effort: None,
            run: None,
            next: None,
            filter: String::new(),
            side: true,
            insp: true,
            tab: Tab::Changes,
            folds: HashMap::new(),
            open: HashSet::new(),
            closed: HashSet::new(),
            now: Instant::now(),
            width: 1280.0,
        }
    }

    pub fn title(&self) -> String {
        let folder = self
            .cwd
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("sasacode — {folder}")
    }

    pub fn theme(&self) -> Theme {
        self.theme.clone()
    }

    pub(crate) fn set_dark(&mut self, dark: bool) {
        self.t = if dark { DARK } else { LIGHT };
        self.theme = self.t.theme();
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        let mut all = vec![
            iced::system::theme_changes().map(Msg::ThemeMode),
            window::resize_events().map(|(_, size)| Msg::Resized(size)),
            event::listen_with(global_key),
        ];
        // The clock only moves what is shown while something runs.
        if self.running() {
            all.push(iced::time::every(Duration::from_secs(1)).map(Msg::Tick));
        }
        Subscription::batch(all)
    }

    pub fn view(&self) -> Element<'_, Msg> {
        crate::view::window(self)
    }

    /// As if a run were going on, to draw and to drive that state without a process. What the
    /// window would write to the process comes out of the returned end.
    #[cfg(test)]
    pub(crate) fn pretend_running(&mut self) -> tokio::sync::mpsc::UnboundedReceiver<ControlMessage> {
        let (control, written) = tokio::sync::mpsc::unbounded_channel();
        self.run = Some(Run {
            control: Some(control),
            _handle: None,
            stderr: Vec::new(),
            ended: false,
        });
        written
    }

    fn reload_sessions(&mut self) {
        self.sessions = match &self.home {
            Some(home) => list_sessions(home, &self.cwd),
            None => Vec::new(),
        };
    }

    /// The agent works: its run has started and has not said it ended.
    pub(crate) fn running(&self) -> bool {
        self.run.as_ref().is_some_and(|r| !r.ended)
    }

    /// A run's process is still there, working or closing. Conversations cannot be switched.
    pub(crate) fn busy(&self) -> bool {
        self.run.is_some()
    }

    pub(crate) fn status(&self) -> Status {
        if self.running() {
            return if self.transcript.pending_approval().is_some() {
                Status::Waiting
            } else {
                Status::Running
            };
        }
        if self.loaded {
            return Status::Loaded;
        }
        match self
            .transcript
            .exchanges
            .last()
            .and_then(|e| e.end.as_ref())
            .map(|e| e.cause)
        {
            None => Status::Blank,
            Some(StopCause::Done) => Status::Done,
            Some(StopCause::Aborted) => Status::Aborted,
            Some(StopCause::MaxTurns) => Status::MaxTurns,
            Some(StopCause::Stopped) => Status::Plugin,
            Some(_) => Status::Error,
        }
    }

    /// Whether the list of conversations is drawn: the inspector takes its room in a narrow window.
    pub(crate) fn shows_side(&self) -> bool {
        self.side && (self.width >= WIDE || !self.insp)
    }

    fn control(&self, message: ControlMessage) {
        if let Some(tx) = self.run.as_ref().and_then(|r| r.control.as_ref()) {
            let _ = tx.send(message);
        }
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Edit(action) => self.editor.perform(action),
            Msg::Send => return self.send(),
            Msg::Chord => {
                let empty = self.editor.text().trim().is_empty();
                if self.transcript.pending_approval().is_some() {
                    // A reply denies; only the empty chord allows.
                    return if empty {
                        self.answer(Decision::Allow)
                    } else {
                        self.send()
                    };
                }
                if empty {
                    return Task::none();
                }
                if self.running() {
                    self.control(ControlMessage::Abort);
                    self.queue_next();
                    return Task::none();
                }
                return self.send();
            }
            Msg::Stop => {
                if self.running() {
                    self.control(ControlMessage::Abort);
                }
            }
            Msg::Continue => {
                if !self.busy() {
                    return self.start(self.s.continue_text.to_owned());
                }
            }
            Msg::Run(update) => return self.on_run(update),
            Msg::Answer(decision) => return self.answer(decision),
            Msg::NewSession => {
                if !self.busy() {
                    self.session = None;
                    self.show(Transcript::default(), false);
                    return focus_input();
                }
            }
            Msg::OpenSession(path) => {
                if !self.busy() && self.open_session(&path) {
                    return focus_input();
                }
            }
            Msg::Filter(text) => self.filter = text,
            Msg::Model(model) => self.model = model,
            Msg::Permission(p) => self.permission = PERMISSIONS.contains(&p).then_some(p),
            Msg::Effort(e) => self.effort = EFFORTS.contains(&e).then_some(e),
            Msg::ToggleSide => {
                self.side = !self.shows_side();
                // In a narrow window the two side panels take turns.
                if self.side && self.width < WIDE {
                    self.insp = false;
                }
            }
            Msg::ToggleInspector => self.insp = !self.insp,
            Msg::Tab(tab) => {
                self.tab = tab;
                self.insp = true;
            }
            Msg::Fold(i) => {
                let open = self.steps_open(i);
                self.folds.insert(i, !open);
            }
            Msg::Open(i, j) => {
                if !self.open.remove(&(i, j)) {
                    self.open.insert((i, j));
                }
            }
            Msg::Card(id) => {
                if !self.closed.remove(&id) {
                    self.closed.insert(id);
                }
            }
            Msg::Link(uri) => open_link(&uri),
            Msg::ThemeMode(mode) => self.set_dark(mode == theme::Mode::Dark),
            Msg::Tick(now) => self.now = now,
            Msg::Resized(size) => self.width = size.width,
        }
        Task::none()
    }

    /// Shows the conversation a log holds, to be continued by the next message. False when the log
    /// cannot be read.
    fn open_session(&mut self, path: &Path) -> bool {
        let Ok(entries) = read_entries(path) else {
            return false;
        };
        self.session = self.sessions.iter().find(|s| s.path == path).map(|s| s.id.clone());
        self.show(Transcript::from_entries(&entries), true);
        true
    }

    /// Opens the conversation of this folder that `sasacode -r` would resume for `named`: its id,
    /// its log, or the start of its id, the newest when several match.
    pub(crate) fn open_named(&mut self, named: &str) -> bool {
        let found = self
            .sessions
            .iter()
            .find(|s| s.id == named || s.path == Path::new(named) || s.id.starts_with(named))
            .map(|s| s.path.clone());
        found.is_some_and(|path| self.open_session(&path))
    }

    fn show(&mut self, transcript: Transcript, loaded: bool) {
        self.transcript = transcript;
        self.loaded = loaded;
        self.folds.clear();
        self.open.clear();
        self.closed.clear();
    }

    /// Whether the steps of an exchange are shown. While it runs, and when it stopped short with no
    /// reply to read instead, they are; a finished one folds them away.
    pub(crate) fn steps_open(&self, i: usize) -> bool {
        if let Some(open) = self.folds.get(&i) {
            return *open;
        }
        let Some(ex) = self.transcript.exchanges.get(i) else {
            return false;
        };
        let last = i + 1 == self.transcript.exchanges.len();
        let short = ex.end.as_ref().is_some_and(|end| end.cause != StopCause::Done);
        (last && self.running()) || (short && ex.answer().is_none())
    }

    fn take_input(&mut self) -> Option<String> {
        let text = self.editor.text().trim().to_owned();
        if text.is_empty() {
            return None;
        }
        self.editor = text_editor::Content::new();
        Some(text)
    }

    /// Keeps the input for when the run that is ending is gone.
    fn queue_next(&mut self) {
        if let Some(text) = self.take_input() {
            self.next = Some(match self.next.take() {
                Some(before) => format!("{before}\n\n{text}"),
                None => text,
            });
        }
    }

    fn send(&mut self) -> Task<Msg> {
        // An approval waits: a reply denies it and tells the model why.
        if let Some(approval) = self.transcript.pending_approval() {
            let id = approval.id.clone();
            let Some(text) = self.take_input() else {
                return Task::none();
            };
            self.control(ControlMessage::Approval {
                id: id.clone(),
                decision: Decision::Deny,
                feedback: Some(text.clone()),
            });
            self.transcript.answer_approval(&id, Answer::Denied(text));
            return Task::none();
        }
        if self.running() {
            // Like typing while the TUI is busy: it reaches the model at its next turn.
            if let Some(text) = self.take_input() {
                self.transcript.queue(&text);
                self.control(ControlMessage::Message { text });
            }
            return Task::none();
        }
        if self.busy() {
            self.queue_next();
            return Task::none();
        }
        match self.take_input() {
            Some(text) => self.start(text),
            None => Task::none(),
        }
    }

    fn answer(&mut self, decision: Decision) -> Task<Msg> {
        if let Some(approval) = self.transcript.pending_approval() {
            let id = approval.id.clone();
            self.control(ControlMessage::Approval {
                id: id.clone(),
                decision,
                feedback: None,
            });
            let answer = match decision {
                Decision::Allow => Answer::Allowed,
                Decision::Always => Answer::Always,
                Decision::Deny => Answer::Denied(String::new()),
            };
            self.transcript.answer_approval(&id, answer);
        }
        Task::none()
    }

    fn start(&mut self, prompt: String) -> Task<Msg> {
        self.now = Instant::now();
        self.transcript.send(&prompt, self.now);
        self.loaded = false;
        let model = self.model.trim();
        let spec = RunSpec {
            program: self.program.clone(),
            cwd: self.cwd.clone(),
            prompt,
            resume: self.session.clone(),
            model: (!model.is_empty()).then(|| model.to_owned()),
            permission: self.permission.map(str::to_owned),
            thinking: self.effort.map(str::to_owned),
        };
        let (task, handle) = Task::run(runner::run(spec), Msg::Run).abortable();
        self.run = Some(Run {
            control: None,
            _handle: Some(handle.abort_on_drop()),
            stderr: Vec::new(),
            ended: false,
        });
        task
    }

    fn on_run(&mut self, update: RunUpdate) -> Task<Msg> {
        let Some(run) = self.run.as_mut() else {
            return Task::none();
        };
        self.now = Instant::now();
        match update {
            RunUpdate::Started(control) => run.control = Some(control),
            RunUpdate::Event(event) => {
                let effect = self.transcript.apply(&event, self.now);
                if let Some(id) = effect.session_id {
                    self.session = Some(id);
                }
                if effect.ended.is_some() {
                    run.ended = true;
                }
                if effect.approval.is_some() {
                    // The run waits for the user: say so even when the window is behind others.
                    return window::latest()
                        .and_then(|id| window::request_user_attention(id, Some(window::UserAttention::Informational)));
                }
            }
            RunUpdate::Stderr(line) => {
                run.stderr.push(line);
                if run.stderr.len() > 40 {
                    run.stderr.remove(0);
                }
            }
            RunUpdate::Failed(error) => {
                self.run = None;
                self.transcript.close(self.now);
                self.transcript
                    .note(Note::Error(format!("{}\n{error}", self.s.not_found)));
            }
            RunUpdate::Exited(code) => {
                // A run that never got to its end says why only on stderr.
                if !run.ended {
                    let code = code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
                    let mut text = fill(self.s.exited, "code", code);
                    let tail = run.stderr.join("\n");
                    if !tail.trim().is_empty() {
                        text = format!("{text}\n{tail}");
                    }
                    self.transcript.note(Note::Error(text));
                }
                self.run = None;
                self.transcript.close(self.now);
                self.reload_sessions();
                // What was sent while it ran and never reached the model, and what waited for it
                // to end, start the next run.
                let mut waiting = std::mem::take(&mut self.transcript.queued);
                waiting.extend(self.next.take());
                if !waiting.is_empty() {
                    return self.start(waiting.join("\n\n"));
                }
            }
        }
        Task::none()
    }
}

/// Gives the keyboard to the input.
fn focus_input() -> Task<Msg> {
    iced::widget::operation::focus(INPUT)
}

/// Keys no part of the window took for itself.
fn global_key(event: iced::Event, status: event::Status, _window: window::Id) -> Option<Msg> {
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    if status == event::Status::Captured {
        return None;
    }
    shortcut(&key, modifiers)
}

/// The window's shortcuts. None of them is a bare letter or a bare Enter: a key meant for the
/// input must never answer an approval.
pub(crate) fn shortcut(key: &Key, modifiers: keyboard::Modifiers) -> Option<Msg> {
    match key.as_ref() {
        Key::Named(Named::Escape) => Some(Msg::Stop),
        Key::Named(Named::Enter) if modifiers.command() => Some(Msg::Chord),
        Key::Character("b") if modifiers.command() => Some(Msg::ToggleSide),
        Key::Character("i") if modifiers.command() => Some(Msg::ToggleInspector),
        Key::Character("n") if modifiers.command() => Some(Msg::NewSession),
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    /// A window on a folder whose logs are the recorded conversation under each of `ids`, the
    /// first the newest. The home it reads them from is returned to be removed.
    fn window_with(name: &str, ids: &[&str]) -> (App, PathBuf) {
        let home = std::env::temp_dir().join(format!("sasacode-gui-{}-{name}", std::process::id()));
        let cwd = home.join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let dir = sasacode_protocol::session_dir(&home, &cwd);
        std::fs::create_dir_all(&dir).unwrap();
        let log = crate::snapshots::fixture("work-session.jsonl");
        for (age, id) in ids.iter().enumerate() {
            let path = dir.join(format!("{id}.jsonl"));
            std::fs::write(&path, log.replacen("17cae3ae", id, 1)).unwrap();
            let then = SystemTime::now() - Duration::from_secs(60 * age as u64);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(then)
                .unwrap();
        }
        let options = Options {
            program: "sasacode".into(),
            cwd,
            session: None,
        };
        let mut app = App::blank(options, Strings::get("ja"));
        app.home = Some(home.clone());
        app.reload_sessions();
        (app, home)
    }

    #[test]
    fn a_conversation_is_opened_by_what_resume_takes() {
        let (mut app, home) = window_with("named", &["17cab000", "17cae3ae", "9d000000"]);
        assert_eq!(app.sessions.len(), 3);

        // Its id.
        assert!(app.open_named("9d000000"));
        assert_eq!(app.session.as_deref(), Some("9d000000"));
        assert!(app.loaded && !app.transcript.is_empty());
        // The start of an id: the newest of those it starts. The next message continues it by
        // its whole id.
        assert!(app.open_named("17ca"));
        assert_eq!(app.session.as_deref(), Some("17cab000"));
        // Its log.
        let log = app.sessions.iter().find(|s| s.id == "17cae3ae").unwrap().path.clone();
        assert!(app.open_named(&log.to_string_lossy()));
        assert_eq!(app.session.as_deref(), Some("17cae3ae"));

        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_conversation_that_is_not_there_leaves_a_new_one() {
        let (mut app, home) = window_with("missing", &["17cae3ae"]);
        assert!(!app.open_named("0000"));
        assert!(app.session.is_none() && !app.loaded && app.transcript.is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }
}
