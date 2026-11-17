//! The window's state and what changes it. Drawing is in `view`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::keyboard::{self, Key, key::Named};
use iced::widget::operation::{self, AbsoluteOffset};
use iced::widget::{markdown, text_editor};
use iced::{Element, Point, Size, Subscription, Task, Theme, event, task, theme, window};
use sasacode_protocol::{ControlMessage, Decision, SessionSummary, StopCause, list_sessions, read_entries};
use tokio::sync::mpsc::UnboundedSender;

use crate::i18n::{Strings, fill};
use crate::input::Command;
use crate::layout::{self, Layout, Prefs};
pub(crate) use crate::overlay::Overlay;
pub(crate) use crate::overlay::SIDE;
use crate::runner::{self, RunSpec, RunUpdate};
use crate::system::Awake;
use crate::theme::{DARK, LIGHT, Tokens};
use crate::transcript::{Answer, Note, Step, Transcript};

/// The input's name among the widgets: what the keyboard is given to when the window opens and
/// when a conversation is opened.
pub const INPUT: &str = "input";
/// The search of the conversation list.
pub const SEARCH: &str = "search";
/// The conversation, scrolled back to its newest.
pub const TIMELINE: &str = "timeline";
/// The field of the command palette, and of a conversation's new name.
pub const PALETTE: &str = "palette";
pub const RENAME: &str = "rename";
/// The field of a conversation's group.
pub const GROUP: &str = "group";
/// The title of the pull request being written.
pub const PR_TITLE: &str = "pr-title";
/// The search inside the conversation shown.
pub const FIND_IN: &str = "find-in";
/// The model's field.
pub const MODEL: &str = "model";

/// How far from its newest the conversation still counts as at it, in pixels: a nudge of the
/// trackpad is not reading back.
const NEAR_END: f32 = 24.0;
/// The stderr lines the window keeps for its log.
pub const LOG_LINES: usize = 400;
/// How long a copy button says it copied.
const COPIED_FOR: Duration = Duration::from_millis(1600);

/// `--permission`, as the pickers list them. The first entry of each list is "what the config says".
/// How often a pull request's running checks are looked at again.
const CHECKS_EVERY: Duration = Duration::from_secs(45);

/// The permission modes, as the CLI's, and plan mode, the window's: a run that reads and plans,
/// asked under `ask` with reads allowed, whose writes and edits are denied ([`crate::plan`]).
pub const PERMISSIONS: [&str; 5] = ["edits", "ask", "agent", "auto", "plan"];
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
/// How often conversations whose pull request was merged or closed are looked for, while the
/// settings say to archive them.
const SWEEP_EVERY: Duration = Duration::from_secs(600);
/// How long the window's size is left alone before it is kept: a resize is many sizes in a row.
const RESIZE_REST: Duration = Duration::from_millis(500);
/// Where a window kept on a screen that is gone is put: near the main screen's top left corner.
const ON_SCREEN: Point = Point::new(80.0, 80.0);

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
    /// How the window opens laid out and what was chosen in its settings, and the file it keeps
    /// them in, if any.
    pub layout: Layout,
    pub prefs: Prefs,
    pub layout_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Changes,
    Plan,
    Commands,
    Usage,
    Git,
    Files,
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
    /// The folder's path, and the conversation's log's.
    Folder,
    Log,
    /// The conversation's id, which `sasacode -r` takes.
    Session,
    /// A link that opens the folder's window on the conversation: `sasacode://open?…`.
    Link,
    /// The whole conversation, as Markdown.
    Conversation,
    /// The versions, the paths and the recent log, for a report of something wrong.
    About,
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
    pub(crate) program: PathBuf,
    pub(crate) cwd: PathBuf,
    pub(crate) home: Option<PathBuf>,
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
    /// Where the window's top left corner is, as the system measures it, once it has been moved or
    /// was kept from the last launch.
    pub(crate) at: Option<Point>,
    /// Where how the window is laid out is kept, and what was last read from it or written to it.
    pub(crate) layout_file: Option<PathBuf>,
    kept: Layout,
    /// What was chosen in the settings, and what was last kept of it.
    pub(crate) prefs: Prefs,
    kept_prefs: Prefs,
    /// The system's light or dark, which the window follows unless told otherwise.
    system_dark: bool,
    /// The window is in front: nothing needs the system to say what happened.
    pub(crate) focused: bool,
    /// What is drawn over the window: the palette, the keys, the settings, a name being given.
    pub(crate) overlay: Option<Overlay>,
    /// What was sent before, oldest first, and the file it is shared with the TUI in.
    pub(crate) history: Vec<String>,
    pub(crate) history_file: Option<PathBuf>,
    /// Which of it the input shows, while ↑ and ↓ go through it, and what was typed before.
    pub(crate) recall: Option<usize>,
    pub(crate) recall_draft: String,
    /// The project's files, for `@`, once asked for.
    pub(crate) files: Vec<String>,
    pub(crate) files_asked: bool,
    /// Which of what the input offers is chosen, and whether the offer was put away.
    pub(crate) pick: usize,
    pub(crate) dismissed: bool,
    /// Files are held over the window, to be dropped into the input.
    pub(crate) dropping: bool,
    /// Holds the computer awake while something runs.
    pub(crate) awake: Option<Awake>,
    /// The side question and its answer, while they are shown.
    pub(crate) aside: Option<Chat>,
    /// A word from the window that is not about a conversation, until the next one.
    pub(crate) toast: Option<String>,
    /// The search inside the conversation shown, while it is open.
    pub(crate) find_in: Option<String>,
    /// Which of what the search in the conversation finds is in view: an index into those found.
    pub(crate) find_at: usize,
    /// A reply whose text is being chosen to quote: its exchange, and its text, read only.
    pub(crate) quoting: Option<(usize, text_editor::Content)>,
    /// The approval whose call's arguments are being edited, and their JSON.
    pub(crate) editing_args: Option<(String, text_editor::Content)>,
    /// The slash commands the plugins offered in the newest run.
    pub(crate) plugin_commands: Vec<sasacode_protocol::event::Command>,
    /// What the CLI said a run here can use, and whether the folder is trusted.
    pub(crate) listing: crate::cli::Listing,
    /// The folder's config is trusted for this window's runs only (`--trust-project`).
    pub(crate) trust_once: bool,
    /// The share of the context window where the conversation is summarised, as configured.
    pub(crate) compact_at: Option<f64>,
    /// The picture shown larger is at its own size, not fitted.
    pub(crate) image_actual: bool,
    /// What the runs wrote on stderr, the newest last, as many as `LOG_LINES`.
    pub(crate) log: std::collections::VecDeque<String>,
    /// The models the next message is sent to at once, and one being typed.
    pub(crate) compare: std::collections::BTreeSet<String>,
    pub(crate) compare_typed: String,
    /// What the conversations used, once read: of every folder, or of this one.
    pub(crate) usage: Option<crate::usage::Report>,
    pub(crate) usage_here: bool,
    pub(crate) usage_part: crate::usage::Part,
    /// The worktrees made here, once read for their list.
    pub(crate) worktree_list: Option<Vec<crate::worktrees::Tree>>,
    /// The permission mode before plan mode was chosen.
    pub(crate) before_plan: Option<&'static str>,
    /// The automation being written.
    pub(crate) auto_form: crate::automations::Form,
    /// A gist is being made.
    pub(crate) gisting: bool,
    /// What `sasacode update --check` said, once asked; none while it is asked.
    pub(crate) update_said: Option<Result<String, String>>,
    pub(crate) checking_update: bool,
    /// The other folders sasacode was used in, the most recently used first.
    pub(crate) others: Vec<PathBuf>,
    /// The background jobs, as last read; the one whose output is shown; the one asked to stop.
    pub(crate) jobs: Vec<crate::jobs::Job>,
    pub(crate) job_open: Option<String>,
    pub(crate) job_stopping: Option<String>,
    /// The next side question, as it is typed.
    pub(crate) side_typed: String,
    /// Conversations whose title the model is writing.
    pub(crate) titling: std::collections::BTreeSet<String>,
    /// The conversations left, the newest last, and those gone back from; while one of them is
    /// being opened, nothing is added.
    pub(crate) back: Vec<String>,
    pub(crate) forward: Vec<String>,
    pub(crate) navigating: bool,
    /// Conversations whose notice that the branch changed was put away.
    pub(crate) branch_seen: HashSet<String>,
    /// That search's field was the last to take the keyboard: an Esc there puts it away.
    pub(crate) find_held: bool,
    /// The list shows the conversations taken out of it, instead of the others.
    pub(crate) show_archived: bool,
    /// The conversation of the list under the pointer, which shows what can be done to it.
    pub(crate) row_hover: Option<String>,
    /// The folder's repository, as last looked at, and whether git is at work on it.
    pub(crate) git: crate::git::Status,
    pub(crate) git_busy: bool,
    /// The message of the next commit.
    pub(crate) commit_msg: String,
    /// The file, or the hunk (`path#n` of what is not staged), whose changes a second click
    /// discards.
    pub(crate) confirm_discard: Option<String>,
    /// The pull request a second click merges.
    pub(crate) confirm_merge: bool,
    /// The folder's scripts run from the window, the newest first, and how many were run.
    pub(crate) script_runs: Vec<crate::scripts::ScriptRun>,
    pub(crate) script_runs_made: u64,
    /// What the git tab compares, what it found, and the commits it can pick from.
    pub(crate) git_scope: crate::scope::Scope,
    pub(crate) scoped: Option<Result<crate::git::Scoped, String>>,
    pub(crate) scope_loading: bool,
    pub(crate) scope_commits: Vec<(String, String)>,
    /// A commit or a push on the default branch was asked for once: the next press does it.
    pub(crate) confirm_default: bool,
    /// What the model is being asked, apart from any conversation.
    pub(crate) drafting: Vec<crate::helper::Draft>,
    /// The pull request being written before it is made.
    pub(crate) pr_draft: Option<crate::helper::PrDraft>,
    /// What the user may say next in the conversation of this key, as the model guessed.
    pub(crate) suggestion: Option<(u64, String)>,
    /// The conversation beside the one shown, by id: one of `parked`.
    pub(crate) split: Option<String>,
    /// The menu a right click opened.
    pub(crate) menu: Option<crate::rows::Menu>,
    /// Conversations chosen together in the list, by id.
    pub(crate) selected: std::collections::BTreeSet<String>,
    /// Changes of the list to take back, the last last; and the notice that offers to.
    pub(crate) undo: Vec<crate::rows::Undo>,
    pub(crate) undo_toast: Option<String>,
    /// The line under the search that picks which conversations are listed, open.
    pub(crate) list_tools: bool,
    /// Pull requests read so far, by branch: number and state.
    pub(crate) prs: HashMap<String, (u64, String)>,
    /// The modifier keys held, for ⌘ and a click in the list.
    pub(crate) modifiers: keyboard::Modifiers,
    /// The shortcut whose new key is waited for in the list of keys, and what was said of the last
    /// key given.
    pub(crate) key_capture: Option<crate::keys::Act>,
    pub(crate) key_note: Option<String>,
    /// The file the files tab shows, and what is typed to find one.
    pub(crate) viewing: Option<crate::files::Viewing>,
    /// The file of the tab, being edited; or shown as it reads, when it is Markdown.
    pub(crate) file_edit: Option<crate::edit::FileEdit>,
    /// The known editors found on the PATH, read when the settings open.
    pub(crate) editors: Vec<(&'static str, &'static str)>,
    pub(crate) file_preview: Option<crate::transcript::Markdown>,
    /// The tab searches what files hold; what the last search found, and one going on.
    pub(crate) file_grep: bool,
    pub(crate) grep_hits: Option<Vec<crate::edit::Hit>>,
    pub(crate) grep_busy: bool,
    pub(crate) file_query: String,
    /// The files tab's folders opened in its tree, and the git tab's folders folded, by path.
    pub(crate) tree_open: HashSet<String>,
    pub(crate) git_folded: HashSet<String>,
    /// The conversation a second click deletes.
    pub(crate) confirm_delete: Option<String>,
    /// The groups of the list folded shut, by name.
    pub(crate) collapsed: HashSet<String>,
    /// The branches there are to switch to, once listed.
    pub(crate) branch_list: Option<Vec<String>>,
    /// The branch's pull request, as last read, and whether it is being read.
    pub(crate) pr: Option<crate::git::Pr>,
    /// How many times this window started a conversation to fix each pull request's checks.
    pub(crate) auto_fixes: HashMap<u64, usize>,
    pub(crate) pr_loading: bool,
    /// The comment being written under a line of a diff.
    pub(crate) commenting: Option<crate::review::Draft>,
    /// The message of the conversation under the pointer, which shows what can be done with it.
    pub(crate) ex_hover: Option<usize>,
    /// What is said in each conversation, lower case, for the search, as of when its log changed;
    /// and whether it is being read.
    pub(crate) bodies: HashMap<PathBuf, (std::time::SystemTime, String)>,
    indexing: bool,
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
    pub(crate) settling: bool,
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
    /// Comments on lines of a diff, to go with the next message.
    pub(crate) comments: Vec<crate::review::Comment>,
    /// Long messages opened whole, by exchange.
    pub(crate) whole: HashSet<usize>,
    /// Messages sent while it ran, each to start a run of its own after it, in order.
    pub(crate) follow: Vec<String>,
    /// The run is of a CLI that reads edited arguments and stops one call (it listed the plugins'
    /// commands, as one since 0.8 does at its start); an older one would take them for others.
    pub(crate) newer: bool,
    /// A scratch conversation: its log goes into the trash once it is left and its run is over.
    pub(crate) scratch: bool,
    /// How its log was when last looked at, while no run here wrote it.
    pub(crate) log_seen: Option<crate::watch::Seen>,
    /// The model its runs use, over the window's: one of several a message was sent to.
    pub(crate) model: Option<String>,
    /// Its last run, in plan mode, ended with a plan: to go ahead with.
    pub(crate) planned: bool,
    /// The working tree as a tree object when the last turn began, and when it ended: what the git
    /// tab's last-turn scope compares.
    pub(crate) turn_from: Option<String>,
    pub(crate) turn_to: Option<String>,
    /// The permission mode its next run uses, over the window's, once: an automation's. What is
    /// sent after in the conversation goes in the mode the window shows.
    pub(crate) mode: Option<&'static str>,
    /// The automation it was started by, by id.
    pub(crate) automation: Option<String>,
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
            comments: Vec::new(),
            whole: HashSet::new(),
            follow: Vec::new(),
            newer: false,
            scratch: false,
            log_seen: None,
            model: None,
            planned: false,
            turn_from: None,
            turn_to: None,
            mode: None,
            automation: None,
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
    pub(crate) fn wanted(&self) -> bool {
        self.busy()
            || self.unread
            || self.next.is_some()
            || !self.editor.text().trim().is_empty()
            || !self.comments.is_empty()
    }

    /// A new conversation nothing was sent in yet.
    fn untouched(&self) -> bool {
        self.session.is_none() && !self.busy() && self.transcript.is_empty()
    }

    pub(crate) fn control(&self, message: ControlMessage) {
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
    /// In plan mode: its writes and edits are denied.
    plan: bool,
}

