//! The window's state and what changes it. Drawing is in `view`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::keyboard::{self, Key, key::Named};
use iced::widget::operation::{self, AbsoluteOffset};
use iced::widget::{markdown, text_editor};
use iced::{Element, Size, Subscription, Task, Theme, event, task, theme, window};
use sasacode_protocol::{ControlMessage, Decision, SessionSummary, StopCause, list_sessions, read_entries};
use tokio::sync::mpsc::UnboundedSender;

use crate::i18n::{Strings, fill};
use crate::layout::{self, Layout};
use crate::runner::{self, RunSpec, RunUpdate};
use crate::theme::{DARK, LIGHT, Tokens};
use crate::transcript::{Answer, Note, Step, Transcript};

/// The input's name among the widgets: what the keyboard is given to when the window opens and
/// when a conversation is opened.
pub const INPUT: &str = "input";
/// The search of the conversation list.
pub const SEARCH: &str = "search";
/// The conversation, scrolled back to its newest.
pub const TIMELINE: &str = "timeline";

/// How far from its newest the conversation still counts as at it, in pixels: a nudge of the
/// trackpad is not reading back.
const NEAR_END: f32 = 24.0;
/// How long a copy button says it copied.
const COPIED_FOR: Duration = Duration::from_millis(1600);

/// `--permission`, as the pickers list them. The first entry of each list is "what the config says".
pub const PERMISSIONS: [&str; 4] = ["edits", "ask", "agent", "auto"];
/// `--thinking`.
pub const EFFORTS: [&str; 6] = ["off", "low", "medium", "high", "xhigh", "max"];

/// The side panels' widths: what they open at, and how narrow and how wide a drag of their edge
/// makes them.
pub const SIDE_W: f32 = 244.0;
pub const INSP_W: f32 = 380.0;
pub(crate) const SIDE_MIN: f32 = 180.0;
pub(crate) const SIDE_MAX: f32 = 480.0;
pub(crate) const INSP_MIN: f32 = 280.0;
pub(crate) const INSP_MAX: f32 = 760.0;
/// The room the conversation keeps between the two side panels. Where the window has less, the
/// list of conversations gives way while the inspector is open: at the widths they open at, under
/// 1180 wide.
const ROOM_BOTH: f32 = 554.0;
/// The room it keeps next to one panel: the panel is drawn narrower where the window has less.
const ROOM_ALONE: f32 = 440.0;
/// A panel's edge dragged this far past its narrowest folds the panel away.
const FOLD_PAST: f32 = 60.0;
/// How long the window's size is left alone before it is kept: a resize is many sizes in a row.
const RESIZE_REST: Duration = Duration::from_millis(500);

/// The inner edge of a side panel, which a drag moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Side,
    Inspector,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub program: PathBuf,
    pub cwd: PathBuf,
    /// The conversation to open with, named as `sasacode -r` takes it.
    pub session: Option<String>,
    /// How the window opens laid out, and the file it keeps how it is laid out in, if any.
    pub layout: Layout,
    pub layout_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Changes,
    Plan,
    Commands,
    Usage,
}

/// What a copy button copies. Text the window holds is named rather than carried, so a button
/// costs nothing to draw; what is copied is the source, not the part of it that is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clip {
    /// The reply of an exchange, as the Markdown it was written in.
    Reply(usize),
    /// A step of an exchange: an error, or text written between calls.
    Step(usize, usize),
    /// A call's command, by call id.
    Command(String),
    /// All a call wrote, by call id.
    Output(String),
    /// The conversation's id, which `sasacode -r` takes.
    Session,
    /// Text drawn whole where it is copied from: a code block.
    Text(String),
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
    /// The conversation shown.
    pub(crate) chat: Chat,
    /// Conversations not shown that are still wanted: one that runs, or has something typed.
    pub(crate) parked: Vec<Chat>,
    /// Conversations made, so that each one's runs can be told apart.
    chats: u64,
    pub(crate) model: String,
    pub(crate) permission: Option<&'static str>,
    pub(crate) effort: Option<&'static str>,
    pub(crate) filter: String,
    pub(crate) side: bool,
    pub(crate) insp: bool,
    /// The widths the side panels were given, drawn narrower only where the window has not the
    /// room.
    pub(crate) side_w: f32,
    pub(crate) insp_w: f32,
    /// The edge being dragged.
    pub(crate) drag: Option<Edge>,
    /// The dragged panel's width when its edge was taken: what it opens at again when the drag
    /// folds it, rather than the narrowest it passed through on the way.
    held: f32,
    /// The edge under the pointer.
    pub(crate) edge_hover: Option<Edge>,
    pub(crate) tab: Tab,
    pub(crate) now: Instant,
    pub(crate) width: f32,
    pub(crate) height: f32,
    /// Where how the window is laid out is kept, and what was last read from it or written to it.
    layout_file: Option<PathBuf>,
    kept: Layout,
    /// Sizes the window was given, so that only the last one's rest keeps it.
    resizes: u64,
    /// What was copied last, while its button says so.
    pub(crate) copied: Option<Clip>,
    /// Copies made, so that only the last one's time running out clears `copied`.
    copies: u64,
    /// The reader scrolled back from the newest: the conversation is held by its start, so what
    /// comes in does not move what is read. Otherwise it is held by its end and follows.
    pub(crate) reading: bool,
    /// A scroll the window asked for has not been made yet: what the conversation reports until
    /// then was measured from the end it was held by before.
    settling: bool,
}

