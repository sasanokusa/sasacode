//! The window's state and what changes it. How it looks is in `view.rs` and `view/`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{markdown, operation, text_editor};
use iced::{Event, Subscription, Task, Theme, event, task, theme, window};
use sasacode_protocol::{ControlMessage, Decision, SessionSummary, list_sessions, read_entries, restore};
use tokio::sync::mpsc::UnboundedSender;

use crate::i18n::Strings;
use crate::palette::{Command, Mode, Palette};
use crate::prefs::Prefs;
use crate::runner::{self, RunSpec, RunUpdate};
use crate::style::{Colors, DARK, LIGHT};
use crate::transcript::{Item, Transcript, Verdict};

pub const PERMISSIONS: [&str; 4] = ["edits", "ask", "agent", "auto"];
pub const EFFORTS: [&str; 6] = ["off", "low", "medium", "high", "xhigh", "max"];
pub const EDITOR: &str = "editor";
pub const FIND: &str = "find";
pub const SCROLL: &str = "conversation";
/// How long "Copied" stays under the input.
const FLASH: Duration = Duration::from_secs(2);
/// How often the window is redrawn while a run is on: the mark of the running turn breathes.
const BEAT: Duration = Duration::from_millis(125);

#[derive(Debug, Clone)]
pub struct Options {
    pub program: PathBuf,
    pub cwd: PathBuf,
    /// The window has no title bar of its own (macOS): its top is what it is moved by.
    pub unified: bool,
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
    /// The folder's git branch, when it has one.
    pub(crate) branch: Option<String>,
    pub(crate) sessions: Vec<SessionSummary>,
    /// The session the next prompt continues; `None` starts a new one.
    pub(crate) session: Option<String>,
    pub(crate) transcript: Transcript,
    pub(crate) editor: text_editor::Content,
    pub(crate) feedback: String,
    pub(crate) prefs: Prefs,
    pub(crate) sidebar: bool,
    /// The panel of changed files is shown.
    pub(crate) changes: bool,
    pub(crate) palette: Option<Palette>,
    /// Turns opened or closed by hand, by the turn's first item. The last turn is open otherwise.
    pub(crate) turns: HashMap<usize, bool>,
    /// All earlier requests are listed, not only the latest few.
    pub(crate) earlier: bool,
    /// Steps whose detail is shown, by item.
    pub(crate) open: HashSet<usize>,
    /// Turns whose folded work is shown, by the turn's first item.
    pub(crate) open_work: HashSet<usize>,
    /// Sent during the run and not yet read by the model.
    pub(crate) queued: Vec<String>,
    pub(crate) run: Option<Run>,
    pub(crate) flash: Option<(&'static str, Instant)>,
    pub(crate) unified: bool,
    /// The conversation is scrolled away from its top.
    pub(crate) scrolled: bool,
    /// The conversation's end is kept in sight while a run adds to it, until the person scrolls away.
    following: bool,
    window: Option<window::Id>,
    focused: bool,
    /// How large the window is: what is drawn beside the conversation depends on the room there is.
    pub(crate) width: f32,
    pub(crate) height: f32,
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
    /// Up in an empty input brings the last request back.
    Recall,
    Run(RunUpdate),
    Answer(Decision),
    Feedback(String),
    NewSession,
    OpenSession(PathBuf),
    /// Open or close a turn.
    Turn(usize),
    Earlier,
    Toggle(usize),
    ToggleWork(usize),
    Sidebar,
    Changes,
    /// Show the command list, or with `None` put it away.
    Find(Option<Mode>),
    FindQuery(String),
    FindRun(usize),
    FindSubmit,
    Link(markdown::Uri),
    Copy(String),
    /// The input is pressed anywhere around its text.
    Write,
    /// The conversation was scrolled: whether it is away from its top, and whether it is at its end.
    Scrolled(bool, bool),
    ThemeMode(theme::Mode),
    Tick(Instant),
    Key(Shortcut),
    Window(Option<window::Id>),
    Focus(window::Id, bool),
    Resized(iced::Size),
    Dropped(PathBuf),
    /// The top of the window is held: move the window (macOS, where the window has no title bar).
    Drag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    /// Ctrl/⌘+Enter: allow what is asked, else send.
    Confirm,
    /// Ctrl/⌘+K.
    Find,
    /// Ctrl/⌘+N.
    New,
    /// Ctrl/⌘+B.
    Sidebar,
    /// Ctrl/⌘+D.
    Changes,
    /// Ctrl/⌘+.
    Stop,
    Escape,
    Up,
    Down,
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
            branch: branch(&options.cwd),
            cwd: options.cwd,
            home,
            sessions: Vec::new(),
            session: None,
            transcript: Transcript::default(),
            editor: text_editor::Content::new(),
            feedback: String::new(),
            sidebar: !prefs.hide_sidebar,
            prefs,
            changes: false,
            palette: None,
            turns: HashMap::new(),
            earlier: false,
            open: HashSet::new(),
            open_work: HashSet::new(),
            queued: Vec::new(),
            run: None,
            flash: None,
            unified: options.unified,
            scrolled: false,
            following: true,
            window: None,
            focused: true,
            width: crate::WINDOW.width,
            height: crate::WINDOW.height,
            now: Instant::now(),
            clock: SystemTime::now(),
        };
        app.reload_sessions();
        let start = Task::batch([
            iced::system::theme().map(Msg::ThemeMode),
            window::latest().map(Msg::Window),
            operation::focus(EDITOR),
        ]);
        (app, start)
    }

    pub fn title(&self) -> String {
        let name = format!("{} - sasacode", self.folder_name());
        if self.transcript.pending_approval().is_some() {
            format!("{} - {name}", self.s.title_awaiting)
        } else {
            name
        }
    }

    pub fn theme(&self) -> Theme {
        self.theme.clone()
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        let mut all = vec![
            iced::system::theme_changes().map(Msg::ThemeMode),
            event::listen_with(listen),
        ];
        // The clock runs only while something on screen counts time.
        if self.running() {
            all.push(iced::time::every(BEAT).map(Msg::Tick));
        } else if self.flash.is_some() {
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
        self.branch = branch(&self.cwd);
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

    /// Between 0 and 1, rising and falling about every second and a half while a run is on.
    pub(crate) fn breath(&self) -> f32 {
        let Some(run) = &self.run else { return 1.0 };
        let phase = (self.now.saturating_duration_since(run.started).as_millis() % 1500) as f32 / 1500.0;
        (phase * std::f32::consts::TAU).cos() * 0.5 + 0.5
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

    pub(crate) fn model_label(&self) -> String {
        self.prefs.model.clone().unwrap_or_else(|| self.s.model_default.into())
    }

    /// The permission mode the next run gets: what the CLI does when it is told nothing is `edits`.
    pub(crate) fn permission(&self) -> &str {
        self.prefs.permission.as_deref().unwrap_or("edits")
    }

    pub(crate) fn effort_label(&self) -> String {
        match &self.prefs.effort {
            Some(level) => self.s.effort(level),
            None => self.s.effort_default.into(),
        }
    }

    /// The text of the conversation's last answer.
    pub(crate) fn last_answer(&self) -> Option<&str> {
        let turn = *self.transcript.turns().last()?;
        match &self.transcript.items[self.transcript.answer(turn)?] {
            Item::Assistant(md) => Some(&md.raw),
            _ => None,
        }
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

    /// The conversation changed its height by a press. A sheet that now fits is at its top; one that
    /// does not says where it is with its next report.
    fn resized(&mut self) {
        self.scrolled = false;
    }

    fn clear_conversation(&mut self) {
        self.transcript = Transcript::default();
        self.turns.clear();
        self.earlier = false;
        self.open.clear();
        self.open_work.clear();
        self.queued.clear();
        self.scrolled = false;
    }

    /// Back to the top of the conversation, with the input ready.
    fn settle(&self) -> Task<Msg> {
        Task::batch([
            operation::snap_to(SCROLL, operation::RelativeOffset::START),
            operation::focus(EDITOR),
        ])
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        // A press on anything that is not a field takes the focus from the input. It goes back
        // there, so the next key typed is never lost.
        let pressed = matches!(
            msg,
            Msg::Turn(_)
                | Msg::Earlier
                | Msg::Toggle(_)
                | Msg::ToggleWork(_)
                | Msg::Sidebar
                | Msg::Changes
                | Msg::Copy(_)
                | Msg::Link(_)
                | Msg::Answer(_)
                | Msg::Stop
                | Msg::Write
        );
        let task = self.handle(msg);
        if pressed && self.palette.is_none() {
            Task::batch([task, operation::focus(EDITOR)])
        } else {
            task
        }
    }

    fn handle(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Edit(action) => self.editor.perform(action),
            Msg::Send => return self.send(),
            Msg::Stop => self.control(ControlMessage::Abort),
            Msg::Recall => {
                let last = self.transcript.items.iter().rev().find_map(|i| match i {
                    Item::User(text) => Some(text.clone()),
                    _ => None,
                });
                if let Some(text) = last
                    && self.editor.text().trim().is_empty()
                {
                    self.editor = text_editor::Content::with_text(&text);
                    self.editor
                        .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
                }
            }
            Msg::Run(update) => return self.on_run(update),
            Msg::Answer(decision) => self.answer(decision),
            Msg::Feedback(text) => self.feedback = text,
            Msg::NewSession => {
                if !self.running() {
                    self.session = None;
                    self.clear_conversation();
                    return self.settle();
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
                    return self.settle();
                }
            }
            Msg::Turn(start) => {
                let last = self.transcript.turns().last().map(|t| t.start);
                let open = self.turns.get(&start).copied().unwrap_or(last == Some(start));
                self.turns.insert(start, !open);
                self.resized();
            }
            Msg::Earlier => {
                self.earlier = !self.earlier;
                self.resized();
            }
            Msg::Toggle(i) => {
                if !self.open.remove(&i) {
                    self.open.insert(i);
                }
                self.resized();
            }
            Msg::ToggleWork(i) => {
                if !self.open_work.remove(&i) {
                    self.open_work.insert(i);
                }
                self.resized();
            }
            Msg::Sidebar => {
                self.sidebar = !self.sidebar;
                self.prefs.hide_sidebar = !self.sidebar;
                self.save_prefs();
            }
            Msg::Changes => self.changes = !self.changes,
            Msg::Find(Some(mode)) => {
                self.palette = Some(Palette::new(mode));
                return operation::focus(FIND);
            }
            Msg::Find(None) => {
                self.palette = None;
                return operation::focus(EDITOR);
            }
            Msg::FindQuery(query) => {
                if let Some(palette) = &mut self.palette {
                    palette.query = query;
                    palette.cursor = 0;
                    return operation::snap_to(crate::view::FIND_LIST, operation::RelativeOffset::START);
                }
            }
            Msg::FindRun(at) => {
                if let Some(palette) = &self.palette
                    && let Some(entry) = self.entries(palette).into_iter().nth(at)
                {
                    return self.command(entry.command);
                }
            }
            Msg::FindSubmit => {
                if let Some(palette) = &self.palette {
                    return self.handle(Msg::FindRun(palette.cursor));
                }
            }
            Msg::Link(uri) => open_link(&uri),
            Msg::Copy(text) => {
                self.flash = Some((self.s.copied, Instant::now()));
                return iced::clipboard::write(text);
            }
            Msg::Write => {}
            Msg::Scrolled(away, end) => {
                self.scrolled = away;
                self.following = end;
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
            Msg::Key(shortcut) => return self.key(shortcut),
            Msg::Window(id) => self.window = id,
            Msg::Focus(id, focused) => {
                self.window = Some(id);
                self.focused = focused;
            }
            Msg::Resized(size) => (self.width, self.height) = (size.width, size.height),
            Msg::Dropped(path) => {
                // A file dropped on the window is named in the input, as the agent would be told it.
                let shown = path.strip_prefix(&self.cwd).unwrap_or(&path).to_string_lossy();
                let name = if shown.contains(' ') {
                    format!("\"{shown}\" ")
                } else {
                    format!("{shown} ")
                };
                self.editor
                    .perform(text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(name))));
                return operation::focus(EDITOR);
            }
            Msg::Drag => {
                if let Some(id) = self.window {
                    return window::drag(id);
                }
            }
        }
        Task::none()
    }

    fn key(&mut self, shortcut: Shortcut) -> Task<Msg> {
        match shortcut {
            Shortcut::Find => {
                let next = self.palette.is_none().then_some(Mode::Root);
                return self.update(Msg::Find(next));
            }
            Shortcut::Escape => {
                if self.palette.is_some() {
                    return self.update(Msg::Find(None));
                }
            }
            Shortcut::Up | Shortcut::Down => {
                if let Some(palette) = &self.palette {
                    let last = self.entries(palette).len().saturating_sub(1);
                    let at = palette.cursor.min(last);
                    let cursor = match shortcut {
                        Shortcut::Up if at == 0 => last,
                        Shortcut::Up => at - 1,
                        _ if at == last => 0,
                        _ => at + 1,
                    };
                    self.palette.as_mut().expect("checked above").cursor = cursor;
                    // The list moves in step with the cursor, so the chosen row is never out of
                    // sight: as far down the list as the cursor is down its rows.
                    let y = if last == 0 { 0.0 } else { cursor as f32 / last as f32 };
                    return operation::snap_to(crate::view::FIND_LIST, operation::RelativeOffset { x: 0.0, y });
                }
            }
            // The rest act on the window behind the list, so they wait until it is put away.
            _ if self.palette.is_some() => {}
            Shortcut::Confirm => {
                if self.transcript.pending_approval().is_some() {
                    self.answer(Decision::Allow);
                } else {
                    return self.send();
                }
            }
            Shortcut::New => return self.update(Msg::NewSession),
            // In a window too narrow for the list, its key has nothing to show or hide.
            Shortcut::Sidebar if !self.list_fits() => {}
            Shortcut::Sidebar => return self.update(Msg::Sidebar),
            Shortcut::Changes => return self.update(Msg::Changes),
            Shortcut::Stop => self.control(ControlMessage::Abort),
        }
        Task::none()
    }

    /// Does what an entry of the command list stands for.
    fn command(&mut self, command: Command) -> Task<Msg> {
        if let Command::Go(mode) = command {
            return self.update(Msg::Find(Some(mode)));
        }
        self.palette = None;
        let done = match command {
            Command::New => self.update(Msg::NewSession),
            Command::Stop => self.update(Msg::Stop),
            Command::Sidebar => self.update(Msg::Sidebar),
            Command::Changes => self.update(Msg::Changes),
            Command::CopyAnswer => match self.last_answer() {
                Some(answer) => self.update(Msg::Copy(answer.to_owned())),
                None => Task::none(),
            },
            Command::Open(path) => self.update(Msg::OpenSession(path)),
            Command::Model(model) => {
                if let Some(name) = &model {
                    self.prefs.remember_model(name);
                }
                self.prefs.model = model;
                self.save_prefs();
                Task::none()
            }
            Command::Permission(mode) => {
                self.prefs.permission = Some(mode);
                self.save_prefs();
                Task::none()
            }
            Command::Effort(level) => {
                self.prefs.effort = level;
                self.save_prefs();
                Task::none()
            }
            Command::Go(_) => Task::none(),
        };
        Task::batch([done, operation::focus(EDITOR)])
    }

    fn answer(&mut self, decision: Decision) {
        let Some(approval) = self.transcript.pending_approval() else {
            return;
        };
        let id = approval.id.clone();
        let feedback =
            (decision == Decision::Deny && !self.feedback.trim().is_empty()).then(|| self.feedback.trim().to_owned());
        let verdict = match decision {
            Decision::Allow => Verdict::Allowed,
            Decision::Always => Verdict::Always,
            Decision::Deny => Verdict::Denied,
        };
        self.control(ControlMessage::Approval {
            id: id.clone(),
            decision,
            feedback,
        });
        self.transcript.answer_approval(&id, verdict);
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
        // The new request is what the sheet is about now: earlier ones close and step back.
        self.turns.clear();
        self.earlier = false;
        self.scrolled = false;
        self.following = true;
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
        Task::batch([task, self.settle()])
    }

    /// Asks for the person's eyes when the window is behind another one.
    fn call(&self) -> Task<Msg> {
        match self.window {
            Some(id) if !self.focused => window::request_user_attention(id, Some(window::UserAttention::Informational)),
            _ => Task::none(),
        }
    }

    fn on_run(&mut self, update: RunUpdate) -> Task<Msg> {
        let seconds = self.elapsed();
        let Some(run) = self.run.as_mut() else {
            return Task::none();
        };
        match update {
            RunUpdate::Started(control) => run.control = Some(control),
            RunUpdate::Event(event) => {
                let rows = self.transcript.items.len();
                let effect = self.transcript.apply(&event, &self.s);
                let grew = self.transcript.items.len() > rows;
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
                let mut tasks = Vec::new();
                if effect.approval.is_some() || effect.ended.is_some() {
                    tasks.push(self.call());
                }
                // While the person stays at the end, what the agent does next stays in sight.
                if grew && self.following && effect.ended.is_none() {
                    tasks.push(operation::snap_to_end(SCROLL));
                }
                return Task::batch(tasks);
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
                return self.rest();
            }
            RunUpdate::Exited(code) => {
                // A run that never got to its end says why only on stderr.
                let failed = !run.ended;
                if failed {
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
                let rest = self.rest();
                return if failed { Task::batch([rest, self.call()]) } else { rest };
            }
        }
        Task::none()
    }

    /// The run is over and its work folds into one line. For a person who stayed with it, the sheet
    /// goes back to its top: the answer is read from its start.
    fn rest(&mut self) -> Task<Msg> {
        if !self.following {
            return Task::none();
        }
        self.scrolled = false;
        operation::snap_to(SCROLL, operation::RelativeOffset::START)
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

/// Keys that work wherever the focus is, and what the window itself is told. Plain Enter belongs to
/// the field that has the focus.
fn listen(event: Event, status: event::Status, window: window::Id) -> Option<Msg> {
    let (key, physical_key, modifiers) = match event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key,
            physical_key,
            modifiers,
            ..
        }) => (key, physical_key, modifiers),
        Event::Window(window::Event::Focused) => return Some(Msg::Focus(window, true)),
        Event::Window(window::Event::Unfocused) => return Some(Msg::Focus(window, false)),
        Event::Window(window::Event::FileDropped(path)) => return Some(Msg::Dropped(path)),
        Event::Window(window::Event::Opened { size, .. } | window::Event::Resized(size)) => {
            return Some(Msg::Resized(size));
        }
        _ => return None,
    };
    let shortcut = match key.as_ref() {
        Key::Named(Named::Escape) => Shortcut::Escape,
        Key::Named(Named::ArrowUp) => Shortcut::Up,
        Key::Named(Named::ArrowDown) => Shortcut::Down,
        // A field that took the Enter (a reason for a refusal) has already acted on it.
        Key::Named(Named::Enter) if modifiers.command() && status == event::Status::Ignored => Shortcut::Confirm,
        Key::Character(".") if modifiers.command() => Shortcut::Stop,
        _ if modifiers.command() => match key.to_latin(physical_key) {
            Some('k') => Shortcut::Find,
            Some('n') => Shortcut::New,
            Some('b') => Shortcut::Sidebar,
            Some('d') => Shortcut::Changes,
            _ => return None,
        },
        _ => return None,
    };
    Some(Msg::Key(shortcut))
}

/// The branch the folder's repository is on, or the start of the commit when it is on none.
/// `None` outside a repository.
fn branch(cwd: &Path) -> Option<String> {
    let dot_git = cwd.ancestors().map(|dir| dir.join(".git")).find(|p| p.exists())?;
    // A worktree's `.git` is a file that says where its git directory is.
    let git_dir = if dot_git.is_file() {
        let pointer = std::fs::read_to_string(&dot_git).ok()?;
        dot_git.parent()?.join(pointer.trim().strip_prefix("gitdir:")?.trim())
    } else {
        dot_git
    };
    head_name(&std::fs::read_to_string(git_dir.join("HEAD")).ok()?)
}

fn head_name(head: &str) -> Option<String> {
    let head = head.trim();
    match head.strip_prefix("ref: refs/heads/") {
        Some(name) => Some(name.to_owned()),
        None => head.get(..7).map(str::to_owned),
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
    use super::*;

    #[test]
    fn head_names_the_branch_or_the_start_of_a_detached_commit() {
        assert_eq!(head_name("ref: refs/heads/feat/gui\n").as_deref(), Some("feat/gui"));
        assert_eq!(
            head_name("89a3acd0f6c2b1e4d5a69788c0ffee0123456789\n").as_deref(),
            Some("89a3acd")
        );
        assert_eq!(head_name(""), None);
    }
}