impl Chat {
    /// Whether its run goes on in plan mode.
    #[cfg(test)]
    pub(crate) fn plan_run(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.plan)
    }
}

/// The sasacode a path in the settings names: empty, `$SASACODE_BIN` or `sasacode` on PATH.
pub(crate) fn program_of(path: &str) -> PathBuf {
    match path.trim() {
        "" => std::env::var_os("SASACODE_BIN").map_or_else(|| PathBuf::from("sasacode"), PathBuf::from),
        path => PathBuf::from(path),
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Edit(text_editor::Action),
    /// Enter in the input, or its button.
    Send,
    /// ⌘↵ or ⌘⇧↵: the chord, which allows the waiting approval when the input is empty, else stops
    /// what runs and sends the input next; or, with Enter set to break the line, ⌘↵ sends.
    CommandEnter {
        shift: bool,
    },
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
    /// Something to draw over the window, or none.
    Show(Option<Overlay>),
    /// What is typed in the palette, or in the new name.
    OverlayText(String),
    /// ↑ (-1) or ↓ (1) where no field took it.
    Arrow(i8),
    /// The palette's entry at this place in what it shows now.
    PaletteRun(usize),
    SetEnter(bool),
    SetDark(Option<bool>),
    /// Larger (1), smaller (-1), or as it was (0).
    Zoom(i8),
    SetNotify(bool),
    SetAwake(bool),
    SetSteps(bool),
    /// The window came to the front, or went behind.
    Focus(bool),
    /// Files were held over the window, or taken away.
    Hover(bool),
    Dropped(PathBuf),
    /// Cmd+V in the input: the clipboard's text, or else its image as a file.
    Paste,
    Pasted(Option<String>),
    PastedImage(Option<PathBuf>),
    /// What was sent before: further back (-1) or forward (1).
    Recall(i8),
    /// The project's files, for `@`.
    Files(Vec<String>),
    /// Another of what the input offers: up (-1) or down (1).
    Pick(i8),
    /// What the input offers taken: the chosen one, or this one.
    Complete(Option<usize>),
    /// What the input offers put away until it is typed in again.
    Dismiss,
    /// A command of the window, from `/`, the palette or a key.
    Command(Command),
    /// A path in the file manager or an editor, or opened as the system opens it.
    Reveal(PathBuf),
    OpenPath(PathBuf),
    /// An image named in the input, taken out of it as it is written there.
    Unattach(String),
    Editor(PathBuf),
    /// A file in the editor, at a line.
    EditorAt(PathBuf, usize),
    /// The request for an AGENTS.md put in the input.
    WriteInstructions,
    /// A conversation kept at the top of the list, or let go; taken out of it, or put back.
    Pin(String),
    Archive(String),
    /// A name for a conversation: asked for, and given.
    Rename(String),
    RenameSave,
    /// A conversation's log deleted for good, on the second press.
    Delete(String),
    /// A group for a conversation: asked for, one of those there are taken, and the one typed
    /// given (none takes it out of its group).
    Group(String),
    GroupPick(String),
    GroupSave,
    /// A group of the list folded shut, or opened.
    FoldGroup(String),
    /// A file shown in the files tab, at a line if given; as it was read; and put away for the
    /// list of files, which is found by what is typed.
    ViewFile(PathBuf, Option<usize>),
    FileRead(Box<crate::files::Viewing>),
    CloseFile,
    FileQuery(String),
    /// A folder of the files tab's tree opened or shut, and one of the git tab's folded or not.
    TreeFold(String),
    GitFold(String),
    /// The file read in the tab, edited and saved, or left as it was.
    FileEditStart,
    FileEditAction(text_editor::Action),
    FileSave,
    FileEditCancel,
    /// The Markdown file read in the tab, shown as it reads, or as written.
    FilePreview(bool),
    /// The files tab finds files by what they hold, not their names; the search, and what it found.
    FileGrepMode(bool),
    FileGrep,
    Grepped(String, Result<Vec<crate::edit::Hit>, String>),
    /// A script of the folder run, by its place in the list; what its run said; stopped; and its
    /// output put in the input for the agent to look at.
    RunScript(usize),
    ScriptUpdate(u64, crate::scripts::ScriptUpdate),
    StopScript(u64),
    ScriptToAgent(u64),
    /// The folder's list of scripts changed in the settings: one added, named, its command
    /// typed, or taken out.
    ScriptAdd,
    ScriptName(usize, String),
    ScriptCommand(usize, String),
    ScriptRemove(usize),
    /// A new key for a shortcut asked for in the list of keys; the key pressed then; and a
    /// shortcut's key back to its default, or all of them.
    KeyCapture(crate::keys::Act),
    KeyCaptured(Key, keyboard::Modifiers),
    KeyReset(Option<crate::keys::Act>),
    /// The conversation scrolled to message `i`, from its outline.
    JumpTo(usize),
    /// A conversation put beside the one shown; the two swapped; the one beside put away.
    Split(PathBuf),
    SplitSwap,
    SplitClose,
    /// The modifier keys held changed.
    Modifiers(keyboard::Modifiers),
    /// The model asked for a commit message, a pull request's words, or the next message; its answer.
    Draft(crate::helper::Draft),
    Drafted(crate::helper::Draft, Result<String, String>),
    /// The pull request written before it is made: what it is written from, its words, made.
    PrOpen,
    PrContext(Result<Box<crate::git::PrContext>, String>),
    PrTitle(String),
    PrBody(text_editor::Action),
    PrCreate,
    PrAsDraft(bool),
    /// The pull request left to `gh pr create --fill`.
    PrFill,
    /// The plan written in plan mode, gone ahead with in the mode chosen before.
    PlanGo,
    /// The next message guessed, put in the input or put away.
    TakeSuggestion,
    DropSuggestion,
    SetSuggest(bool),
    /// The window was asked to close; closed after all.
    CloseAsked(window::Id),
    Quit(window::Id),
    /// `auto` agreed to, the first time.
    AutoAgree,
    /// A conversation marked unread, or read again.
    MarkUnread(String),
    /// The conversation written as Markdown to a file.
    ExportTo(String),
    SetWidth(crate::layout::Width),
    /// A long message opened whole, or folded again.
    Whole(usize),
    SetLang(Option<&'static str>),
    /// What the git tab compares, and what was found for it.
    GitScope(crate::scope::Scope),
    Scoped(
        crate::scope::Scope,
        Vec<(String, String)>,
        Result<Box<crate::git::Scoped>, String>,
    ),
    /// Every file of the scope folded, or opened.
    FoldFiles(bool),
    SetSplitDiff(bool),
    SetWhitespace(bool),
    /// Diff lines of the git tab kept whole, scrolled sideways, or wrapped.
    SetDiffNowrap(bool),
    /// The working tree taken as conversation `key`'s turn began (`true`) or ended.
    TurnTree(u64, bool, Option<String>),
    /// A file marked as viewed, or not, by its key.
    Viewed(String),
    /// A menu opened by a right click, where the pointer is; put away; or one of its items chosen.
    OpenMenu(crate::rows::MenuFor),
    CloseMenu,
    MenuDo(Box<Msg>),
    /// A conversation chosen in the list with others, by ⇧-click; none chosen; done to all chosen.
    Select(String),
    SelectNone,
    Bulk(crate::rows::Bulk),
    /// Which conversations the list shows and in which order, and the line that picks them.
    ListShow(crate::layout::Show),
    ListOrder(crate::layout::Order),
    ListTools,
    /// Conversations archived after this many days unwritten to.
    SetArchiveAfter(Option<u32>),
    /// Every conversation marked read.
    ReadAll,
    /// A conversation of the list, gone on with in the terminal.
    RowTui(String),
    /// The last change of the list taken back; by ⌘Z, unless a field has the keyboard.
    Undo,
    UndoKey,
    UndoKeyHeld(bool),
    /// A follow-up waiting for the run: taken back into the input, sent to the run now, moved up
    /// a place, or dropped.
    FollowEdit(usize),
    FollowNow(usize),
    FollowUp(usize),
    FollowDrop(usize),
    /// Messages sent while a run works wait for it to end, each for a run of its own.
    SetQueue(bool),
    /// Another conversation put in the input as context: its Markdown, written to a file, named.
    AttachConversation(String),
    /// A reply opened where its text can be chosen, and the choice quoted into the input.
    QuoteOpen(usize),
    QuoteEdit(text_editor::Action),
    QuoteTake,
    /// A plugin's question answered (`Null` dismisses it), what is ticked or typed for it so far,
    /// and the answer as it stands sent.
    AskAnswer(String, serde_json::Value),
    AskToggle(String, String),
    AskType(String, String),
    AskSubmit(String),
    /// One tool call stopped, by its id: the run goes on.
    StopCall(String),
    /// The waiting call's arguments edited, then allowed with them.
    ArgsOpen,
    ArgsEdit(text_editor::Action),
    ArgsSend,
    /// A rule "always" added for the conversation shown, taken back.
    ForgetAllowed(String),
    /// A rule "always" allowed kept in the CLI's settings: of this folder (`true`) or of all.
    KeepAllowed(String, bool),
    /// A picture shown larger, and at its own size or to fit.
    ViewImage(PathBuf),
    ImageActual(bool),
    /// The next side question typed, asked; and the side questions and answers put in the input
    /// as a quote, for the conversation.
    SideType(String),
    SideAsk,
    SideBring,
    /// A title for a conversation written by the model: asked for, and come back.
    WriteTitle(String),
    Titled(String, bool, Result<String, String>),
    SetAutoTitle(bool),
    /// Which notifications are sent, with a sound or not; the editor files open in.
    SetQuietDone(bool),
    SetQuietWait(bool),
    SetSound(bool),
    SetNotifyAlways(bool),
    /// The files tab saves what is typed by itself; a look whether it is time to.
    SetAutosave(bool),
    /// Failed checks fixed by a conversation started by itself; their logs, read for it.
    SetAutoFix(bool),
    AutoFixReady(String),
    FileAutosave,
    /// The sasacode executable the window runs; empty, the one on PATH.
    SetProgram(String),
    SetEditor(String),
    /// sasacode's colour, and the fonts the window uses from its next start.
    SetAccent(String),
    SetUiFont(String),
    SetCodeFont(String),
    /// A message asking for a review of the changes, put in the input.
    AskReview,
    /// A new conversation whose log goes once it is left.
    NewScratch,
    /// The log of the conversation shown is looked at, in case another wrote it.
    WatchLog,
    /// What the conversations used, of every folder or (true) this one: asked, and read.
    Usage(bool),
    UsageRead(bool, Box<crate::usage::Report>),
    /// What of it the usage screen shows (`/usage graph` and the others), opened if it is not.
    UsagePart(crate::usage::Part),
    /// The automations' screen, and their clock.
    Auto(crate::automations::AutoMsg),
    /// The conversation shared as a secret gist: asked, made, and its URL or why not.
    GistAsk,
    GistCreate,
    GistMade(Result<String, String>),
    /// The models the next message goes to at once: one chosen or not, one typed, added; sent.
    CompareToggle(String),
    CompareType(String),
    CompareAdd,
    CompareSend,
    /// A file of instructions opened in the editor, made empty first when it is not there.
    OpenInstructions(PathBuf),
    /// The sasacode home folder, shown in the file manager.
    OpenHome,
    /// "Check for updates" on the about screen: `sasacode update --check`, and what it said.
    CheckUpdate,
    UpdateChecked(Result<String, String>),
    /// To the conversation left, and forward again.
    Back,
    Forward,
    /// The next run's effort and permission mode, one step on; the model field, to type in.
    CycleEffort,
    CyclePermission,
    FocusModel,
    /// The side question's panel, open to type one.
    SideOpen,
    /// What the CLI listed (part of it), asked again, and the folder trusted: for good, as the
    /// CLI asks, or for this window's runs.
    Listed(Box<crate::cli::Listing>),
    Relist,
    TrustFolder,
    Trusted(Result<String, String>),
    TrustOnce,
    /// Back to the branch the conversation was started on.
    BackToBranch(String),
    /// The notice that the branch changed, put away for this conversation.
    BranchSeen(String),
    /// A new conversation that holds the one shown through exchange `i`'s reply.
    ForkAfter(usize),
    /// To the next, or the last, of what the search in the conversation finds.
    FindStep(i8),
    /// A file named with its line in what a program wrote, `src/a.rs:12`, opened.
    OpenRef(String),
    /// Of the calls, only those that failed are shown; what the model thought is not.
    SetFailedOnly(bool),
    SetHideThinking(bool),
    SetMergeMethod(&'static str),
    /// The pointer over a row of the list, or off it.
    RowHover(Option<String>),
    /// What the search inside the conversation looks for, and the search put away.
    FindInText(String),
    FindInClose,
    /// Another folder sasacode was used in, in a window of its own.
    OpenProject(PathBuf),
    /// The other folders sasacode was used in, as read from its home.
    Others(Vec<PathBuf>),
    /// The other folders, listed under the conversations or not.
    FoldFolders,
    /// The background jobs read; read again; one's output shown or not; one stopped, on the
    /// second press.
    Jobs(Vec<crate::jobs::Job>),
    JobsRefresh,
    JobOpen(String),
    JobStop(String),
    /// A worktree made (and readied, or what failed readying it), or why not.
    WorktreeMade(Result<(PathBuf, Option<String>), String>),
    /// The worktrees made here, read for the list; one to remove, and how that went.
    Worktrees(Vec<crate::worktrees::Tree>),
    WorktreeRemove(PathBuf),
    WorktreeRemoved(PathBuf, Result<(), String>),
    /// The command that readies this folder's new worktrees.
    SetWorktreeSetup(String),
    /// The folder chosen in the system's picker (⌘O), none when it was closed: a window of its own.
    FolderPicked(Option<PathBuf>),
    /// Another window on a folder, open on a conversation if given, where this one is if known.
    NewWindow(PathBuf, Option<String>, Option<iced::Point>),
    /// What is said in conversations, read for the search: by log, as of when it was changed.
    Indexed(Vec<(PathBuf, std::time::SystemTime, String)>),
    /// The repository looked at again, and as it was found.
    GitRefresh,
    GitStatus(Box<crate::git::Status>),
    /// Something done from the git tab, and what came of it.
    GitDo(crate::git::Op),
    GitDone(Result<String, String>),
    CommitText(String),
    /// A line of a diff: where it is goes into the input, to be talked about.
    Quote(PathBuf, usize),
    /// A comment on line `n` of a file's diff, in its staged part or not: asked for under the line,
    /// typed, kept to go with the next message, its place put in the input instead, or let go.
    CommentAt(PathBuf, usize, bool),
    CommentText(String),
    CommentSave,
    CommentQuote,
    CommentCancel,
    /// A comment kept to go with the next message, taken back.
    CommentRemove(usize),
    /// The branches there are to switch to.
    Branches(Vec<String>),
    /// The branch's pull request read again, and as it was found.
    PrRefresh,
    PrLoaded(Option<Box<crate::git::Pr>>),
    /// The failed checks' logs read, to ask the agent to fix them; and as they were read.
    FixChecks,
    FixReady(String),
    /// What a reviewer said, put in the input for the agent to address.
    Address(usize),
    /// Conversations are archived when their branch's pull request is merged or closed, or not.
    SetAutoArchive(bool),
    /// Conversations whose branch's pull request was merged or closed are looked for; and found.
    Sweep,
    Swept(Vec<String>),
    /// A new conversation branched off before message `i`, the message in its input to be changed
    /// and sent; or before the last message, sent again as it was.
    Fork(usize),
    Retry,
    /// The pointer over a message of the conversation, or off it.
    ExHover(Option<usize>),
    /// What waits to be sent when the run ends, back into the input.
    Retract,
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
    /// Esc that a field took for itself, letting go of the keyboard.
    Escaped,
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
    /// The window was moved: where its top left corner is now, less the zoom.
    Moved(Point),
}

impl App {
    pub fn new(options: Options) -> (Self, Task<Msg>) {
        let wanted = options.session.clone();
        let lang = options.prefs.more.lang.clone();
        let mut app = App::blank(options, App::strings_for(lang.as_deref()));
        // Shared with the TUI. A window made in a test has none, so tests never write to it.
        app.history_file = app.home.as_ref().map(|home| home.join("history.jsonl"));
        app.history = app
            .history_file
            .as_deref()
            .map(crate::composer::read_history)
            .unwrap_or_default();
        app.reload_sessions();
        if let Some(home) = &app.home {
            crate::rows::purge_trash(home);
        }
        app.archive_idle();
        // One that is not there is not an error: a conversation nobody has said anything in has an
        // id and no log yet. The window opens on a new one.
        let asked = wanted.is_some();
        if let Some(wanted) = wanted
            && !app.open_named(&wanted)
        {
            eprintln!(
                "sasacode-gui: no session {wanted} in {}: starting a new conversation",
                app.cwd.display()
            );
        }
        app.restore_folder(asked);
        let screen = window::oldest().and_then(|id| window::monitor_size(id).map(move |size| Msg::Screen(id, size)));
        // In front when it opens: a window another one started is not brought there by the system.
        let front = window::oldest().and_then(window::gain_focus);
        let git = app.refresh_git();
        let sweep = app.sweep();
        let listed = app.relist_cli();
        let others = app.list_others();
        app.compact_at = crate::cli::compaction_point(app.home.as_deref(), &app.cwd);
        (
            app,
            Task::batch([
                iced::system::theme().map(Msg::ThemeMode),
                focus_input(),
                screen,
                front,
                git,
                sweep,
                listed,
                others,
            ]),
        )
    }

    /// Reads which other folders sasacode was used in, off the window's thread.
    pub(crate) fn list_others(&self) -> Task<Msg> {
        let Some(home) = self.home.clone() else {
            return Task::none();
        };
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || crate::system::projects(&home))
                    .await
                    .unwrap_or_default()
            },
            Msg::Others,
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
            // The window's size as it is drawn: what the system measures, less the zoom.
            width: layout.size.width / options.prefs.zoom,
            height: layout.size.height / options.prefs.zoom,
            at: layout.at,
            layout_file: options.layout_file,
            kept: layout,
            prefs: {
                crate::keys::bind(crate::keys::Table::from(&options.prefs.keys));
                options.prefs.clone()
            },
            kept_prefs: options.prefs,
            system_dark: false,
            focused: true,
            overlay: None,
            history: Vec::new(),
            history_file: None,
            recall: None,
            recall_draft: String::new(),
            files: Vec::new(),
            files_asked: false,
            pick: 0,
            dismissed: false,
            dropping: false,
            awake: None,
            find_in: None,
            find_at: 0,
            quoting: None,
            editing_args: None,
            plugin_commands: Vec::new(),
            listing: crate::cli::Listing::default(),
            trust_once: false,
            compact_at: None,
            image_actual: false,
            log: Default::default(),
            compare: Default::default(),
            compare_typed: String::new(),
            usage: None,
            usage_here: false,
            usage_part: crate::usage::Part::All,
            worktree_list: None,
            before_plan: None,
            auto_form: crate::automations::Form::default(),
            gisting: false,
            update_said: None,
            checking_update: false,
            others: Vec::new(),
            jobs: Vec::new(),
            job_open: None,
            job_stopping: None,
            side_typed: String::new(),
            titling: Default::default(),
            back: Vec::new(),
            forward: Vec::new(),
            navigating: false,
            branch_seen: HashSet::new(),
            find_held: false,
            show_archived: false,
            row_hover: None,
            bodies: HashMap::new(),
            indexing: false,
            git: crate::git::Status::default(),
            git_busy: false,
            commit_msg: String::new(),
            confirm_discard: None,
            confirm_merge: false,
            confirm_delete: None,
            viewing: None,
            file_edit: None,
            editors: Vec::new(),
            file_preview: None,
            file_grep: false,
            grep_hits: None,
            grep_busy: false,
            file_query: String::new(),
            tree_open: HashSet::new(),
            git_folded: HashSet::new(),
            key_capture: None,
            key_note: None,
            git_scope: crate::scope::Scope::Changes,
            scoped: None,
            scope_loading: false,
            scope_commits: Vec::new(),
            confirm_default: false,
            drafting: Vec::new(),
            pr_draft: None,
            suggestion: None,
            split: None,
            menu: None,
            selected: std::collections::BTreeSet::new(),
            undo: Vec::new(),
            undo_toast: None,
            list_tools: false,
            prs: HashMap::new(),
            modifiers: keyboard::Modifiers::default(),
            script_runs: Vec::new(),
            script_runs_made: 0,
            collapsed: HashSet::new(),
            branch_list: None,
            pr: None,
            auto_fixes: HashMap::new(),
            pr_loading: false,
            commenting: None,
            ex_hover: None,
            aside: None,
            toast: None,
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
        // What the conversation is doing, before its name: the window list and the Dock show it.
        let state = if self.running() && self.chat.transcript.waits() {
            "⚠ "
        } else if self.running() {
            "● "
        } else {
            ""
        };
        if self.chat.transcript.exchanges.is_empty() && self.chat.session.is_none() {
            return format!("sasacode — {folder}");
        }
        format!("{state}{} — {folder}", self.chat_title(&self.chat))
    }

    pub fn theme(&self) -> Theme {
        self.theme.clone()
    }

    pub(crate) fn set_dark(&mut self, dark: bool) {
        self.t = crate::theme::with_accent(if dark { DARK } else { LIGHT }, &self.prefs.more.accent);
        self.theme = self.t.theme();
    }

    /// Light or dark as the settings say, else as the system is.
    pub(crate) fn apply_theme(&mut self) {
        self.set_dark(self.prefs.dark.unwrap_or(self.system_dark));
    }

    /// How large the window's contents are drawn.
    pub fn scale(&self) -> f32 {
        self.prefs.zoom
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        // While a shortcut's new key is waited for, the keys are that, and do nothing else.
        let keys = if self.key_capture.is_some() {
            event::listen_with(capture_key)
        } else {
            event::listen_with(global_key)
        };
        let mut all = vec![
            iced::system::theme_changes().map(Msg::ThemeMode),
            window::resize_events().map(|(_, size)| Msg::Resized(size)),
            keys,
            event::listen_with(window_event),
            event::listen_with(crate::rows::pointer),
            window::close_requests().map(Msg::CloseAsked),
        ];
        // Conversations whose pull request was merged or closed are looked for now and then.
        if self.prefs.auto_archive {
            all.push(iced::time::every(SWEEP_EVERY).map(|_| Msg::Sweep));
        }
        // The conversation shown is read again when another continues it.
        if self.watches_log() {
            all.push(iced::time::every(Duration::from_secs(2)).map(|_| Msg::WatchLog));
        }
        // Automations, while any of this folder is on.
        if self.watches_automations() {
            all.push(
                iced::time::every(crate::automations::LOOK_EVERY).map(|_| Msg::Auto(crate::automations::AutoMsg::Look)),
            );
        }
        // A pull request's checks, while they run.
        if self.watches_checks() {
            all.push(iced::time::every(CHECKS_EVERY).map(|_| Msg::PrRefresh));
        }
        // The clock only moves what is shown while something runs.
        if self.running() {
            all.push(iced::time::every(Duration::from_secs(1)).map(Msg::Tick));
        }
        // What is typed in the files tab, saved a moment after the last key.
        if self.prefs.more.autosave && self.file_edit.as_ref().is_some_and(|e| e.typed_at.is_some()) {
            all.push(iced::time::every(Duration::from_millis(400)).map(|_| Msg::FileAutosave));
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
            plan: false,
        });
        written
    }

    pub(crate) fn reload_sessions(&mut self) {
        self.sessions = match &self.home {
            Some(home) => list_sessions(home, &self.cwd),
            None => Vec::new(),
        };
        for session in &mut self.sessions {
            session.first_prompt = crate::transcript::as_typed(&session.first_prompt, &self.cwd);
        }
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

    pub(crate) fn control(&self, message: ControlMessage) {
        self.chat.control(message);
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        // Esc puts a menu away first.
        if self.menu.is_some() && matches!(msg, Msg::Stop | Msg::Escaped) {
            self.menu = None;
            return Task::none();
        }
        // Over the window, Esc puts away what is drawn there; it never stops a run.
        if self.overlay.is_some() && matches!(msg, Msg::Stop | Msg::FocusInput | Msg::Escaped) {
            self.overlay = None;
            return focus_input();
        }
        // Which field has the keyboard is not asked of iced; what was last typed in tells.
        match &msg {
            Msg::FindInText(_) => self.find_held = true,
            Msg::Edit(_)
            | Msg::Model(_)
            | Msg::Filter(_)
            | Msg::CommitText(_)
            | Msg::FocusInput
            | Msg::Find
            | Msg::Command(Command::Model) => {
                self.find_held = false;
            }
            _ => {}
        }
        let msg = match self.update_more(msg) {
            Ok(task) => return task,
            Err(msg) => msg,
        };
        match msg {
            Msg::Edit(action) => return self.edit(action),
            Msg::Send => return self.send(),
            // ⌘↵ is the chord; with Enter set to break the line it sends, and the chord is ⌘⇧↵.
            Msg::CommandEnter { shift } => {
                return if self.prefs.enter_sends || shift {
                    self.chord()
                } else {
                    self.send()
                };
            }
            Msg::Stop => {
                if self.running() {
                    self.control(ControlMessage::Abort { call_id: None });
                } else {
                    // Nothing to stop: Esc puts the search in the conversation away.
                    self.find_in = None;
                }
            }
            Msg::Continue => {
                if !self.busy() {
                    return self.start(self.chat.key, self.s.continue_text.to_owned());
                }
            }
            Msg::Run(key, update) => return self.on_run(key, update),
            Msg::Answer(decision) => return self.answer(decision),
            Msg::NewScratch => return self.new_scratch(),
            Msg::NewSession => {
                if !self.chat.untouched() {
                    // What was typed for a new conversation before is still there.
                    let chat = match self.parked.iter().position(|c| c.untouched() && !c.scratch) {
                        Some(i) => self.parked.remove(i),
                        None => self.fresh_chat(),
                    };
                    self.bring(chat);
                }
                return Task::batch([focus_input(), self.back_to_newest()]);
            }
            Msg::OpenSession(path) => {
                // Opened from the usage screen: the conversation is what is wanted now.
                if self.overlay == Some(Overlay::Usage) {
                    self.overlay = None;
                }
                if self.open_session(&path) {
                    return Task::batch([focus_input(), self.back_to_newest()]);
                }
            }
            Msg::Fork(i) => return self.fork(i, false),
            Msg::Retry => {
                let last = self.chat.transcript.exchanges.len().saturating_sub(1);
                return self.fork(last, true);
            }
            Msg::ExHover(i) => self.ex_hover = i,
            Msg::Retract => {
                if let Some(next) = self.chat.next.take() {
                    let typed = self.chat.editor.text();
                    let text = if typed.trim().is_empty() {
                        next
                    } else {
                        format!("{next}\n\n{typed}")
                    };
                    self.chat.editor = text_editor::Content::with_text(&text);
                    self.chat
                        .editor
                        .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
                    return focus_input();
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
            Msg::Filter(text) => {
                self.filter = text;
                // Two letters at least: one finds nearly everything.
                if self.filter.trim().chars().count() >= 2 {
                    return self.index_bodies();
                }
            }
            Msg::Indexed(read) => {
                self.indexing = false;
                for (path, when, body) in read {
                    self.bodies.insert(path, (when, body));
                }
            }
            Msg::Model(model) => self.model = model,
            // Running everything without asking is agreed to once, knowing what it does.
            Msg::Permission("auto") if !self.prefs.more.auto_agreed => {
                self.overlay = Some(Overlay::ConfirmAuto);
            }
            Msg::Permission(p) => {
                // The mode plan mode is left for, when its plan is gone ahead with.
                if p == crate::plan::MODE && self.permission != Some(crate::plan::MODE) {
                    self.before_plan = self.permission;
                }
                self.permission = PERMISSIONS.contains(&p).then_some(p);
            }
            Msg::Effort(e) => self.effort = EFFORTS.contains(&e).then_some(e),
            Msg::PlanGo => {
                if self.permission != Some(crate::plan::MODE) || self.busy() {
                    return Task::none();
                }
                self.permission = self.before_plan.take();
                self.chat.planned = false;
                let said = self.s.w().plan_go_prompt.to_owned();
                return self.start(self.chat.key, said);
            }
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
                if tab == Tab::Git {
                    return Task::batch([self.refresh_git(), self.refresh_pr()]);
                }
                if tab == Tab::Files {
                    return self.list_files();
                }
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
            Msg::Link(uri) => match crate::files::linked_file(uri.as_str(), &self.cwd) {
                // A file of the project named in a reply opens in the files tab.
                Some((path, line)) => return self.update(Msg::ViewFile(path, line)),
                None => open_link(&uri),
            },
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
            Msg::Escaped => {
                // In the search of the conversation, the first Esc puts it away, as Esc does in
                // other windows' find bars; in another field it only gives the keys back.
                if std::mem::take(&mut self.find_held) {
                    self.find_in = None;
                }
                return focus_input();
            }
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
            Msg::ThemeMode(mode) => {
                self.system_dark = mode == theme::Mode::Dark;
                self.apply_theme();
            }
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
            // Each step of a drag of the window is a move: kept once it has rested, as a resize is.
            // iced gives where it is less the zoom; the system measures it whole.
            Msg::Moved(at) => {
                let zoom = self.prefs.zoom;
                self.at = Some(Point::new(at.x * zoom, at.y * zoom));
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
                if let Some(size) = self.fitted(screen) {
                    return window::resize(id, size);
                }
            }
            // On no screen: kept where a screen was that is gone now. Brought back to the main one.
            Msg::Screen(id, None) => {
                if self.at.is_some() {
                    return window::move_to(id, ON_SCREEN);
                }
            }
            // Handled by `update_more`.
            _ => {}
        }
        Task::none()
    }

    /// How the window is laid out now: the widths the panels were given, folded or not.
    pub(crate) fn layout(&self) -> Layout {
        let zoom = self.prefs.zoom;
        Layout {
            size: Size::new(self.width * zoom, self.height * zoom),
            side: self.side_w,
            inspector: self.insp_w,
            at: self.at,
        }
    }

    /// The window's size made to fit `screen`, when it does not: both as iced measures them, in what
    /// is drawn at the zoom (iced divides the screen's size by the zoom too).
    pub(crate) fn fitted(&self, screen: Size) -> Option<Size> {
        let zoom = self.prefs.zoom;
        let on_screen = Size::new(screen.width * zoom, screen.height * zoom);
        layout::fit(self.layout().size, on_screen).map(|size| Size::new(size.width / zoom, size.height / zoom))
    }

    /// Keeps how the window is laid out for the next launch, when it changed since it was read or
    /// last written.
    pub(crate) fn keep_layout(&mut self) {
        let Some(file) = &self.layout_file else {
            return;
        };
        let now = self.layout();
        if now == self.kept && self.prefs == self.kept_prefs {
            return;
        }
        // Another window may have written since: what this one did not change is left as it is.
        let prefs = self.prefs.over(&self.kept_prefs, &Prefs::read(file));
        match layout::write(file, &now, &prefs) {
            Ok(()) => {
                self.kept = now;
                self.adopt(prefs);
            }
            Err(e) => eprintln!(
                "sasacode-gui: could not keep the window's layout in {}: {e}",
                file.display()
            ),
        }
    }

    /// Shows what other windows said about conversations since this one last read or wrote the
    /// file. This window's own settings stay as they are, as shown; the file keeps another window's
    /// until this one changes them.
    pub(crate) fn read_others(&mut self) {
        if let Some(file) = &self.layout_file {
            let held = self.prefs.over(&self.kept_prefs, &Prefs::read(file));
            self.adopt(held);
        }
    }

    /// Takes what other windows said about conversations from `held`: what the file holds, as this
    /// window wrote or read it.
    fn adopt(&mut self, held: Prefs) {
        self.prefs.names = held.names;
        self.prefs.pinned = held.pinned;
        self.prefs.archived = held.archived;
        self.prefs.branches = held.branches;
        self.prefs.groups = held.groups;
        self.prefs.scripts = held.scripts;
        // An automation another window of the folder ran, changed, stopped or deleted.
        self.prefs.more.automations = held.more.automations;
        self.kept_prefs = self.prefs.clone();
    }

    /// Shows the conversation a log holds, to be continued by the next message: as it was left
    /// when it was kept, running or typed in, else as its log has it. False when it is the one
    /// shown, or its log cannot be read.
    pub(crate) fn open_session(&mut self, path: &Path) -> bool {
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
        // As typed: an image sent from a file shows as its `@path` again, and goes with a retry.
        for ex in &mut chat.transcript.exchanges {
            ex.prompt = crate::transcript::as_typed(&ex.prompt, &self.cwd);
        }
        chat.loaded = true;
        // What was typed in it and not sent, before a restart.
        if let Some(draft) = chat.session.as_deref().and_then(|id| self.kept_draft(id)) {
            chat.editor = text_editor::Content::with_text(draft);
            chat.editor
                .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
        }
        self.bring(chat);
        true
    }

    /// A new conversation branched off before exchange `i` of the one shown: the old one is kept
    /// as it was. Its message goes into the new one's input, or, with `again`, is sent as it was.
    fn fork(&mut self, i: usize, again: bool) -> Task<Msg> {
        let Some(prompt) = self.chat.transcript.exchanges.get(i).map(|ex| ex.prompt.clone()) else {
            return Task::none();
        };
        if self.busy() {
            return Task::none();
        }
        if i == 0 {
            // Before the first message there is nothing to keep: a new conversation.
            let chat = self.new_chat();
            self.bring(chat);
        } else {
            let log = self
                .chat
                .session
                .as_ref()
                .and_then(|id| self.sessions.iter().find(|s| s.id == *id))
                .map(|s| s.path.clone());
            let made = log
                .ok_or_else(|| "no log".to_owned())
                .and_then(|log| crate::fork::fork(&log, i, &prompt));
            match made {
                Ok((_, path)) => {
                    self.reload_sessions();
                    if !self.open_session(&path) {
                        return Task::none();
                    }
                }
                Err(error) => {
                    self.toast = Some(fill(self.s.fork_failed, "error", error));
                    return Task::none();
                }
            }
        }
        if again {
            return self.start(self.chat.key, prompt);
        }
        self.chat.editor = text_editor::Content::with_text(&prompt);
        self.chat
            .editor
            .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
        Task::batch([focus_input(), self.back_to_newest()])
    }

    pub(crate) fn new_chat(&mut self) -> Chat {
        self.chats += 1;
        Chat::new(self.chats)
    }

    /// Shows `chat` in place of the conversation shown, which is kept while it is still wanted.
    pub(crate) fn bring(&mut self, chat: Chat) {
        self.bring_as(chat, false);
    }

    /// Shows `chat` as [`App::bring`] does, the one shown kept anyway when it is a conversation:
    /// it goes beside.
    pub(crate) fn bring_keeping(&mut self, chat: Chat) {
        self.bring_as(chat, true);
    }

    fn bring_as(&mut self, mut chat: Chat, keep: bool) {
        if chat.session != self.chat.session || chat.key != self.chat.key {
            self.leaving();
        }
        chat.unread = false;
        if let Some(id) = chat.session.clone()
            && self.prefs.more.unread.contains(&id)
        {
            let _ = self.set(|p| {
                p.more.unread.remove(&id);
            });
        }
        // Shown here, it is no longer beside.
        if chat.session.is_some() && chat.session == self.split {
            self.split = None;
        }
        let last = chat.session.clone().filter(|_| !chat.scratch);
        // The last turn the git tab showed was the conversation left's: back to the changes.
        if self.git_scope == crate::scope::Scope::Turn {
            self.git_scope = crate::scope::Scope::Changes;
            self.scoped = None;
        }
        let left = std::mem::replace(&mut self.chat, chat);
        self.keep_last(last);
        // A scratch conversation left is gone, once its run is over.
        if left.scratch && !left.busy() {
            self.drop_scratch(&left);
        } else {
            if !left.scratch {
                self.keep_draft(&left);
            }
            if left.wanted() || (keep && left.session.is_some()) {
                self.parked.push(left);
            }
        }
        self.copied = None;
        self.reading = false;
    }

    /// The conversation with `key`, shown or kept, or the side question.
    pub(crate) fn chat_mut(&mut self, key: u64) -> Option<&mut Chat> {
        if self.chat.key == key {
            return Some(&mut self.chat);
        }
        if key == SIDE {
            return self.aside.as_mut();
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

    /// The list's conversations in its order, those the search finds: see [`App::listed`].
    pub(crate) fn found(&self) -> impl Iterator<Item = &SessionSummary> {
        self.listed().into_iter().flat_map(|l| l.rows)
    }

    /// The search finds `s`: it looks in the names given, the first messages, and, once read,
    /// everything said in them. Archived ones only while those are shown.
    pub(crate) fn finds(&self, s: &SessionSummary) -> bool {
        let needle = self.filter.trim().to_lowercase();
        self.prefs.archived.contains(&s.id) == self.show_archived
            && (needle.is_empty()
                || s.first_prompt.to_lowercase().contains(&needle)
                || self
                    .prefs
                    .names
                    .get(&s.id)
                    .is_some_and(|n| n.to_lowercase().contains(&needle))
                || self.bodies.get(&s.path).is_some_and(|(_, body)| body.contains(&needle))
                // Its model, the branch it was started on, and that branch's pull request: `#12`.
                || s.model.as_deref().is_some_and(|m| m.to_lowercase().contains(&needle))
                || self
                    .prefs
                    .branches
                    .get(&s.id)
                    .is_some_and(|b| b.to_lowercase().contains(&needle))
                || self.pr_of(&s.id).is_some_and(|(n, _)| format!("#{n}") == needle))
    }

    /// What is said in the conversations that have not been read for the search, or changed since,
    /// read away from the window.
    fn index_bodies(&mut self) -> Task<Msg> {
        if self.indexing {
            return Task::none();
        }
        let stale: Vec<(PathBuf, std::time::SystemTime)> = self
            .sessions
            .iter()
            .filter(|s| self.bodies.get(&s.path).is_none_or(|(when, _)| *when != s.modified))
            .map(|s| (s.path.clone(), s.modified))
            .collect();
        if stale.is_empty() {
            return Task::none();
        }
        self.indexing = true;
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || crate::export::bodies(stale))
                    .await
                    .unwrap_or_default()
            },
            Msg::Indexed,
        )
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
            Clip::Conversation => Some(crate::export::markdown(self.name(), &self.chat.transcript, self.s)),
            Clip::Text(text) => Some(text.clone()),
            Clip::Folder => Some(self.cwd.display().to_string()),
            Clip::Link => Some(crate::link::make(&self.cwd, self.chat.session.as_deref())),
            Clip::About => Some(self.about_text()),
            Clip::Log => {
                let id = self.chat.session.as_deref()?;
                Some(self.sessions.iter().find(|s| s.id == id)?.path.display().to_string())
            }
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
    pub(crate) fn back_to_newest(&mut self) -> Task<Msg> {
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

    /// What was typed, taken out of the input and remembered with what was sent before.
    pub(crate) fn take_input(&mut self) -> Option<String> {
        let typed = self.chat.editor.text().trim().to_owned();
        if typed.is_empty() && self.chat.comments.is_empty() {
            return None;
        }
        // What is remembered under ↑ is what was typed; the comments go with the message only.
        let comments = std::mem::take(&mut self.chat.comments);
        let text = crate::review::with_comments(&typed, &comments, &self.cwd, self.s.w().comments_block);
        self.chat.editor = text_editor::Content::new();
        self.recall = None;
        self.dismissed = false;
        if !typed.is_empty() && self.history.last() != Some(&typed) {
            self.history.push(typed.clone());
            if let Some(file) = &self.history_file {
                crate::composer::append_history(file, &typed);
            }
        }
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
        // A command of the window runs here; it is never sent.
        if let Some(task) = self.as_command() {
            return task;
        }
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
                args: None,
            });
            self.chat.transcript.answer_approval(&id, Answer::Denied(text));
            return Task::none();
        }
        // `!command`: run in the folder's shell, its output in the commands tab.
        if let Some(command) = self
            .chat
            .editor
            .text()
            .trim()
            .strip_prefix('!')
            .map(str::trim)
            .map(str::to_owned)
            && !command.is_empty()
        {
            let _ = self.take_input();
            return self.run_command(format!("!{command}"), command);
        }
        // A plugin's slash command, or a skill's: run as one, not sent to the model as text.
        if !self.running() && !self.busy() && self.is_slash(&self.chat.editor.text()) {
            let Some(text) = self.take_input() else {
                return Task::none();
            };
            return self.start_run(self.chat.key, text, true);
        }
        if self.running() {
            let Some(text) = self.take_input() else {
                return Task::none();
            };
            // Kept for a run of its own after this one, as the settings say.
            if self.prefs.more.queue {
                self.chat.follow.push(text);
                return Task::none();
            }
            // Like typing while the TUI is busy: it reaches the model at its next turn.
            self.chat.transcript.queue(&text);
            self.control(ControlMessage::Message { text });
            return self.back_to_newest();
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

    /// Allows the waiting approval when nothing is typed; else stops what runs and sends the input
    /// next.
    fn chord(&mut self) -> Task<Msg> {
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
            self.control(ControlMessage::Abort { call_id: None });
            self.queue_next();
            return Task::none();
        }
        self.send()
    }

    fn answer(&mut self, decision: Decision) -> Task<Msg> {
        self.answer_with(decision, None)
    }

    /// Answers the approval waiting, with the call's arguments as the user edited them if so.
    /// "Always" is kept for the conversation's later runs too.
    pub(crate) fn answer_with(&mut self, decision: Decision, args: Option<serde_json::Value>) -> Task<Msg> {
        let Some(approval) = self.chat.transcript.pending_approval() else {
            return Task::none();
        };
        let (id, rules) = (approval.id.clone(), approval.rules.clone());
        self.control(ControlMessage::Approval {
            id: id.clone(),
            decision,
            feedback: None,
            args,
        });
        let answer = match decision {
            Decision::Allow => Answer::Allowed,
            Decision::Always => Answer::Always,
            Decision::Deny => Answer::Denied(String::new()),
        };
        self.chat.transcript.answer_approval(&id, answer);
        match self.chat.session.clone() {
            Some(session) if decision == Decision::Always && !rules.is_empty() => self.set(|p| {
                let kept = p.more.allowed.entry(session).or_default();
                for rule in rules {
                    if !kept.contains(&rule) {
                        kept.push(rule);
                    }
                }
            }),
            _ => Task::none(),
        }
    }

    /// Starts a run of conversation `key` on `prompt`.
    pub(crate) fn start(&mut self, key: u64, prompt: String) -> Task<Msg> {
        self.start_run(key, prompt, false)
    }

    /// Whether `text` names one of the slash commands the CLI runs: `/name`, alone or with text.
    pub(crate) fn is_slash(&self, text: &str) -> bool {
        let Some(word) = text.trim_start().strip_prefix('/') else {
            return false;
        };
        let name = word.split(char::is_whitespace).next().unwrap_or_default();
        !name.is_empty() && self.slash_commands().iter().any(|c| c.name == name)
    }

    /// Starts a run of conversation `key` on `prompt`; with `slash`, a slash command it runs.
    pub(crate) fn start_run(&mut self, key: u64, prompt: String, slash: bool) -> Task<Msg> {
        let now = Instant::now();
        // An automation's run goes in its own mode, and is not the user's choice either; what is
        // sent after it in that conversation is.
        let own_mode = self.chat_mut(key).and_then(|c| c.mode.take());
        let automated = own_mode.is_some();
        let mode = own_mode.or(self.permission);
        // One of several the message went to keeps its own model.
        let own = std::iter::once(&self.chat)
            .chain(&self.parked)
            .find(|c| c.key == key)
            .and_then(|c| c.model.clone());
        let model = own.as_deref().unwrap_or(self.model.trim());
        let mut spec = RunSpec {
            program: self.program.clone(),
            cwd: self.cwd.clone(),
            prompt: String::new(),
            resume: None,
            model: (!model.is_empty()).then(|| model.to_owned()),
            permission: mode.map(str::to_owned),
            thinking: self.effort.map(str::to_owned),
            no_session: key == SIDE,
            lang: self.prefs.more.lang.clone(),
            allow: Vec::new(),
            trust_project: self.trust_once,
            slash,
            system_file: None,
        };
        // What "always" allowed in its earlier runs.
        let session = std::iter::once(&self.chat)
            .chain(&self.parked)
            .find(|c| c.key == key)
            .and_then(|c| c.session.as_ref());
        if key != SIDE
            && let Some(rules) = session.and_then(|id| self.prefs.more.allowed.get(id))
        {
            spec.allow = rules.clone();
        }
        let plan = key != SIDE && mode == Some(crate::plan::MODE);
        if plan {
            crate::plan::shape(&mut spec, self.plan_file());
        }
        if key == SIDE {
            // A side question reads; what would change something is asked, and denied.
            spec.permission = Some("ask".into());
        }
        if key != SIDE && !automated {
            self.keep_choices();
            if let Some(draft) = self.draft_key_of(key) {
                self.forget_draft(draft);
            }
        }
        let shown = key == self.chat.key;
        if self.suggestion.as_ref().is_some_and(|(k, _)| *k == key) {
            self.suggestion = None;
        }
        let (repo, cwd) = (self.git.repo, self.cwd.clone());
        let Some(chat) = self.chat_mut(key) else {
            return Task::none();
        };
        chat.transcript.send(&prompt, now);
        chat.newer = false;
        chat.log_seen = None;
        chat.loaded = false;
        spec.prompt = prompt;
        spec.resume = chat.session.clone();
        chat.planned = false;
        chat.turn_from = None;
        chat.turn_to = None;
        let (task, handle) = Task::run(runner::run(spec), move |update| Msg::Run(key, update)).abortable();
        // The working tree is taken before the run can change it, for the git tab's last turn.
        let task = if key != SIDE && repo {
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || crate::git::snapshot(&cwd))
                        .await
                        .ok()
                        .flatten()
                },
                move |tree| Msg::TurnTree(key, true, tree),
            )
            .chain(task)
        } else {
            task
        };
        chat.run = Some(Run {
            control: None,
            _handle: Some(handle.abort_on_drop()),
            stderr: Vec::new(),
            ended: false,
            plan,
        });
        self.now = now;
        self.hold_awake();
        if !shown {
            return task;
        }
        // What was just sent is in view, however far back the reader was.
        Task::batch([task, self.back_to_newest()])
    }

    /// What the run of conversation `key` said. A conversation not shown is marked for what the
    /// user should come back to: its run ended, or it waits for an answer.
    fn on_run(&mut self, key: u64, update: RunUpdate) -> Task<Msg> {
        let shown = key == self.chat.key || key == SIDE;
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
        // What the system is told when the window is behind others.
        let mut news: Option<&'static str> = None;
        let mut git = false;
        // The run's process is gone: what the user may say next can be guessed.
        let mut ended_now = false;
        // A conversation's id, once its run names it: the branch it was started on is kept.
        let mut named = None;
        // A plugin's notice for the toast, and the slash commands the plugins offer now.
        let mut notice = None;
        let mut commands = None;
        // The conversation whose log now holds a summary to show.
        let mut compacted: Option<String> = None;
        // A stderr line, for the window's log.
        let mut logged: Option<String> = None;
        // The model and the message of one of several a message went to, once it has an id.
        let mut named_by: Option<(String, String)> = None;
        match update {
            RunUpdate::Started(control) => run.control = Some(control),
            RunUpdate::Event(event) => {
                let effect = chat.transcript.apply(&event, now);
                if let Some(id) = effect.session_id {
                    chat.session = Some(id.clone());
                    named = Some(id);
                    named_by = chat.model.clone().zip(chat.transcript.title().map(str::to_owned));
                }
                if let Some(cause) = effect.ended {
                    run.ended = true;
                    // A plan written: offered to go ahead with.
                    chat.planned = run.plan && cause == StopCause::Done;
                    chat.unread |= !shown;
                    news = Some(if cause == StopCause::Done {
                        s.n_done
                    } else {
                        s.n_stopped
                    });
                }
                let plan_denies = effect.approval.as_ref().is_some_and(|id| {
                    run.plan
                        && chat
                            .transcript
                            .approval_by_id(id)
                            .is_some_and(|a| crate::plan::denies(&a.tool))
                });
                if let Some(id) = effect.approval {
                    if plan_denies {
                        // Plan mode: nothing is written until the plan is gone ahead with.
                        let said = s.w().plan_denied.to_owned();
                        chat.control(ControlMessage::Approval {
                            id: id.clone(),
                            decision: Decision::Deny,
                            feedback: Some(said.clone()),
                            args: None,
                        });
                        chat.transcript.answer_approval(&id, Answer::Denied(said));
                    } else if key == SIDE {
                        // Nothing a side question does is let change anything.
                        let feedback = Some(s.side_denied.to_owned());
                        chat.control(ControlMessage::Approval {
                            id: id.clone(),
                            decision: Decision::Deny,
                            feedback: feedback.clone(),
                            args: None,
                        });
                        chat.transcript
                            .answer_approval(&id, Answer::Denied(s.side_denied.to_owned()));
                    } else {
                        chat.unread |= !shown;
                        attention = true;
                        news = Some(s.n_waiting);
                    }
                }
                if let Some(id) = effect.asked {
                    if key == SIDE {
                        // Answered as a headless run answers: a side question asks nobody.
                        chat.transcript.asks.retain(|a| a.id != id);
                        chat.control(ControlMessage::UiResponse {
                            id,
                            value: serde_json::Value::Null,
                        });
                    } else {
                        chat.unread |= !shown;
                        attention = true;
                        news = Some(s.n_waiting);
                    }
                }
                if shown {
                    notice = effect.notice.map(|(_, text)| text);
                }
                chat.newer |= effect.commands.is_some();
                if effect.compacted {
                    compacted = chat.session.clone();
                }
                commands = effect.commands;
                // A new conversation's log is there once its first message is: the list shows it.
                relist = matches!(*event, sasacode_protocol::Event::MessageEnd { .. }) && chat.session.is_some();
            }
            RunUpdate::Stderr(line) => {
                // The config's warnings, and others the CLI gives, are said in the conversation.
                if let Some(warning) = crate::transcript::warning_of(&line) {
                    chat.transcript.warn(warning);
                }
                logged = Some(line.clone());
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
                news = Some(s.n_stopped);
            }
            RunUpdate::Exited(code) => {
                ended_now = true;
                // A summary the log did not hold yet when the run said it compacted.
                if chat.transcript.summary_missing() {
                    compacted = chat.session.clone();
                }
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
                } else if !chat.follow.is_empty()
                    && chat
                        .transcript
                        .exchanges
                        .last()
                        .and_then(|ex| ex.end.as_ref())
                        .is_some_and(|end| end.cause == StopCause::Done)
                {
                    // Then the follow-ups, one run each; not after a run that was stopped or failed.
                    next = Some(chat.follow.remove(0));
                }
                self.reload_sessions();
                // What the run made or removed is offered after `@` from now on, and shown in the
                // git tab.
                self.files_asked = false;
                git = true;
            }
        }
        if relist
            && let Some(id) = self.chat_mut(key).and_then(|c| c.session.clone())
            && !self.sessions.iter().any(|s| s.id == id)
        {
            self.reload_sessions();
        }
        self.now = now;
        if let Some(line) = logged {
            self.log.push_back(line);
            if self.log.len() > LOG_LINES {
                self.log.pop_front();
            }
        }
        if let Some(text) = notice {
            self.toast = Some(text);
        }
        if let Some(id) = compacted
            && let Some(log) = self.sessions.iter().find(|s| s.id == id).map(|s| s.path.clone())
            && let Ok(entries) = sasacode_protocol::read_entries(&log)
            && let Some(chat) = self.chat_mut(key)
        {
            chat.transcript.fill_summary(&entries);
        }
        if let Some(commands) = commands {
            self.plugin_commands = commands;
        }
        if let (Some(id), Some((model, prompt))) = (&named, named_by)
            && !self.prefs.names.contains_key(id)
        {
            // Named by its model, so the list tells the several apart.
            let short = model.rsplit('/').next().unwrap_or(&model).to_owned();
            let first: String = prompt.lines().next().unwrap_or("").chars().take(40).collect();
            let name = format!("{first} · {short}");
            let id = id.clone();
            let _ = self.set(|p| {
                p.names.insert(id, name);
            });
        }
        if let Some(id) = named
            && key != SIDE
            && let Some(branch) = self.git.branch.clone()
            && !self.prefs.branches.contains_key(&id)
        {
            let _ = self.set(|p| {
                p.branches.insert(id, branch);
            });
        }
        if let Some(body) = news
            && (!self.focused || self.prefs.more.notify_always)
        {
            self.tell(key, body);
            attention = true;
        }
        self.hold_awake();
        let mut tasks = Vec::new();
        // The branch, the git tab's count and its files, as the run left them; and the working
        // tree taken as the turn ended, for the git tab's last turn.
        if git {
            tasks.push(self.refresh_git());
            if key != SIDE && self.git.repo {
                let cwd = self.cwd.clone();
                tasks.push(Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || crate::git::snapshot(&cwd))
                            .await
                            .ok()
                            .flatten()
                    },
                    move |tree| Msg::TurnTree(key, false, tree),
                ));
            }
        }
        let again = next.is_some();
        if let Some(prompt) = next {
            tasks.push(self.start(key, prompt));
        } else if ended_now {
            tasks.push(self.guess_next(key));
            // A conversation's first run ended: it may get a title of the model's.
            if key != SIDE
                && let Some(id) = self
                    .chat_mut(key)
                    .filter(|c| !c.scratch)
                    .and_then(|c| c.session.clone())
            {
                tasks.push(self.write_title(&id, false));
            }
        }
        // A scratch conversation left while it ran is gone, now that its run is over.
        if ended_now
            && !again
            && key != self.chat.key
            && let Some(i) = self.parked.iter().position(|c| c.key == key && c.scratch)
        {
            let chat = self.parked.remove(i);
            self.drop_scratch(&chat);
        }
        if attention && !again {
            // The run waits for the user or ended: say so even when the window is behind others
            // (on Windows, where no notification is sent, the taskbar button flashes).
            tasks.push(
                window::latest()
                    .and_then(|id| window::request_user_attention(id, Some(window::UserAttention::Informational))),
            );
        }
        Task::batch(tasks)
    }
}