/// One conversation: what it says, what is typed for it, and its run.
pub(crate) struct Chat {
    /// Which conversation a run's messages are for.
    pub(crate) key: u64,
    /// The session the next prompt continues; `None` starts a new one.
    pub(crate) session: Option<String>,
    /// The conversation shown was read from its log, and nothing ran here since.
    pub(crate) loaded: bool,
    pub(crate) transcript: Transcript,
    pub(crate) editor: text_editor::Content,
    run: Option<Run>,
    /// Sent as soon as the run that is ending is gone.
    pub(crate) next: Option<String>,
    /// Exchanges whose steps were opened or closed by hand.
    pub(crate) folds: HashMap<usize, bool>,
    /// Rows opened by hand: (exchange, step).
    pub(crate) open: HashSet<(usize, usize)>,
    /// Cards of the inspector closed by hand, by call id.
    pub(crate) closed: HashSet<String>,
    /// Something happened while it was not shown: its run ended, or it waits for an answer.
    pub(crate) unread: bool,
}

impl Chat {
    pub(crate) fn new(key: u64) -> Self {
        Chat {
            key,
            session: None,
            loaded: false,
            transcript: Transcript::default(),
            editor: text_editor::Content::new(),
            run: None,
            next: None,
            folds: HashMap::new(),
            open: HashSet::new(),
            closed: HashSet::new(),
            unread: false,
        }
    }

    /// The agent works: its run has started and has not said it ended.
    pub(crate) fn running(&self) -> bool {
        self.run.as_ref().is_some_and(|r| !r.ended)
    }

    /// A run's process is still there, working or closing.
    pub(crate) fn busy(&self) -> bool {
        self.run.is_some()
    }

    /// Worth keeping when another conversation is shown: it runs, has something typed or waiting
    /// to be sent, or has news not seen yet. Anything else is read back from its log when it is
    /// opened again.
    fn wanted(&self) -> bool {
        self.busy() || self.unread || self.next.is_some() || !self.editor.text().trim().is_empty()
    }

    /// A new conversation nothing was sent in yet.
    fn untouched(&self) -> bool {
        self.session.is_none() && !self.busy() && self.transcript.is_empty()
    }