/// Gives the keyboard to the input.
fn focus_input() -> Task<Msg> {
    iced::widget::operation::focus(INPUT)
}

/// Keys no part of the window took for itself.
pub(crate) fn global_key(event: iced::Event, status: event::Status, _window: window::Id) -> Option<Msg> {
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    if status == event::Status::Captured {
        // A field that let go of the keyboard on Esc, the search or the model, gives it back to
        // the input: the next Esc is the input's again.
        return matches!(key, Key::Named(Named::Escape)).then_some(Msg::Escaped);
    }
    if modifiers.is_empty() {
        match key {
            Key::Named(Named::ArrowUp) => return Some(Msg::Arrow(-1)),
            Key::Named(Named::ArrowDown) => return Some(Msg::Arrow(1)),
            _ => {}
        }
    }
    shortcut(&key, modifiers)
}

/// A key pressed while a shortcut's new key is waited for: any but a modifier alone.
fn capture_key(event: iced::Event, _status: event::Status, _window: window::Id) -> Option<Msg> {
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    if matches!(
        key,
        Key::Named(Named::Shift | Named::Control | Named::Alt | Named::Super | Named::Meta)
    ) {
        return None;
    }
    Some(Msg::KeyCaptured(key, modifiers))
}

/// What the window hears of itself: whether it is in front, where it was moved, and files held over
/// it or dropped.
fn window_event(event: iced::Event, _status: event::Status, _window: window::Id) -> Option<Msg> {
    match event {
        iced::Event::Window(window::Event::Focused) => Some(Msg::Focus(true)),
        iced::Event::Window(window::Event::Unfocused) => Some(Msg::Focus(false)),
        iced::Event::Window(window::Event::FileHovered(_)) => Some(Msg::Hover(true)),
        iced::Event::Window(window::Event::FilesHoveredLeft) => Some(Msg::Hover(false)),
        iced::Event::Window(window::Event::Moved(at)) => Some(Msg::Moved(at)),
        iced::Event::Window(window::Event::FileDropped(path)) => Some(Msg::Dropped(path)),
        iced::Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => Some(Msg::Modifiers(m)),
        _ => None,
    }
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
    // ⌃Tab and ⌃⇧Tab: the next and the previous conversation, as tabs are gone through.
    if modifiers.control() && matches!(key, Key::Named(Named::Tab)) {
        return Some(Msg::Neighbour(if modifiers.shift() { -1 } else { 1 }));
    }
    if !modifiers.command() {
        return None;
    }
    if let Key::Named(Named::Enter) = key.as_ref() {
        return Some(Msg::CommandEnter {
            shift: modifiers.shift(),
        });
    }
    // ⌘Z, which no setting takes: a change of the list taken back, while no field has the keyboard.
    if let Key::Character(c) = key.as_ref()
        && c.eq_ignore_ascii_case("z")
        && !modifiers.shift()
        && !modifiers.alt()
    {
        return Some(Msg::UndoKey);
    }
    // ⌘G and ⌘⇧G: the next and the last of what the search in the conversation finds.
    if let Key::Character(c) = key.as_ref()
        && c.eq_ignore_ascii_case("g")
        && !modifiers.alt()
    {
        return Some(Msg::FindStep(if modifiers.shift() { -1 } else { 1 }));
    }
    // The others as bound: by default, or as the settings say.
    let chord = crate::keys::Chord::of(key, modifiers)?;
    crate::keys::bound().act(&chord).map(crate::keys::Act::msg)
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

    #[test]
    fn a_zoomed_window_is_fitted_to_the_screen_as_iced_measures_both() {
        let mut app = crate::snapshots::window("ja");
        app.prefs.zoom = 1.25;
        // 1250 by 875 on a screen of 1512 by 982, both as iced gives them at this zoom.
        app.width = 1000.0;
        app.height = 700.0;
        assert_eq!(app.fitted(Size::new(1512.0 / 1.25, 982.0 / 1.25)), None);
        // Taller than the screen: as tall as it, measured at the zoom again.
        app.height = 900.0;
        let fit = app.fitted(Size::new(1512.0 / 1.25, 982.0 / 1.25)).unwrap();
        assert!(
            (fit.width - 1000.0).abs() < 0.01 && (fit.height - 785.6).abs() < 0.01,
            "{fit:?}"
        );
    }

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
            prefs: crate::layout::Prefs::default(),
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
    fn messages_sent_while_it_runs_wait_their_turn_when_the_settings_say_so() {
        let (mut app, home) = window_with("follow", &["aaaa0000", "bbbb0000"]);
        assert!(app.open_named("aaaa0000"));
        let _ = app.update(Msg::SetQueue(true));
        let mut written = app.pretend_running();
        let key = app.chat.key;
        let _ = app.update(Msg::Run(key, event(r#"{"type":"agent_start"}"#)));
        type_in(&mut app, "一つ目");
        let _ = app.update(Msg::Send);
        type_in(&mut app, "二つ目");
        let _ = app.update(Msg::Send);
        assert_eq!(app.chat.follow, vec!["一つ目", "二つ目"]);
        assert!(written.try_recv().is_err(), "nothing reached the run");
        let _ = app.update(Msg::FollowUp(1));
        assert_eq!(app.chat.follow, vec!["二つ目", "一つ目"]);
        let _ = app.update(Msg::FollowDrop(1));
        assert_eq!(app.chat.follow, vec!["二つ目"]);

        // It ends well: the next starts, as a run of its own.
        let before = app.chat.transcript.exchanges.len();
        let _ = app.update(Msg::Run(key, event(r#"{"type":"agent_end","cause":"done"}"#)));
        let _ = app.update(Msg::Run(key, RunUpdate::Exited(Some(0))));
        assert!(app.chat.follow.is_empty() && app.busy());
        assert_eq!(app.chat.transcript.exchanges.len(), before + 1);
        assert_eq!(app.chat.transcript.exchanges.last().unwrap().prompt, "二つ目");

        // Stopped, it starts none; and "send now" reaches the run at its next turn.
        let mut written = app.pretend_running();
        type_in(&mut app, "三つ目");
        let _ = app.update(Msg::Send);
        let _ = app.update(Msg::FollowNow(0));
        assert!(matches!(written.try_recv(), Ok(ControlMessage::Message { text }) if text == "三つ目"));
        type_in(&mut app, "四つ目");
        let _ = app.update(Msg::Send);
        let _ = app.update(Msg::Run(key, event(r#"{"type":"agent_end","cause":"aborted"}"#)));
        let _ = app.update(Msg::Run(key, RunUpdate::Exited(Some(0))));
        assert_eq!(app.chat.follow, vec!["四つ目"]);

        // `!command` runs in the folder's shell, not as a message.
        let runs = app.script_runs.len();
        type_in(&mut app, "!echo hi");
        let _ = app.update(Msg::Send);
        assert_eq!(app.script_runs.len(), runs + 1);
        assert_eq!(app.script_runs[0].name, "!echo hi");
        assert!(app.chat.editor.text().trim().is_empty());

        // Another conversation goes in the input as context, written as Markdown.
        let _ = app.update(Msg::AttachConversation("bbbb0000".into()));
        let typed = app.chat.editor.text();
        let file = home.join("context").join("bbbb0000.md");
        assert!(file.is_file() && typed.contains("bbbb0000.md"), "{typed}");
        assert!(std::fs::read_to_string(&file).unwrap().contains("## "));
        std::fs::remove_dir_all(home).unwrap();
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
    fn a_conversation_branched_off_after_a_reply_holds_it_through_that_reply() {
        let (mut app, home) = window_with("fork-after", &["aaaa0000"]);
        assert!(app.open_named("aaaa0000"));
        let count = app.chat.transcript.exchanges.len();
        assert!(count >= 2, "the fixture has {count} exchanges");

        // After the first reply: that exchange alone, nothing typed, the old one listed still.
        let _ = app.update(Msg::ForkAfter(0));
        assert_ne!(app.chat.session.as_deref(), Some("aaaa0000"));
        assert_eq!(app.chat.transcript.exchanges.len(), 1);
        assert!(app.chat.editor.text().trim().is_empty());
        assert_eq!(app.sessions.len(), 2);

        // After the last: all of it.
        assert!(app.open_named("aaaa0000"));
        let _ = app.update(Msg::ForkAfter(count - 1));
        assert_eq!(app.chat.transcript.exchanges.len(), count);
        assert_eq!(app.sessions.len(), 3);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_conversation_branched_off_before_a_message_keeps_what_came_before() {
        let (mut app, home) = window_with("fork", &["aaaa0000"]);
        assert!(app.open_named("aaaa0000"));
        let count = app.chat.transcript.exchanges.len();
        assert!(count >= 2, "the fixture has {count} exchanges");
        let last = app.chat.transcript.exchanges[count - 1].prompt.clone();

        // Edit and resend: a new conversation, the message in its input, the old one listed still.
        let _ = app.update(Msg::Fork(count - 1));
        let id = app.chat.session.clone().unwrap();
        assert_ne!(id, "aaaa0000");
        assert_eq!(app.chat.transcript.exchanges.len(), count - 1);
        assert_eq!(app.chat.editor.text().trim(), last.trim());
        assert_eq!(app.sessions.len(), 2);

        // A message waiting for the run to end is taken back into the input.
        app.chat.editor = text_editor::Content::new();
        app.chat.next = Some("あとで".into());
        let _ = app.update(Msg::Retract);
        assert!(app.chat.next.is_none());
        assert_eq!(app.chat.editor.text().trim(), "あとで");
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