    fn control(&self, message: ControlMessage) {
        if let Some(tx) = self.run.as_ref().and_then(|r| r.control.as_ref()) {
            let _ = tx.send(message);
        }
    }
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
    /// What a conversation's run said, by the conversation's key.
    Run(u64, RunUpdate),
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
    Copy(Clip),
    /// A copy's time to say so ran out: the copy it is, by count.
    Copied(u64),
    /// The conversation was scrolled: how far from the end it is held by, and how far it can go.
    Scrolled {
        offset: f32,
        room: f32,
    },
    /// The scroll the window asked for was made.
    Settled,
    /// Back to the newest of the conversation.
    Latest,
    /// The keyboard to the search of the conversation list.
    Find,
    /// Enter in the search: the first conversation it finds.
    FindOpen,
    /// The conversation above the one shown in the list (-1), or below it (1).
    Neighbour(i8),
    /// The keyboard back to the input.
    FocusInput,
    /// A panel's edge was pressed, to be dragged.
    Grab(Edge),
    /// Where the pointer is while an edge is dragged: how far from the window's left.
    Drag(f32),
    /// The dragged edge was let go.
    Drop,
    /// A panel's edge was double-clicked: the panel back to the width it opened at.
    ResetWidth(Edge),
    /// The pointer came over a panel's edge, or left it.
    HoverEdge(Option<Edge>),
    ThemeMode(theme::Mode),
    Tick(Instant),
    Resized(Size),
    /// The window's size was left alone for a while since the size it is, by count.
    Rested(u64),
    /// The size of the screen the window opened on.
    Screen(window::Id, Option<Size>),
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
        let screen = window::oldest().and_then(|id| window::monitor_size(id).map(move |size| Msg::Screen(id, size)));
        (
            app,
            Task::batch([iced::system::theme().map(Msg::ThemeMode), focus_input(), screen]),
        )
    }

    /// A window with no conversation and no list of sessions.
    pub(crate) fn blank(options: Options, s: Strings) -> Self {
        let layout = options.layout;
        App {
            s,
            t: LIGHT,
            theme: LIGHT.theme(),
            program: options.program,
            cwd: options.cwd,
            home: sasacode_protocol::sasacode_home(),
            sessions: Vec::new(),
            chat: Chat::new(0),
            parked: Vec::new(),
            chats: 0,
            model: String::new(),
            permission: None,
            effort: None,
            filter: String::new(),
            side: true,
            insp: true,
            side_w: layout.side,
            insp_w: layout.inspector,
            drag: None,
            held: layout.side,
            edge_hover: None,
            tab: Tab::Changes,
            now: Instant::now(),
            width: layout.size.width,
            height: layout.size.height,
            layout_file: options.layout_file,
            kept: layout,
            resizes: 0,
            copied: None,
            copies: 0,
            reading: false,
            settling: false,
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
        // A drag follows the pointer, and ends where the button is let go, outside the window too.
        if self.drag.is_some() {
            all.push(event::listen_with(dragging));
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
        self.chat.run = Some(Run {
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

    /// The conversation shown is worked on: its run has started and has not said it ended.
    pub(crate) fn running(&self) -> bool {
        self.chat.running()
    }

    /// The conversation shown has a run's process, working or closing.
    pub(crate) fn busy(&self) -> bool {
        self.chat.busy()
    }

    pub(crate) fn status(&self) -> Status {
        if self.running() {
            return if self.chat.transcript.pending_approval().is_some() {
                Status::Waiting
            } else {
                Status::Running
            };
        }
        if self.chat.loaded {
            return Status::Loaded;
        }
        match self
            .chat
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
        self.side && (!self.insp || self.both_fit())
    }

    /// The window has room for both side panels at their widths and the conversation between.
    fn both_fit(&self) -> bool {
        self.width - self.side_w - self.insp_w - 2.0 >= ROOM_BOTH
    }

    /// How wide the list of conversations is drawn: as it was given, less where a narrow window
    /// keeps the conversation's room.
    pub(crate) fn side_width(&self) -> f32 {
        self.side_w.min(self.width - 1.0 - ROOM_ALONE).max(SIDE_MIN)
    }

    /// How wide the inspector is drawn, likewise.
    pub(crate) fn insp_width(&self) -> f32 {
        self.insp_w.min(self.width - 1.0 - ROOM_ALONE).max(INSP_MIN)
    }

    /// The dragged edge follows the pointer at `x`, as far as the panel's bounds and the
    /// conversation's room let it. Dragged far enough past its narrowest, the panel folds away,
    /// keeping the width it had when the edge was taken; back out in the same drag, it opens again.
    fn drag_to(&mut self, x: f32) {
        match self.drag {
            Some(Edge::Side) => {
                let want = x;
                self.side = want >= SIDE_MIN - FOLD_PAST;
                if !self.side {
                    self.side_w = self.held;
                } else {
                    // Next to the inspector both keep fitting; alone, the conversation its room.
                    let most = if self.insp {
                        self.width - self.insp_w - 2.0 - ROOM_BOTH
                    } else {
                        self.width - 1.0 - ROOM_ALONE
                    };
                    self.side_w = want.min(most.min(SIDE_MAX)).max(SIDE_MIN);
                }
            }
            Some(Edge::Inspector) => {
                let want = self.width - x - 1.0;
                self.insp = want >= INSP_MIN - FOLD_PAST;
                if !self.insp {
                    self.insp_w = self.held;
                } else {
                    let most = if self.shows_side() {
                        self.width - self.side_w - 2.0 - ROOM_BOTH
                    } else {
                        self.width - 1.0 - ROOM_ALONE
                    };
                    self.insp_w = want.min(most.min(INSP_MAX)).max(INSP_MIN);
                }
            }
            None => {}
        }
    }

    fn control(&self, message: ControlMessage) {
        self.chat.control(message);
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Edit(action) => self.chat.editor.perform(action),
            Msg::Send => return self.send(),
            Msg::Chord => {
                let empty = self.chat.editor.text().trim().is_empty();
                if self.chat.transcript.pending_approval().is_some() {
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
                    return self.start(self.chat.key, self.s.continue_text.to_owned());
                }
            }
            Msg::Run(key, update) => return self.on_run(key, update),
            Msg::Answer(decision) => return self.answer(decision),
            Msg::NewSession => {
                if !self.chat.untouched() {
                    // What was typed for a new conversation before is still there.
                    let chat = match self.parked.iter().position(Chat::untouched) {
                        Some(i) => self.parked.remove(i),
                        None => self.new_chat(),
                    };
                    self.bring(chat);
                }
                return Task::batch([focus_input(), self.back_to_newest()]);
            }
            Msg::OpenSession(path) => {
                if self.open_session(&path) {
                    return Task::batch([focus_input(), self.back_to_newest()]);
                }
            }
            Msg::Neighbour(step) => {
                let paths: Vec<PathBuf> = self.found().map(|s| s.path.clone()).collect();
                let here = self.chat.session.as_deref();
                let at = self.found().position(|s| Some(s.id.as_str()) == here);
                let to = match (at, step < 0) {
                    (Some(i), true) => i.checked_sub(1),
                    (Some(i), false) => Some(i + 1),
                    // A new conversation is above the list.
                    (None, true) => None,
                    (None, false) => Some(0),
                };
                if let Some(path) = to.and_then(|i| paths.get(i))
                    && self.open_session(&path.clone())
                {
                    return Task::batch([focus_input(), self.back_to_newest()]);
                }
            }
            Msg::Filter(text) => self.filter = text,
            Msg::Model(model) => self.model = model,
            Msg::Permission(p) => self.permission = PERMISSIONS.contains(&p).then_some(p),
            Msg::Effort(e) => self.effort = EFFORTS.contains(&e).then_some(e),
            Msg::ToggleSide => {
                self.side = !self.shows_side();
                // In a narrow window the two side panels take turns.
                if self.side && !self.both_fit() {
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
                self.chat.folds.insert(i, !open);
            }
            Msg::Open(i, j) => {
                if !self.chat.open.remove(&(i, j)) {
                    self.chat.open.insert((i, j));
                }
            }
            Msg::Card(id) => {
                if !self.chat.closed.remove(&id) {
                    self.chat.closed.insert(id);
                }
            }
            Msg::Link(uri) => open_link(&uri),
            Msg::Copy(clip) => {
                if let Some(text) = self.copy_text(&clip) {
                    self.copies += 1;
                    self.copied = Some(clip);
                    let n = self.copies;
                    return Task::batch([
                        iced::clipboard::write(text),
                        // The timer is made when the task runs, where its runtime is.
                        Task::perform(async { tokio::time::sleep(COPIED_FOR).await }, move |()| Msg::Copied(n)),
                    ]);
                }
            }
            Msg::Copied(n) => {
                if n == self.copies {
                    self.copied = None;
                }
            }
            Msg::Scrolled { offset, room } => {
                if !self.settling {
                    return self.follow(offset, room);
                }
            }
            Msg::Settled => self.settling = false,
            Msg::Latest => return self.back_to_newest(),
            Msg::Find => {
                if !self.shows_side() {
                    self.side = true;
                    if !self.both_fit() {
                        self.insp = false;
                    }
                }
                return Task::batch([operation::focus(SEARCH), operation::select_all(SEARCH)]);
            }
            Msg::FindOpen => {
                let current = self.chat.session.as_deref();
                let first = self.found().next().filter(|s| Some(s.id.as_str()) != current);
                if let Some(path) = first.map(|s| s.path.clone())
                    && self.open_session(&path)
                {
                    return Task::batch([focus_input(), self.back_to_newest()]);
                }
                return focus_input();
            }
            Msg::FocusInput => return focus_input(),
            Msg::Grab(edge) => {
                self.drag = Some(edge);
                self.held = match edge {
                    Edge::Side => self.side_w,
                    Edge::Inspector => self.insp_w,
                };
            }
            Msg::Drag(x) => self.drag_to(x),
            Msg::Drop => {
                if self.drag.take().is_some() {
                    self.keep_layout();
                }
            }
            Msg::ResetWidth(edge) => {
                // The second press of a double click grabbed the edge too: what it does is this.
                self.drag = None;
                match edge {
                    Edge::Side => self.side_w = SIDE_W,
                    Edge::Inspector => self.insp_w = INSP_W,
                }
                self.keep_layout();
            }
            Msg::HoverEdge(edge) => self.edge_hover = edge,
            Msg::ThemeMode(mode) => self.set_dark(mode == theme::Mode::Dark),
            Msg::Tick(now) => self.now = now,
            Msg::Resized(size) => {
                self.width = size.width;
                self.height = size.height;
                self.resizes += 1;
                let n = self.resizes;
                return Task::perform(async { tokio::time::sleep(RESIZE_REST).await }, move |()| {
                    Msg::Rested(n)
                });
            }
            Msg::Rested(n) => {
                if n == self.resizes {
                    self.keep_layout();
                }
            }
            Msg::Screen(id, Some(screen)) => {
                if let Some(size) = layout::fit(Size::new(self.width, self.height), screen) {
                    return window::resize(id, size);
                }
            }
            Msg::Screen(_, None) => {}
        }
        Task::none()
    }

    /// How the window is laid out now: the widths the panels were given, folded or not.
    pub(crate) fn layout(&self) -> Layout {
        Layout {
            size: Size::new(self.width, self.height),
            side: self.side_w,
            inspector: self.insp_w,
        }
    }

    /// Keeps how the window is laid out for the next launch, when it changed since it was read or
    /// last written.
    fn keep_layout(&mut self) {
        let Some(file) = &self.layout_file else {
            return;
        };
        let now = self.layout();
        if now == self.kept {
            return;
        }
        match now.write(file) {
            Ok(()) => self.kept = now,
            Err(e) => eprintln!(
                "sasacode-gui: could not keep the window's layout in {}: {e}",
                file.display()
            ),
        }
    }

    /// Shows the conversation a log holds, to be continued by the next message: as it was left
    /// when it was kept, running or typed in, else as its log has it. False when it is the one
    /// shown, or its log cannot be read.
    fn open_session(&mut self, path: &Path) -> bool {
        let id = self.sessions.iter().find(|s| s.path == path).map(|s| s.id.clone());
        if id.is_some() && id == self.chat.session {
            return false;
        }
        if let Some(i) = self.parked.iter().position(|c| id.is_some() && c.session == id) {
            let chat = self.parked.remove(i);
            self.bring(chat);
            return true;
        }
        let Ok(entries) = read_entries(path) else {
            return false;
        };
        let mut chat = self.new_chat();
        chat.session = id;
        chat.transcript = Transcript::from_entries(&entries);
        chat.loaded = true;
        self.bring(chat);
        true
    }

    fn new_chat(&mut self) -> Chat {
        self.chats += 1;
        Chat::new(self.chats)
    }

    /// Shows `chat` in place of the conversation shown, which is kept while it is still wanted.
    fn bring(&mut self, mut chat: Chat) {
        chat.unread = false;
        let left = std::mem::replace(&mut self.chat, chat);
        if left.wanted() {
            self.parked.push(left);
        }
        self.copied = None;
        self.reading = false;
    }

    /// The conversation with `key`, shown or kept.
    fn chat_mut(&mut self, key: u64) -> Option<&mut Chat> {
        if self.chat.key == key {
            return Some(&mut self.chat);
        }
        self.parked.iter_mut().find(|c| c.key == key)
    }

    /// The conversation continuing session `id`, shown or kept.
    pub(crate) fn chat_of(&self, id: &str) -> Option<&Chat> {
        std::iter::once(&self.chat)
            .chain(&self.parked)
            .find(|c| c.session.as_deref() == Some(id))
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

    /// The conversations the search finds, newest first: all of them when it is empty.
    pub(crate) fn found(&self) -> impl Iterator<Item = &SessionSummary> {
        let needle = self.filter.trim().to_lowercase();
        self.sessions
            .iter()
            .filter(move |s| needle.is_empty() || s.first_prompt.to_lowercase().contains(&needle))
    }

    /// The text a copy button copies, as the window holds it now.
    pub(crate) fn copy_text(&self, clip: &Clip) -> Option<String> {
        let markdown = |step: &Step| match step {
            Step::Text(md) => Some(md.raw.trim().to_owned()),
            Step::Note(Note::Error(body)) => Some(body.clone()),
            _ => None,
        };
        let text = match clip {
            Clip::Reply(i) => {
                let ex = self.chat.transcript.exchanges.get(*i)?;
                markdown(ex.steps.get(ex.answer()?)?)
            }
            Clip::Step(i, j) => markdown(self.chat.transcript.exchanges.get(*i)?.steps.get(*j)?),
            Clip::Command(id) => self.chat.transcript.tool(id)?.arg("command").map(str::to_owned),
            Clip::Output(id) => Some(self.chat.transcript.tool(id)?.shown_output().to_owned()),
            Clip::Session => self.chat.session.clone(),
            Clip::Text(text) => Some(text.clone()),
        }?;
        (!text.trim().is_empty()).then_some(text)
    }

    /// Which end holds the conversation, after it was scrolled to `offset` from the end that holds
    /// it now, of `room` it can go. Turns it, at the same place, when the reader crossed into
    /// reading back or came back to the newest.
    fn follow(&mut self, offset: f32, room: f32) -> Task<Msg> {
        let from_end = if self.reading { room - offset } else { offset };
        let reading = from_end > NEAR_END;
        if reading == self.reading {
            return Task::none();
        }
        self.reading = reading;
        // Counted from the end that holds it now: the same place, or the very end.
        self.scroll_timeline(if reading { room - offset } else { 0.0 })
    }

    /// Back to the newest, held by the end again.
    fn back_to_newest(&mut self) -> Task<Msg> {
        self.reading = false;
        self.scroll_timeline(0.0)
    }

    /// Scrolls the conversation to `y` from the end that holds it. What it reports until that is
    /// done is not listened to.
    fn scroll_timeline(&mut self, y: f32) -> Task<Msg> {
        self.settling = true;
        operation::scroll_to(TIMELINE, AbsoluteOffset { x: 0.0, y }).chain(Task::done(Msg::Settled))
    }

    /// Whether the steps of an exchange are shown. While it runs, and when it stopped short with no
    /// reply to read instead, they are; a finished one folds them away.
    pub(crate) fn steps_open(&self, i: usize) -> bool {
        if let Some(open) = self.chat.folds.get(&i) {
            return *open;
        }
        let Some(ex) = self.chat.transcript.exchanges.get(i) else {
            return false;
        };
        let last = i + 1 == self.chat.transcript.exchanges.len();
        let short = ex.end.as_ref().is_some_and(|end| end.cause != StopCause::Done);
        (last && self.running()) || (short && ex.answer().is_none())
    }

    fn take_input(&mut self) -> Option<String> {
        let text = self.chat.editor.text().trim().to_owned();
        if text.is_empty() {
            return None;
        }
        self.chat.editor = text_editor::Content::new();
        Some(text)
    }

    /// Keeps the input for when the run that is ending is gone.
    fn queue_next(&mut self) {
        if let Some(text) = self.take_input() {
            self.chat.next = Some(match self.chat.next.take() {
                Some(before) => format!("{before}\n\n{text}"),
                None => text,
            });
        }
    }

    fn send(&mut self) -> Task<Msg> {
        // An approval waits: a reply denies it and tells the model why.
        if let Some(approval) = self.chat.transcript.pending_approval() {
            let id = approval.id.clone();
            let Some(text) = self.take_input() else {
                return Task::none();
            };
            self.control(ControlMessage::Approval {
                id: id.clone(),
                decision: Decision::Deny,
                feedback: Some(text.clone()),
            });
            self.chat.transcript.answer_approval(&id, Answer::Denied(text));
            return Task::none();
        }
        if self.running() {
            // Like typing while the TUI is busy: it reaches the model at its next turn.
            if let Some(text) = self.take_input() {
                self.chat.transcript.queue(&text);
                self.control(ControlMessage::Message { text });
                return self.back_to_newest();
            }
            return Task::none();
        }
        if self.busy() {
            self.queue_next();
            return Task::none();
        }
        match self.take_input() {
            Some(text) => self.start(self.chat.key, text),
            None => Task::none(),
        }
    }

    fn answer(&mut self, decision: Decision) -> Task<Msg> {
        if let Some(approval) = self.chat.transcript.pending_approval() {
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
            self.chat.transcript.answer_approval(&id, answer);
        }
        Task::none()
    }

    /// Starts a run of conversation `key` on `prompt`.
    fn start(&mut self, key: u64, prompt: String) -> Task<Msg> {
        let now = Instant::now();
        let model = self.model.trim();
        let mut spec = RunSpec {
            program: self.program.clone(),
            cwd: self.cwd.clone(),
            prompt: String::new(),
            resume: None,
            model: (!model.is_empty()).then(|| model.to_owned()),
            permission: self.permission.map(str::to_owned),
            thinking: self.effort.map(str::to_owned),
        };
        let shown = key == self.chat.key;
        let Some(chat) = self.chat_mut(key) else {
            return Task::none();
        };
        chat.transcript.send(&prompt, now);
        chat.loaded = false;
        spec.prompt = prompt;
        spec.resume = chat.session.clone();
        let (task, handle) = Task::run(runner::run(spec), move |update| Msg::Run(key, update)).abortable();
        chat.run = Some(Run {
            control: None,
            _handle: Some(handle.abort_on_drop()),
            stderr: Vec::new(),
            ended: false,
        });
        self.now = now;
        if !shown {
            return task;
        }
        // What was just sent is in view, however far back the reader was.
        Task::batch([task, self.back_to_newest()])
    }

    /// What the run of conversation `key` said. A conversation not shown is marked for what the
    /// user should come back to: its run ended, or it waits for an answer.
    fn on_run(&mut self, key: u64, update: RunUpdate) -> Task<Msg> {
        let shown = key == self.chat.key;
        let now = Instant::now();
        let s = self.s;
        let Some(chat) = self.chat_mut(key) else {
            return Task::none();
        };
        let Some(run) = chat.run.as_mut() else {
            return Task::none();
        };
        let mut attention = false;
        let mut relist = false;
        let mut next = None;
        match update {
            RunUpdate::Started(control) => run.control = Some(control),
            RunUpdate::Event(event) => {
                let effect = chat.transcript.apply(&event, now);
                if let Some(id) = effect.session_id {
                    chat.session = Some(id);
                }
                if effect.ended.is_some() {
                    run.ended = true;
                    chat.unread |= !shown;
                }
                if effect.approval.is_some() {
                    chat.unread |= !shown;
                    attention = true;
                }
                // A new conversation's log is there once its first message is: the list shows it.
                relist = matches!(*event, sasacode_protocol::Event::MessageEnd { .. }) && chat.session.is_some();
            }
            RunUpdate::Stderr(line) => {
                run.stderr.push(line);
                if run.stderr.len() > 40 {
                    run.stderr.remove(0);
                }
            }
            RunUpdate::Failed(error) => {
                chat.run = None;
                chat.unread |= !shown;
                chat.transcript.close(now);
                chat.transcript.note(Note::Error(format!("{}\n{error}", s.not_found)));
            }
            RunUpdate::Exited(code) => {
                // A run that never got to its end says why only on stderr.
                if !run.ended {
                    let code = code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
                    let mut text = fill(s.exited, "code", code);
                    let tail = run.stderr.join("\n");
                    if !tail.trim().is_empty() {
                        text = format!("{text}\n{tail}");
                    }
                    chat.transcript.note(Note::Error(text));
                }
                chat.run = None;
                chat.unread |= !shown;
                chat.transcript.close(now);
                // What was sent while it ran and never reached the model, and what waited for it
                // to end, start the next run.
                let mut waiting = std::mem::take(&mut chat.transcript.queued);
                waiting.extend(chat.next.take());
                if !waiting.is_empty() {
                    next = Some(waiting.join("\n\n"));
                }
                self.reload_sessions();
            }
        }
        if relist
            && let Some(id) = self.chat_mut(key).and_then(|c| c.session.clone())
            && !self.sessions.iter().any(|s| s.id == id)
        {
            self.reload_sessions();
        }
        self.now = now;
        if let Some(prompt) = next {
            return self.start(key, prompt);
        }
        if attention {
            // The run waits for the user: say so even when the window is behind others.
            return window::latest()
                .and_then(|id| window::request_user_attention(id, Some(window::UserAttention::Informational)));
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
        // A field that let go of the keyboard on Esc, the search or the model, gives it back to
        // the input: the next Esc is the input's again.
        return matches!(key, Key::Named(Named::Escape)).then_some(Msg::FocusInput);
    }
    shortcut(&key, modifiers)
}

/// While an edge is dragged: where the pointer goes, and the left button let go, wherever the
/// pointer is. Out of the window the panel folds, or takes all the room it may.
pub(crate) fn dragging(event: iced::Event, _status: event::Status, _window: window::Id) -> Option<Msg> {
    use iced::mouse::{Button, Event as Mouse};
    match event {
        iced::Event::Mouse(Mouse::CursorMoved { position }) => Some(Msg::Drag(position.x)),
        iced::Event::Mouse(Mouse::ButtonReleased(Button::Left)) => Some(Msg::Drop),
        _ => None,
    }
}

/// The window's shortcuts. None of them is a bare letter or a bare Enter: a key meant for the
/// input must never answer an approval.
pub(crate) fn shortcut(key: &Key, modifiers: keyboard::Modifiers) -> Option<Msg> {
    if matches!(key, Key::Named(Named::Escape)) {
        return Some(Msg::Stop);
    }
    if !modifiers.command() {
        return None;
    }
    match key.as_ref() {
        Key::Named(Named::Enter) => Some(Msg::Chord),
        Key::Character("b") => Some(Msg::ToggleSide),
        Key::Character("i") => Some(Msg::ToggleInspector),
        Key::Character("n") => Some(Msg::NewSession),
        // ⌘⇧[ and ⌘⇧]: the conversation above or below in the list.
        Key::Character("[") if modifiers.shift() => Some(Msg::Neighbour(-1)),
        Key::Character("]") if modifiers.shift() => Some(Msg::Neighbour(1)),
        Key::Character("k") => Some(Msg::Find),
        Key::Character("l") => Some(Msg::FocusInput),
        Key::Character("1") => Some(Msg::Tab(Tab::Changes)),
        Key::Character("2") => Some(Msg::Tab(Tab::Plan)),
        Key::Character("3") => Some(Msg::Tab(Tab::Commands)),
        Key::Character("4") => Some(Msg::Tab(Tab::Usage)),
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
            layout: Layout::default(),
            layout_file: None,
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
        assert_eq!(app.chat.session.as_deref(), Some("9d000000"));
        assert!(app.chat.loaded && !app.chat.transcript.is_empty());
        // The start of an id: the newest of those it starts. The next message continues it by
        // its whole id.
        assert!(app.open_named("17ca"));
        assert_eq!(app.chat.session.as_deref(), Some("17cab000"));
        // Its log.
        let log = app.sessions.iter().find(|s| s.id == "17cae3ae").unwrap().path.clone();
        assert!(app.open_named(&log.to_string_lossy()));
        assert_eq!(app.chat.session.as_deref(), Some("17cae3ae"));

        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn enter_in_the_search_opens_the_newest_it_finds() {
        let (mut app, home) = window_with("find", &["17cab000", "9d000000"]);
        app.filter = "見当たらない".into();
        let _ = app.update(Msg::FindOpen);
        assert!(app.chat.session.is_none() && app.chat.transcript.is_empty());
        app.filter = "指数バックオフ".into();
        let _ = app.update(Msg::FindOpen);
        assert_eq!(app.chat.session.as_deref(), Some("17cab000"));
        assert!(app.chat.loaded && !app.chat.transcript.is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }

    fn event(line: &str) -> RunUpdate {
        RunUpdate::Event(Box::new(sasacode_protocol::parse_event(line).unwrap()))
    }

    fn type_in(app: &mut App, text: &str) {
        app.chat.editor = text_editor::Content::with_text(text);
    }

    #[test]
    fn a_conversation_keeps_running_while_another_is_shown() {
        let (mut app, home) = window_with("parallel", &["aaaa0000", "bbbb0000", "cccc0000"]);
        let path = |app: &App, id: &str| app.sessions.iter().find(|s| s.id == id).unwrap().path.clone();

        // One runs; another is opened, and what runs is kept.
        let _ = app.update(Msg::OpenSession(path(&app, "aaaa0000")));
        let mut written = app.pretend_running();
        let a = app.chat.key;
        let _ = app.update(Msg::OpenSession(path(&app, "bbbb0000")));
        assert_eq!(app.chat.session.as_deref(), Some("bbbb0000"));
        assert!(!app.running());
        assert!(app.chat_of("aaaa0000").is_some_and(Chat::running));

        // What it says reaches it, not the conversation shown: it waits for an answer, and then
        // it ends. Each time it is marked for the user to come back to.
        let shown = app.chat.transcript.exchanges.len();
        let _ = app.update(Msg::Run(a, event(r#"{"type":"agent_start"}"#)));
        let _ = app.update(Msg::Run(
            a,
            event(r#"{"type":"approval_request","id":"ap-1","callId":"c1","tool":"bash","args":{"command":"ls"},"reason":"ask","cwd":"/tmp"}"#),
        ));
        let waiting = app.chat_of("aaaa0000").unwrap();
        assert!(waiting.unread && waiting.transcript.pending_approval().is_some());
        assert_eq!(app.chat.transcript.exchanges.len(), shown);
        assert!(app.chat.transcript.pending_approval().is_none());

        // Opened again, it is as it was left, its mark cleared; the answer reaches its run.
        let _ = app.update(Msg::OpenSession(path(&app, "aaaa0000")));
        assert_eq!(app.chat.key, a);
        assert!(!app.chat.unread && app.running());
        let _ = app.update(Msg::Answer(Decision::Allow));
        assert!(matches!(
            written.try_recv(),
            Ok(ControlMessage::Approval {
                decision: Decision::Allow,
                ..
            })
        ));

        // Back on the other, it ends: the list marks it until it is seen.
        let _ = app.update(Msg::Neighbour(1));
        assert_eq!(app.chat.session.as_deref(), Some("bbbb0000"));
        let _ = app.update(Msg::Run(a, event(r#"{"type":"agent_end","cause":"done"}"#)));
        let _ = app.update(Msg::Run(a, RunUpdate::Exited(Some(0))));
        let ended = app.chat_of("aaaa0000").unwrap();
        assert!(ended.unread && !ended.busy());
        // A conversation with nothing going on is not kept: it is read from its log again.
        let _ = app.update(Msg::Neighbour(1));
        assert_eq!(app.chat.session.as_deref(), Some("cccc0000"));
        assert!(app.chat_of("bbbb0000").is_none());
        let _ = app.update(Msg::Neighbour(-1));
        let _ = app.update(Msg::Neighbour(-1));
        assert_eq!(app.chat.session.as_deref(), Some("aaaa0000"));
        assert!(!app.chat.unread && app.parked.is_empty());
        // The top of the list goes no further up.
        let _ = app.update(Msg::Neighbour(-1));
        assert_eq!(app.chat.session.as_deref(), Some("aaaa0000"));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn what_is_typed_stays_with_its_conversation() {
        let (mut app, home) = window_with("drafts", &["aaaa0000", "bbbb0000"]);
        let path = |app: &App, id: &str| app.sessions.iter().find(|s| s.id == id).unwrap().path.clone();

        // Typed in a new conversation, then another opened and typed in.
        type_in(&mut app, "新しい会話の下書き");
        let _ = app.update(Msg::OpenSession(path(&app, "aaaa0000")));
        assert_eq!(app.chat.editor.text().trim(), "");
        type_in(&mut app, "続きの下書き");
        let _ = app.update(Msg::OpenSession(path(&app, "bbbb0000")));

        // Each comes back with what was typed for it.
        let _ = app.update(Msg::NewSession);
        assert!(app.chat.session.is_none());
        assert_eq!(app.chat.editor.text().trim(), "新しい会話の下書き");
        let _ = app.update(Msg::FindOpen);
        let _ = app.update(Msg::OpenSession(path(&app, "aaaa0000")));
        assert_eq!(app.chat.editor.text().trim(), "続きの下書き");
        // ⌘N on a conversation that is already new stays on it.
        let _ = app.update(Msg::NewSession);
        let key = app.chat.key;
        let _ = app.update(Msg::NewSession);
        assert_eq!(app.chat.key, key);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_conversation_that_is_not_there_leaves_a_new_one() {
        let (mut app, home) = window_with("missing", &["17cae3ae"]);
        assert!(!app.open_named("0000"));
        assert!(app.chat.session.is_none() && !app.chat.loaded && app.chat.transcript.is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }
}
