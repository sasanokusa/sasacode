//! The interactive terminal UI (`app.ts`): the transcript, the input box (or a dialog in its
//! place), the status line and the footer, wired to the agent's events, the plugins' UI requests
//! and the core slash commands.
//!
//! One task owns everything on screen (the [`App`]). Whatever happens elsewhere (an agent event, a
//! plugin asking a question, a command's next step, a paste a plugin answered) reaches it as a
//! closure through one channel, so the screen is only touched from that task and nothing is
//! awaited while it holds the state. Commands and other flows that wait on the user run as their
//! own tasks and talk to the app through a [`Handle`].

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use crossterm::event::{KeyEvent, KeyEventKind, KeyModifiers};
use futures::future::{BoxFuture, FutureExt, Shared};
use futures::stream::{BoxStream, StreamExt};
use regex::Regex;
use tokio::sync::{mpsc, oneshot};

use sasacode_agent::agent::Agent;
use sasacode_agent::events::{AgentEvent, Subscription};
use sasacode_agent::permission::{PERMISSION_MODES, permission_mode_label};
use sasacode_agent::plugins::PluginHost;
use sasacode_agent::session::SessionEntry;
use sasacode_agent::tool_exec::{ApprovalAnswer, ApprovalDecision, ApprovalRequest};
use sasacode_ai::format::fmt_tokens;
use sasacode_ai::{Message, ModelInfo, StopReason, StreamEvent, ThinkingLevel, ToolCall, UserContent, text_of_user};
use sasacode_host::{current_lang, list_sessions, note_text, retranslate, set_lang, t};
use sasacode_plugin_api::{
    Command, CommandContext, InputOptions, Lang, NotifyLevel, PermissionMode, SelectManyOptions, SelectOption,
    ShowTextOptions, StopCause, UiBridge,
};

use crate::activity::Activity;
use crate::ambiguous::match_ambiguous_width;
use crate::clipboard::{Copied, copy_to_clipboard};
use crate::dialogs::{ApprovalDialog, CheckList, Dialog, InputDialog, Picker, Rows};
use crate::jsfmt::to_fixed;
use crate::keys::{InputEvent, Keybindings, matches_key};
use crate::summary::{UsageTally, exit_summary};
use crate::term::alt::{DockPart, MouseOutcome};
use crate::term::screen::Screen;
use crate::term::{ProcessTerminal, Terminal};
use crate::text::{graphemes, truncate_to_width, utf16_len};
use crate::text_viewer::{TextViewer, ViewerAction};
use crate::theme::{self, EDITOR_THEME};
use crate::ui::Component;
use crate::ui::Spacer;
use crate::ui::autocomplete::{AutocompleteProvider, CommandEntry};
use crate::ui::editor::{ArgumentRequest, Editor};
use crate::ui::select_list::SelectItem;
use crate::views::{AssistantView, NoteView, Notice, Padded, ToolHandle, ToolStatus, ToolView, UserView, plain};

// ── what the TUI needs from the CLI ─────────────────────────────────

/// The `tui` part of the configuration, and the models listed in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiConfig {
    /// Models offered by /model besides the providers' own lists.
    pub models: Vec<String>,
    /// Full screen (the default); `false` draws inline below the shell prompt.
    pub alt_screen: bool,
    /// The terminal bell after a run of 30 seconds or more.
    pub bell: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            models: Vec::new(),
            alt_screen: true,
            bell: true,
        }
    }
}

/// A model a provider lists. `max_context` without `context_window`: the size the server will use
/// is not known yet (`server` says why).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListedModel {
    pub spec: String,
    pub context_window: Option<u64>,
    pub max_context: Option<u64>,
    pub server: Option<String>,
}

/// What the TUI needs from the CLI (`TuiHost`).
#[async_trait]
pub trait TuiHost: Send + Sync {
    fn agent(&self) -> Agent;
    fn host(&self) -> Arc<PluginHost>;
    /// Shown at the top of the transcript.
    fn warnings(&self) -> Vec<String> {
        Vec::new()
    }
    fn config(&self) -> TuiConfig;
    /// "<provider>/<model>" into the model to switch to.
    fn resolve(&self, spec: &str) -> anyhow::Result<ModelInfo>;
    fn sessions_dir(&self) -> PathBuf;
    /// Where sent prompts are kept for ↑ (JSON lines, the last 200 read).
    fn history_path(&self) -> Option<PathBuf> {
        None
    }
    async fn list_models(&self, refresh: bool) -> anyhow::Result<Vec<ListedModel>>;
    /// Load the plugins (in the background, after the UI is up), with this bridge as their UI.
    async fn load_plugins(&self, ui: Arc<dyn UiBridge>) -> anyhow::Result<()>;
    async fn load_session(&self, path: &Path) -> anyhow::Result<()>;
    async fn new_session(&self) -> anyhow::Result<()>;
    /// A new session with the conversation before message `index`.
    async fn fork(&self, index: usize) -> anyhow::Result<()>;
    async fn shutdown(&self);
    /// Keep the interface language for the next start (the `lang` setting in config.json).
    fn save_lang(&self, _lang: Lang) -> anyhow::Result<()> {
        Ok(())
    }
    /// Everything said in the session loaded, also what a compaction summarized (shown on resume).
    fn transcript(&self) -> Option<Vec<Message>> {
        None
    }
}

/// Run the UI on the process's terminal until the user leaves; the exit code.
pub async fn run_tui(host: Arc<dyn TuiHost>, initial_prompt: String) -> anyhow::Result<i32> {
    theme::set_no_color(theme::no_color_from_env());
    run_tui_with(host, initial_prompt, Box::new(ProcessTerminal::new())).await
}

/// [`run_tui`] on a given terminal (tests drive the app through a stand-in).
pub async fn run_tui_with(
    host: Arc<dyn TuiHost>,
    initial_prompt: String,
    terminal: Box<dyn Terminal>,
) -> anyhow::Result<i32> {
    let (tx, rx) = mpsc::unbounded_channel();
    let app = App::new(host, terminal, tx);
    app.run(initial_prompt, rx).await
}

// ── constants ───────────────────────────────────────────────────────

const EFFORT_LABELS: [(ThinkingLevel, &str); 6] = [
    (ThinkingLevel::Off, "推論しない"),
    (ThinkingLevel::Low, "浅く・速く"),
    (ThinkingLevel::Medium, "標準"),
    (ThinkingLevel::High, "深く（既定）"),
    (ThinkingLevel::Xhigh, "さらに深く"),
    (ThinkingLevel::Max, "最大（対応していないモデルでは xhigh 相当）"),
];

fn effort_label(level: ThinkingLevel) -> String {
    let ja = EFFORT_LABELS
        .iter()
        .find(|(l, _)| *l == level)
        .map(|(_, s)| *s)
        .unwrap_or("");
    t(ja, &[])
}

/// Columns left free at both sides of everything on screen.
const MARGIN: usize = 2;
/// Runs at least this long end with the terminal bell (tui.bell).
const BELL_AFTER_MS: i64 = 30_000;
/// How the agent marks a message sent after an interrupt; not part of what the user wrote.
const INTERRUPTED: &str = "[The user interrupted the previous response.]";
/// A paste up to this size is shown in the input as it is; a longer one is folded into a marker.
const SHOWN_PASTE_LINES: usize = 40;
const SHOWN_PASTE_CHARS: usize = 4000;
const FRAME: Duration = Duration::from_millis(16);

/// An image in the input, and how a sent message names one (the image-attach plugin's forms).
static IMAGE_MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[Image #(\d+)\]").unwrap_or_else(|_| unreachable!()));
static IMAGE_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[Image (\d+): ([^\]\n]*)\]").unwrap_or_else(|_| unreachable!()));

const LANGUAGES: [(&str, &str); 2] = [("ja", "日本語"), ("en", "English")];

fn language_named(name: &str) -> Option<Lang> {
    match name {
        "ja" | "jp" | "japanese" | "日本語" => Some(Lang::Ja),
        "en" | "english" | "英語" => Some(Lang::En),
        _ => None,
    }
}

fn languages() -> Vec<SelectOption> {
    LANGUAGES.iter().map(|(v, l)| SelectOption::new(*v, *l)).collect()
}

/// The conversation search box's words, in the interface's language.
fn search_text() -> (String, String) {
    (t("会話内を検索", &[]), t("一致するものがありません", &[]))
}

/// At most `n` graphemes, the last one an ellipsis when cut.
fn clip(s: &str, n: usize) -> String {
    let all: Vec<&str> = graphemes(s).collect();
    if all.len() > n {
        format!("{}…", all[..n.saturating_sub(1)].concat())
    } else {
        s.to_string()
    }
}

/// `s.replace(/\s+/g, " ")`.
fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

/// A model's maximum, when the size the server will give it is not known yet, and why.
fn max_label(server: Option<&str>, size: &str) -> String {
    match server {
        Some("ollama") => t("最大 {size}（num_ctx 未設定）", &[("size", &size)]),
        Some("lmstudio") => t("最大 {size}（未ロード）", &[("size", &size)]),
        _ => t("最大 {size}", &[("size", &size)]),
    }
}

fn parse_mode(s: &str) -> Option<PermissionMode> {
    PERMISSION_MODES.into_iter().find(|m| m.as_str() == s)
}

fn level_color(level: NotifyLevel) -> fn(&str) -> String {
    match level {
        NotifyLevel::Error => theme::red,
        NotifyLevel::Warning => theme::yellow,
        NotifyLevel::Info => theme::gray,
    }
}

/// `toLocaleString()` of an epoch-ms time, in the interface's language.
fn local_time(ms: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(ms).single() {
        Some(d) if current_lang() == Lang::En => d.format("%-m/%-d/%Y, %-I:%M:%S %p").to_string(),
        Some(d) => d.format("%Y/%-m/%-d %-H:%M:%S").to_string(),
        None => String::new(),
    }
}

// ── the parts on screen ─────────────────────────────────────────────

/// A view shared between the transcript and the events that update it.
struct SharedView<T>(Arc<Mutex<T>>);

impl<T: Component> Component for SharedView<T> {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).render(width)
    }
    fn invalidate(&mut self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).invalidate();
    }
}

/// The header and the conversation.
struct Body {
    header: Notice,
    chat: Vec<(u64, Box<dyn Component + Send>)>,
}

impl Component for Body {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut out = self.header.render(width);
        for (_, c) in &mut self.chat {
            out.extend(c.render(width));
        }
        out
    }
    fn invalidate(&mut self) {
        self.header.invalidate();
        for (_, c) in &mut self.chat {
            c.invalidate();
        }
    }
}

/// The activity line and the messages queued for the next turn.
#[derive(Default)]
struct Status {
    activity: Option<Activity>,
    queued: Vec<Notice>,
}

impl Component for Status {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(a) = self.activity.as_mut() {
            out.extend(a.render(width));
        }
        for q in &mut self.queued {
            out.extend(q.render(width));
        }
        out
    }
}

/// A single truncated line whose text can change (the footer).
#[derive(Default)]
struct Line {
    text: String,
}

impl Component for Line {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.text
            .split('\n')
            .map(|l| truncate_to_width(l, width, "...", false))
            .collect()
    }
}

/// What has the input box's place.
enum Current {
    Editor,
    Dialog(u64, Box<dyn Dialog>),
    Viewer(u64, Box<TextViewer>, Option<oneshot::Sender<()>>),
}

struct Slot {
    editor: Editor,
    current: Current,
}

impl Component for Slot {
    fn render(&mut self, width: usize) -> Vec<String> {
        match &mut self.current {
            Current::Editor => self.editor.render(width),
            Current::Dialog(_, d) => d.render(width),
            Current::Viewer(_, v, _) => v.render(width),
        }
    }
    fn invalidate(&mut self) {
        if let Current::Viewer(_, v, _) = &mut self.current {
            v.invalidate();
        }
    }
}

/// A dialog waiting for its turn: made when it is shown (an approval obsolete by then is not).
enum Shown {
    Dialog(Box<dyn Dialog>),
    Viewer(Box<TextViewer>, oneshot::Sender<()>),
}
type Make = Box<dyn FnOnce(&mut App) -> Option<Shown> + Send>;

/// A message sent while the agent works, until it reaches the model: `content` is what the plugins
/// made of it.
struct Queued {
    id: u64,
    text: String,
    content: Option<Vec<UserContent>>,
}

/// The assistant reply streaming now, and the ids of its view and the spacer above it.
struct CurrentReply {
    view: Arc<Mutex<AssistantView>>,
    view_id: u64,
    spacer_id: u64,
}

// ── the handle flows use ────────────────────────────────────────────

type Msg = Box<dyn FnOnce(&mut App) + Send>;

/// How tasks reach the app.
#[derive(Clone)]
struct Handle {
    tx: mpsc::UnboundedSender<Msg>,
    host: Arc<dyn TuiHost>,
    agent: Agent,
    plugins: Arc<PluginHost>,
    full_screen: bool,
}

impl Handle {
    fn send(&self, f: impl FnOnce(&mut App) + Send + 'static) {
        let _ = self.tx.send(Box::new(f));
    }

    /// Run `f` on the app and get its result (None once the app is gone).
    async fn with<R: Send + 'static>(&self, f: impl FnOnce(&mut App) -> R + Send + 'static) -> Option<R> {
        let (tx, rx) = oneshot::channel();
        self.send(move |app| {
            let _ = tx.send(f(app));
        });
        rx.await.ok()
    }

    /// Notices from commands and plugins carry text from config, servers and models: shown plain.
    fn notify(&self, text: impl Into<String>, color: fn(&str) -> String) {
        let text = plain(&text.into());
        self.send(move |app| app.notify(&text, color));
    }

    /// Show a dialog after any earlier one closes; its answer.
    async fn ask<T: Send + 'static>(
        &self,
        make: impl FnOnce(&mut App, oneshot::Sender<T>) -> Option<Shown> + Send + 'static,
    ) -> Option<T> {
        let (tx, rx) = oneshot::channel();
        self.send(move |app| {
            app.enqueue(Box::new(move |app| make(app, tx)));
        });
        rx.await.ok()
    }

    async fn pick(&self, title: String, options: Vec<SelectOption>, max_visible: usize) -> Option<String> {
        self.ask(move |app, tx| {
            let items = options
                .into_iter()
                .map(|o| SelectItem::new(o.value, o.label, o.description))
                .collect();
            let done = Box::new(move |item: Option<SelectItem>| {
                let _ = tx.send(item.map(|i| i.value));
            });
            Some(Shown::Dialog(Box::new(Picker::new(
                &title,
                items,
                done,
                max_visible,
                Some(app.input_rows_fn()),
            ))))
        })
        .await
        .flatten()
    }

    async fn approval(&self, req: ApprovalRequest) -> ApprovalAnswer {
        let deny = || ApprovalAnswer::new(ApprovalDecision::Deny);
        if req.signal.is_cancelled() {
            return deny();
        }
        let signal = req.signal.clone();
        let (tx, rx) = oneshot::channel::<ApprovalAnswer>();
        let (id_tx, id_rx) = oneshot::channel();
        self.send(move |app| {
            let id = app.enqueue(Box::new(move |app| app.make_approval(req, tx)));
            let _ = id_tx.send(id);
        });
        let Ok(id) = id_rx.await else { return deny() };
        tokio::select! {
            answer = rx => answer.unwrap_or_else(|_| deny()),
            _ = signal.cancelled() => {
                self.send(move |app| app.cancel_dialog(id));
                deny()
            }
        }
    }
}

/// How plugins reach the user (api.ui).
struct Bridge(Handle);

#[async_trait]
impl UiBridge for Bridge {
    fn interactive(&self) -> bool {
        true
    }
    fn notify(&self, message: &str, level: NotifyLevel) {
        self.0.notify(message, level_color(level));
    }
    async fn show_text(&self, options: ShowTextOptions) {
        let _ = self
            .0
            .ask(move |app, tx: oneshot::Sender<()>| {
                let lang = app.plugins.api("core-commands").ui().lang();
                let rows = app.viewer_rows_fn();
                Some(Shown::Viewer(Box::new(TextViewer::new(&options, rows, lang)), tx))
            })
            .await;
    }
    async fn confirm(&self, title: &str, message: Option<&str>) -> bool {
        let title = match message {
            Some(m) => format!("{title}\n{m}"),
            None => title.to_string(),
        };
        let options = vec![
            SelectOption::new("yes", t("はい", &[])),
            SelectOption::new("no", t("いいえ", &[])),
        ];
        self.0.pick(title, options, 10).await.as_deref() == Some("yes")
    }
    async fn select(&self, title: &str, options: &[SelectOption]) -> Option<String> {
        self.0.pick(title.to_string(), options.to_vec(), 10).await
    }
    async fn input(&self, title: &str, options: &InputOptions) -> Option<String> {
        let title = title.to_string();
        let options = options.clone();
        self.0
            .ask(move |app, tx: oneshot::Sender<Option<String>>| {
                let done = Box::new(move |v: Option<String>| {
                    let _ = tx.send(v);
                });
                Some(Shown::Dialog(Box::new(InputDialog::new(
                    &title,
                    options.message.as_deref(),
                    options.placeholder.as_deref(),
                    options.initial.as_deref(),
                    done,
                    Some(app.input_rows_fn()),
                ))))
            })
            .await
            .flatten()
    }
    async fn select_many(
        &self,
        title: &str,
        options: &[SelectOption],
        opts: &SelectManyOptions,
    ) -> Option<Vec<String>> {
        let title = title.to_string();
        let items: Vec<SelectItem> = options
            .iter()
            .map(|o| SelectItem::new(o.value.clone(), o.label.clone(), o.description.clone()))
            .collect();
        let selected = opts.selected.clone().unwrap_or_default();
        self.0
            .ask(move |app, tx: oneshot::Sender<Option<Vec<String>>>| {
                let done = Box::new(move |v: Option<Vec<String>>| {
                    let _ = tx.send(v);
                });
                Some(Shown::Dialog(Box::new(CheckList::new(
                    &title,
                    items,
                    &selected,
                    done,
                    10,
                    Some(app.input_rows_fn()),
                ))))
            })
            .await
            .flatten()
    }
}

// ── the app ─────────────────────────────────────────────────────────

struct App {
    handle: Handle,
    host: Arc<dyn TuiHost>,
    agent: Agent,
    plugins: Arc<PluginHost>,
    config: TuiConfig,
    screen: Screen,
    kb: Keybindings,
    transcript: Padded<Body>,
    status: Padded<Status>,
    input: Padded<Slot>,
    footer: Padded<Line>,
    /// Tool calls of the last reply that have not started yet (their permission checks are running).
    checking: Vec<ToolCall>,
    current: Option<CurrentReply>,
    tools: HashMap<String, ToolHandle>,
    queued: Vec<Queued>,
    /// Images of a message put back into the input (fork, interrupt), behind [Image #n] markers there.
    held: BTreeMap<u32, (String, UserContent)>,
    total_cost: f64,
    context_tokens: u64,
    usage: UsageTally,
    run_started: i64,
    last_ctrl_c: i64,
    fold_notice_shown: bool,
    warned_context: HashSet<String>,
    exiting: bool,
    finish: Option<i32>,
    ready: Shared<BoxFuture<'static, ()>>,
    dialogs: VecDeque<(u64, Make)>,
    next_id: u64,
    dirty: bool,
    input_rows: Arc<AtomicUsize>,
    viewer_rows: Arc<AtomicUsize>,
    subscription: Option<Subscription>,
}

impl App {
    fn new(host: Arc<dyn TuiHost>, terminal: Box<dyn Terminal>, tx: mpsc::UnboundedSender<Msg>) -> Self {
        let agent = host.agent();
        let plugins = host.host();
        let config = host.config();
        let full = config.alt_screen;
        let mut kb = Keybindings::new();
        // home/end stay with the input line; the transcript jumps with ctrl+home/end.
        if full {
            kb.set("tui.altScreen.top", &["ctrl+home"]);
            kb.set("tui.altScreen.bottom", &["ctrl+end"]);
        }
        let handle = Handle {
            tx,
            host: host.clone(),
            agent: agent.clone(),
            plugins: plugins.clone(),
            full_screen: full,
        };
        let mut editor = Editor::new(EDITOR_THEME, 1);
        editor.focused = true;
        let mut chat: Vec<(u64, Box<dyn Component + Send>)> = Vec::new();
        let mut next_id = 0;
        for w in host.warnings() {
            next_id += 1;
            chat.push((
                next_id,
                Box::new(Notice::colored(
                    &t("警告: {warning}", &[("warning", &plain(&w))]),
                    theme::yellow,
                )),
            ));
        }
        let mut app = App {
            handle,
            host: host.clone(),
            agent,
            plugins,
            config,
            screen: Screen::new(terminal, full),
            kb,
            // One margin for everything, as a terminal block would give it, with a blank line above
            // the transcript and below the footer on the full screen.
            transcript: Padded::with_rows(
                Body {
                    header: Notice::new(""),
                    chat,
                },
                MARGIN,
                MARGIN,
                usize::from(full),
                0,
            ),
            status: Padded::new(Status::default(), MARGIN, MARGIN),
            input: Padded::new(
                Slot {
                    editor,
                    current: Current::Editor,
                },
                MARGIN,
                MARGIN,
            ),
            footer: Padded::with_rows(Line::default(), MARGIN, MARGIN, 0, usize::from(full)),
            checking: Vec::new(),
            current: None,
            tools: HashMap::new(),
            queued: Vec::new(),
            held: BTreeMap::new(),
            total_cost: 0.0,
            context_tokens: 0,
            usage: UsageTally::new(),
            run_started: 0,
            last_ctrl_c: 0,
            fold_notice_shown: false,
            warned_context: HashSet::new(),
            exiting: false,
            finish: None,
            ready: futures::future::ready(()).boxed().shared(),
            dialogs: VecDeque::new(),
            next_id,
            dirty: true,
            input_rows: Arc::new(AtomicUsize::new(20)),
            viewer_rows: Arc::new(AtomicUsize::new(12)),
            subscription: None,
        };
        app.transcript.child.header = Notice::new(&app.header_text());
        app.apply_search_text();
        for h in app.load_history() {
            app.editor().add_to_history(&h);
        }
        app.render_history(true);
        app.update_footer();
        app
    }

    fn editor(&mut self) -> &mut Editor {
        &mut self.input.child.editor
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn add_chat(&mut self, c: Box<dyn Component + Send>) -> u64 {
        let id = self.id();
        self.transcript.child.chat.push((id, c));
        self.dirty = true;
        id
    }

    fn remove_chat(&mut self, id: u64) {
        self.transcript.child.chat.retain(|(i, _)| *i != id);
    }

    fn input_rows_fn(&self) -> Rows {
        let rows = self.input_rows.clone();
        Box::new(move || rows.load(Ordering::Relaxed))
    }

    fn viewer_rows_fn(&self) -> Rows {
        let rows = self.viewer_rows.clone();
        Box::new(move || rows.load(Ordering::Relaxed))
    }

    fn apply_search_text(&mut self) {
        let (placeholder, none) = search_text();
        if let Some(a) = self.screen.alt() {
            a.search_placeholder = placeholder;
            a.search_no_matches = none;
            a.indicator_label = format!(" {} ", t("↓ 最新へ (ctrl+end)", &[]));
        }
    }

    // ── the loop ────────────────────────────────────────────────────

    async fn run(mut self, initial_prompt: String, mut rx: mpsc::UnboundedReceiver<Msg>) -> anyhow::Result<i32> {
        // The agent's side: approvals, events and the plugins' changes come in through the channel.
        let h = self.handle.clone();
        self.agent.set_approve(Some(Arc::new(move |req: ApprovalRequest| {
            let h = h.clone();
            Box::pin(async move { Ok(h.approval(req).await) })
        })));
        let h = self.handle.clone();
        self.subscription = Some(self.agent.events().on(move |e: &AgentEvent| {
            let e = e.clone();
            h.send(move |app| app.on_event(e));
        }));
        let h = self.handle.clone();
        self.plugins.set_on_change(Some(Arc::new(move || {
            h.send(|app| {
                app.refresh_commands();
                app.update_footer();
                app.dirty = true;
            });
        })));
        // The core commands are registered through the public plugin API like any other (P2).
        let commands = core_commands(&self.handle);
        self.plugins
            .load("core-commands", move |api| {
                for c in commands {
                    api.register_command(c);
                }
                async { Ok(()) }.boxed()
            })
            .await;
        self.refresh_commands();
        match_ambiguous_width(self.screen.terminal());
        // Asked now (tmux is asked with a subprocess): never in the middle of a frame.
        crate::ui::markdown::hyperlinks();
        let mut input: Option<BoxStream<'static, InputEvent>> = Some(self.screen.start()?);

        // Plugins and MCP servers load in the background so input is never blocked.
        let (ready_tx, ready_rx) = oneshot::channel::<()>();
        self.ready = ready_rx.map(|_| ()).boxed().shared();
        let h = self.handle.clone();
        tokio::spawn(async move {
            let bridge: Arc<dyn UiBridge> = Arc::new(Bridge(h.clone()));
            if let Err(e) = h.host.load_plugins(bridge).await {
                h.notify(t("プラグインの読み込みに失敗: {error}", &[("error", &e)]), theme::red);
            }
            let _ = ready_tx.send(());
        });
        // The providers' model lists, in the background: /model is instant, and the current
        // model learns its real context window.
        let h = self.handle.clone();
        tokio::spawn(async move {
            if h.host.list_models(false).await.is_ok() {
                h.send(|app| app.update_footer());
                warn_context(&h).await;
            }
        });
        if !initial_prompt.trim().is_empty() {
            self.submit(initial_prompt);
        }

        let mut tick = tokio::time::interval(Duration::from_millis(crate::activity::TICK_MS));
        // Idle for minutes, then a run: one tick, not every missed one at once.
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_render = Instant::now() - Duration::from_secs(1);
        let code = loop {
            if crate::term::take_panic_restored() {
                self.screen.reenter();
                self.dirty = true;
            }
            if let Some(code) = self.finish {
                break code;
            }
            let mut render_at = None;
            if self.dirty {
                let due = last_render + FRAME;
                if Instant::now() >= due {
                    self.render_frame();
                    last_render = Instant::now();
                } else {
                    render_at = Some(due);
                }
            }
            let wake = self.screen.alt_ref().and_then(|a| a.next_wakeup());
            let ticking = self.status.child.activity.is_some();
            tokio::select! {
                ev = async { match input.as_mut() { Some(s) => s.next().await, None => std::future::pending().await } } => {
                    match ev {
                        Some(ev) => {
                            // What was already waiting (a command a plugin just registered, an
                            // agent event) goes first: the TS ran those callbacks before the next key.
                            while let Ok(msg) = rx.try_recv() {
                                msg(&mut self);
                            }
                            self.on_input(ev)
                        }
                        None => {
                            // The input is gone (stdin closed): nothing can be typed any more.
                            input = None;
                            self.exit(0);
                        }
                    }
                }
                msg = rx.recv() => {
                    if let Some(msg) = msg {
                        msg(&mut self);
                        // Everything already waiting goes into the same frame.
                        while let Ok(msg) = rx.try_recv() {
                            msg(&mut self);
                        }
                    }
                }
                _ = tick.tick(), if ticking => {
                    if let Some(a) = self.status.child.activity.as_mut() {
                        a.tick();
                        self.dirty = true;
                    }
                }
                _ = sleep_until(render_at), if render_at.is_some() => {}
                // A frame already due draws whatever the wake-up was for.
                _ = sleep_until(wake), if wake.is_some() && !self.dirty => { self.dirty = true; }
            }
        };
        if let Some(s) = self.subscription.take() {
            s.off();
        }
        self.plugins.set_on_change(None);
        // Full screen: leave without copying the transcript onto the normal screen (that is the point).
        self.screen.stop();
        let session = self.agent.session();
        let id = session
            .as_ref()
            .filter(|s| s.path().exists())
            .map(|s| s.id().to_string());
        // What was used, and the way back (the session is gone from the screen now).
        let summary = exit_summary(&self.usage, id.as_deref());
        self.screen.write(&format!("\r\x1b[2K{summary}"));
        Ok(code)
    }

    /// Leave: the host shuts down first, then the loop ends.
    fn exit(&mut self, code: i32) {
        if self.exiting {
            return;
        }
        self.exiting = true;
        let h = self.handle.clone();
        tokio::spawn(async move {
            h.host.shutdown().await;
            h.send(move |app| app.finish = Some(code));
        });
    }

    fn render_frame(&mut self) {
        self.dirty = false;
        let w = self.screen.columns();
        let h = self.screen.rows();
        let search = self.screen.alt_ref().is_some_and(|a| a.search_open());
        // The editor shows the cursor only while it has the keys (not under the search box).
        let slot = &mut self.input.child;
        slot.editor.terminal_rows = h;
        match &mut slot.current {
            Current::Editor => slot.editor.focused = !search,
            Current::Dialog(_, d) => d.set_focused(!search),
            Current::Viewer(..) => {}
        }
        let transcript = self.transcript.render(w);
        let status = self.status.render(w);
        let footer = self.footer.render(w);
        self.input_rows.store(
            (h as i64 - status.len() as i64 - footer.len() as i64 - 3).max(1) as usize,
            Ordering::Relaxed,
        );
        self.viewer_rows
            .store((h as i64 - 12).clamp(1, 24) as usize, Ordering::Relaxed);
        let input = self.input.render(w);
        let dock = [
            DockPart { lines: status, min: 0 },
            DockPart { lines: input, min: 3 },
            DockPart { lines: footer, min: 0 },
        ];
        self.screen.render(transcript, &dock, &self.kb);
    }

    fn full_redraw(&mut self) {
        self.screen.force_full();
        self.dirty = true;
    }

    // ── input ───────────────────────────────────────────────────────

    fn on_input(&mut self, ev: InputEvent) {
        self.dirty = true;
        match ev {
            InputEvent::Key(k) => {
                if k.kind == KeyEventKind::Release {
                    return;
                }
                if let Some(alt) = self.screen.alt() {
                    alt.clear_selection();
                    if alt.handle_key(&k, &self.kb) {
                        return;
                    }
                }
                if self.on_key(&k) {
                    return;
                }
                if let Some(alt) = self.screen.alt()
                    && alt.search_open()
                {
                    alt.search_key(&k, &self.kb);
                    return;
                }
                self.focused_key(&k);
            }
            InputEvent::Paste(text) => {
                if let Some(alt) = self.screen.alt()
                    && alt.search_paste(&text)
                {
                    return;
                }
                match &mut self.input.child.current {
                    Current::Editor => self.paste(text),
                    Current::Dialog(_, d) => d.handle_paste(&text),
                    Current::Viewer(_, v, _) => v.handle_paste(&text),
                }
            }
            InputEvent::Mouse(m) => {
                let Some(alt) = self.screen.alt() else { return };
                match alt.handle_mouse(&m) {
                    MouseOutcome::None | MouseOutcome::Render => {}
                    MouseOutcome::Copy(text) => {
                        let h = self.handle.clone();
                        tokio::spawn(async move {
                            let h2 = h.clone();
                            let how =
                                copy_to_clipboard(&text, move |seq| h2.send(move |app| app.screen.write(&seq))).await;
                            // Drag-selected text is copied on release; OSC 52 alone does nothing in many terminals.
                            let _ = how;
                            h.send(|app| {
                                if let Some(a) = app.screen.alt() {
                                    a.copied(true, None);
                                }
                                app.dirty = true;
                            });
                        });
                    }
                    MouseOutcome::DockClick { part, row, col } => {
                        if part == 1 && matches!(self.input.child.current, Current::Editor) {
                            let w = self.screen.columns().saturating_sub(2 * MARGIN).max(10);
                            let x = col.saturating_sub(MARGIN);
                            if let Some(item) = self.editor().click(x, row, w) {
                                let _ = item;
                            }
                            self.after_editor_key();
                        }
                    }
                }
            }
            InputEvent::Resize(..) => {}
            InputEvent::FocusGained | InputEvent::FocusLost => {}
        }
    }

    /// The app's own keys, after the full-screen view's and before the focused part's. True when
    /// the key was used.
    fn on_key(&mut self, k: &KeyEvent) -> bool {
        let search = self.screen.alt_ref().is_some_and(|a| a.search_open());
        // The editor may be shown while something else has the keys: the conversation search box, say.
        let editing = matches!(self.input.child.current, Current::Editor) && !search;
        if editing {
            if self.delete_marker(k) {
                return true;
            }
            // An image on the clipboard pastes nothing in most terminals; ctrl+v asks for it.
            if matches_key(k, "ctrl+v") && !self.plugins.paste_handlers().is_empty() {
                self.paste(String::new());
                return true;
            }
        }
        // The pager owns its navigation keys, including Esc; it must not stop an active agent.
        if matches!(self.input.child.current, Current::Viewer(..)) && !matches_key(k, "ctrl+c") {
            return false;
        }
        let running = self.agent.is_running();
        if matches_key(k, "escape") && running && editing && !self.input.child.editor.is_showing_autocomplete() {
            self.interrupt();
            return true;
        }
        if matches_key(k, "ctrl+c") {
            if running {
                self.interrupt();
            } else if !self.input.child.editor.get_text().is_empty() {
                self.editor().set_text("");
            } else if sasacode_ai::now_ms() - self.last_ctrl_c < 1500 {
                self.exit(0);
            } else {
                self.last_ctrl_c = sasacode_ai::now_ms();
                self.notify(&t("もう一度 ctrl+c で終了します", &[]), theme::gray);
            }
            return true;
        }
        if matches_key(k, "ctrl+d") && self.input.child.editor.get_text().is_empty() && !running {
            self.exit(0);
            return true;
        }
        if matches_key(k, "ctrl+o") {
            crate::views::set_expanded(!crate::views::expanded());
            self.transcript.invalidate();
            self.full_redraw();
            return true;
        }
        if matches_key(k, "shift+tab") && editing {
            let mode = self.agent.permissions().mode();
            let i = PERMISSION_MODES.iter().position(|m| *m == mode).unwrap_or(0);
            self.set_mode(PERMISSION_MODES[(i + 1) % PERMISSION_MODES.len()]);
            return true;
        }
        false
    }

    /// A key for whatever has the input box's place.
    fn focused_key(&mut self, k: &KeyEvent) {
        let kb = self.kb.clone();
        match &mut self.input.child.current {
            Current::Editor => {
                self.input.child.editor.handle_key(k, &kb);
                self.after_editor_key();
            }
            Current::Dialog(_, d) => {
                d.handle_key(k, &kb);
                if d.finished() {
                    self.close_dialog();
                }
            }
            Current::Viewer(_, v, _) => match v.handle_key(k, &kb) {
                ViewerAction::None => {}
                ViewerAction::Close => self.close_dialog(),
                ViewerAction::Copy(text) => {
                    let h = self.handle.clone();
                    tokio::spawn(async move {
                        let h2 = h.clone();
                        let how = copy_to_clipboard(&text, move |seq| h2.send(move |app| app.screen.write(&seq))).await;
                        h.send(move |app| {
                            if let Current::Viewer(_, v, _) = &mut app.input.child.current {
                                v.copied(Some(how));
                            }
                            app.dirty = true;
                        });
                    });
                }
            },
        }
    }

    /// What the editor did with a key: a submitted text, an argument completion to ask for.
    fn after_editor_key(&mut self) {
        if let Some(text) = self.editor().take_submitted() {
            self.submit(text);
        }
        if let Some(req) = self.editor().take_argument_request() {
            self.complete_argument(req);
        }
    }

    /// Ask the command for its argument's candidates. Answered at once when it can be (enter must
    /// not meet a list that is out of date: #29, #114); otherwise when it comes.
    fn complete_argument(&mut self, req: ArgumentRequest) {
        let cmd = self
            .plugins
            .commands()
            .into_iter()
            .find(|c| c.command.name == req.command)
            .and_then(|c| c.command.complete);
        let Some(complete) = cmd else {
            self.editor().resolve_argument(req, Vec::new());
            return;
        };
        let prefix = req.argument.clone();
        let fut = AssertUnwindSafe(complete(prefix.clone())).catch_unwind();
        // An argument typed out in full offers nothing more, for any plugin's command: enter then
        // runs it (with a list up, enter would only complete). Longer ones show again after another key.
        let finish = move |options: Vec<SelectOption>| -> Vec<SelectItem> {
            if options.iter().any(|o| o.value == prefix) {
                return Vec::new();
            }
            options
                .into_iter()
                .map(|o| SelectItem::new(o.value, o.label, o.description))
                .collect()
        };
        let mut fut = fut.boxed();
        match (&mut fut).now_or_never() {
            Some(r) => {
                let items = finish(r.unwrap_or_default());
                self.editor().resolve_argument(req, items);
            }
            None => {
                let h = self.handle.clone();
                tokio::spawn(async move {
                    let items = finish(fut.await.unwrap_or_default());
                    h.send(move |app| {
                        app.editor().resolve_argument(req, items);
                        app.dirty = true;
                    });
                });
            }
        }
    }

    /// Backspace or delete next to (or inside) an "[Image #1]" marker removes all of it, as the
    /// editor does for its own "[paste #1 +12 lines]": a marker with a character missing is no longer one.
    fn delete_marker(&mut self, k: &KeyEvent) -> bool {
        let back = matches_key(k, "backspace");
        if !back && !matches_key(k, "delete") {
            return false;
        }
        let (line, col) = self.input.child.editor.get_cursor();
        let text = self
            .input
            .child
            .editor
            .get_lines()
            .get(line)
            .cloned()
            .unwrap_or_default();
        for m in IMAGE_MARKER.find_iter(&text) {
            let (start, end) = (m.start(), m.end());
            let outside = if back {
                col <= start || col > end
            } else {
                col < start || col >= end
            };
            if outside {
                continue;
            }
            // Through the editor's own keys, so undo and the cursor stay right.
            let kb = self.kb.clone();
            let len = end - start;
            let keys: Vec<&str> = if back {
                std::iter::repeat_n("right", end - col)
                    .chain(std::iter::repeat_n("backspace", len))
                    .collect()
            } else {
                std::iter::repeat_n("left", col - start)
                    .chain(std::iter::repeat_n("delete", len))
                    .collect()
            };
            for key in keys {
                let ev = key_of(key);
                self.editor().handle_key(&ev, &kb);
            }
            return true;
        }
        false
    }

    /// Pasted text (or "" for ctrl+v) offered to the plugins; unclaimed text goes to the editor as a paste.
    fn paste(&mut self, text: String) {
        self.prune_held();
        let handlers = self.plugins.paste_handlers();
        if handlers.is_empty() {
            self.paste_into_editor(&text);
            return;
        }
        let draft = self.input.child.editor.get_text();
        let h = self.handle.clone();
        tokio::spawn(async move {
            for handle in handlers {
                let out = AssertUnwindSafe(handle(text.clone(), draft.clone()))
                    .catch_unwind()
                    .await;
                if let Ok(Some(out)) = out {
                    h.send(move |app| {
                        app.editor().insert_text_at_cursor(&out);
                        app.dirty = true;
                    });
                    return;
                }
            }
            h.send(move |app| app.paste_into_editor(&text));
        });
    }

    /// A held image whose marker left the draft (ctrl+c, deleted) is gone: its number is free for the
    /// image plugin to give a new paste, and must not send the old image in that one's place (#109).
    fn prune_held(&mut self) {
        let text = self.input.child.editor.get_text();
        let shown: HashSet<u32> = IMAGE_MARKER
            .captures_iter(&text)
            .filter_map(|c| c[1].parse().ok())
            .collect();
        self.held.retain(|n, _| shown.contains(n));
    }

    fn paste_into_editor(&mut self, text: &str) {
        self.dirty = true;
        if text.is_empty() {
            return;
        }
        // The editor folds a paste over 10 lines into "[paste #1 +12 lines]" (sent in full), which reads
        // like something else was pasted. A paste that fits on screen is shown as it is.
        let clean: String = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\t', "    ")
            .chars()
            .filter(|&c| !matches!(c, '\x00'..='\x09' | '\x0b'..='\x1f' | '\x7f'))
            .collect();
        if clean.split('\n').count() <= SHOWN_PASTE_LINES && utf16_len(&clean) <= SHOWN_PASTE_CHARS {
            self.editor().insert_text_at_cursor(&clean);
            return;
        }
        self.editor().handle_paste(text);
        if !self.fold_notice_shown {
            self.fold_notice_shown = true;
            self.notify(
                &t(
                    "長い貼り付けは [paste #番号 +行数] とまとめて表示します。送信すると全文が送られます",
                    &[],
                ),
                theme::gray,
            );
        }
    }

    /// The editor hands over text with paste markers already expanded, and clears itself.
    fn submit(&mut self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        self.editor().add_to_history(&text);
        self.save_history(&text);
        // Whatever comes back appears at the end: follow it again if the user had scrolled up.
        if let Some(a) = self.screen.alt() {
            a.scroll_to_bottom();
        }
        if text.starts_with('/') {
            let h = self.handle.clone();
            tokio::spawn(async move { run_command(&h, &text).await });
            return;
        }
        let id = self.id();
        if self.agent.is_running() {
            self.queued.push(Queued {
                id,
                text: text.clone(),
                content: None,
            });
            self.render_status();
        }
        let input = self.with_held(&text);
        let ready = self.ready.clone();
        let agent = self.agent.clone();
        let h = self.handle.clone();
        tokio::spawn(async move {
            ready.await;
            let accepted = Box::new(move |content: Option<Vec<UserContent>>| {
                h.send(move |app| match content {
                    Some(c) => {
                        if let Some(q) = app.queued.iter_mut().find(|q| q.id == id) {
                            q.content = Some(c);
                        }
                    }
                    None => {
                        // A plugin took it as a command: it will not reach the model.
                        if app.queued.iter().any(|q| q.id == id) {
                            app.queued.retain(|q| q.id != id);
                            app.render_status();
                        }
                    }
                });
            });
            let _ = agent.prompt(input, Some(accepted)).await;
        });
    }

    // ── agent events ────────────────────────────────────────────────

    fn on_event(&mut self, e: AgentEvent) {
        self.dirty = true;
        match e {
            // The model may have learned its real context window at the last turn's end.
            AgentEvent::TurnStart { .. } => self.update_footer(),
            AgentEvent::AgentStart => {
                self.run_started = sasacode_ai::now_ms();
                self.status.child.activity = Some(Activity::with_defaults(theme::cyan, theme::gray));
                self.render_status();
            }
            AgentEvent::MessageStart { .. } => {
                self.checking.clear();
                if let Some(a) = self.status.child.activity.as_mut() {
                    a.new_reply();
                }
                let view = Arc::new(Mutex::new(AssistantView::new(Some(self.agent.cwd()))));
                let spacer_id = self.add_chat(Box::new(Spacer(1)));
                let view_id = self.add_chat(Box::new(SharedView(view.clone())));
                self.current = Some(CurrentReply {
                    view,
                    view_id,
                    spacer_id,
                });
            }
            AgentEvent::Usage { model, usage, .. } => {
                // Subagents and side requests: in the cost and the summary, not in the context size.
                self.total_cost += usage.cost;
                self.usage.add_usage(&model, &usage, true);
                self.update_footer();
            }
            AgentEvent::MessageDiscarded { message } => {
                // A plugin asked for this response to be generated again.
                if let Some(c) = self.current.take() {
                    self.remove_chat(c.view_id);
                    self.remove_chat(c.spacer_id);
                }
                if let Some(a) = self.status.child.activity.as_mut() {
                    a.discard_reply();
                }
                if message.stop_reason == StopReason::Error {
                    self.notify(&t("接続が応答の途中で切れたため、やり直します", &[]), theme::yellow);
                }
            }
            AgentEvent::ToolRepaired { call, note, .. } => {
                if let Some(v) = self.tools.get(&call.id) {
                    lock(v).repair_note = Some(note);
                }
            }
            AgentEvent::MessageUpdate { message, event } => {
                self.update_current(&message);
                if let StreamEvent::ToolcallDelta { index, delta, .. } = &event
                    && let Some(c) = &self.current
                {
                    lock(&c.view).receiving(*index, utf16_len(delta));
                }
                match &event {
                    StreamEvent::TextDelta { delta, .. }
                    | StreamEvent::ThinkingDelta { delta, .. }
                    | StreamEvent::ToolcallDelta { delta, .. } => {
                        if let Some(a) = self.status.child.activity.as_mut() {
                            a.streamed(delta);
                        }
                    }
                    _ => {}
                }
            }
            AgentEvent::MessageEnd { message } => self.message_end(message),
            AgentEvent::ToolStart {
                call,
                summary,
                parent_call_id,
            } => {
                let v = self.tool_view(&call, parent_call_id.is_some());
                if let Some(v) = v {
                    let mut v = lock(&v);
                    v.status = ToolStatus::Running;
                    if !summary.is_empty() {
                        v.summary = summary;
                    }
                }
                self.checking.retain(|b| b.id != call.id);
                if let Some(a) = self.status.child.activity.as_mut() {
                    a.set_detail(Some(t("{tool} を実行中…", &[("tool", &call.name)])));
                }
            }
            AgentEvent::ToolUpdate { call, text, .. } => {
                if let Some(v) = self.tools.get(&call.id) {
                    let mut v = lock(v);
                    let live = format!("{}{text}", v.live);
                    v.live = keep_last_utf16(&live, 20_000);
                }
            }
            AgentEvent::ToolEnd {
                call,
                result,
                parent_call_id,
            } => {
                if let Some(v) = self.tool_view(&call, parent_call_id.is_some()) {
                    let mut v = lock(&v);
                    v.output = text_of_user(&result.content);
                    v.diff = result
                        .details
                        .as_ref()
                        .and_then(|d| d.get("diff"))
                        .and_then(|d| d.as_str())
                        .map(str::to_string);
                    v.status = if result.is_error == Some(true) {
                        ToolStatus::Error
                    } else {
                        ToolStatus::Done
                    };
                    v.result = Some(result);
                }
                self.show_checking();
            }
            AgentEvent::Error { error } => {
                self.add_chat(Box::new(Notice::colored(
                    &t("エラー: {error}", &[("error", &plain(&error))]),
                    theme::red,
                )));
            }
            AgentEvent::PluginError { plugin, hook, error } => {
                self.add_chat(Box::new(Notice::colored(
                    &t(
                        "プラグイン {plugin} の {hook} ハンドラでエラー: {error}",
                        &[
                            ("plugin", &plain(&plugin)),
                            ("hook", &plain(&hook)),
                            ("error", &plain(&error)),
                        ],
                    ),
                    theme::red,
                )));
            }
            AgentEvent::MessagesReplaced => {
                // A compaction: the model now gets a summary, but what was said stays on screen to
                // scroll back to. Anything else that swaps the history is redrawn.
                let first = self.agent.messages().first().map(Message::text).unwrap_or_default();
                if first.starts_with("[Summary of") {
                    self.context_tokens = 0; // the next reply tells
                    self.update_footer();
                } else {
                    self.reset_view(false);
                }
            }
            AgentEvent::AgentEnd { cause, stopped, .. } => {
                self.status.child.activity = None;
                // A run long enough to switch windows ends with the terminal bell (a badge or sound).
                if self.config.bell
                    && cause != StopCause::Aborted
                    && sasacode_ai::now_ms() - self.run_started >= BELL_AFTER_MS
                {
                    self.screen.write("\x07");
                }
                if cause == StopCause::Aborted {
                    self.add_chat(Box::new(Notice::colored(&t("中断しました", &[]), theme::yellow)));
                }
                if cause == StopCause::MaxTurns {
                    let h = self.handle.clone();
                    tokio::spawn(async move { offer_to_continue(&h).await });
                }
                // Only when compaction could not make room: the event itself comes before that.
                if cause == StopCause::ContextLimit {
                    self.add_chat(Box::new(Notice::colored(
                        &t(
                            "コンテキストの上限に達したため停止しました。/compact で要約するか、/clear で新しいセッションを始めてください。",
                            &[],
                        ),
                        theme::yellow,
                    )));
                }
                if cause == StopCause::Stopped
                    && let Some(s) = stopped
                {
                    self.add_chat(Box::new(Notice::colored(
                        &t(
                            "{plugin} が停止しました: {reason}",
                            &[("plugin", &s.plugin), ("reason", &s.reason)],
                        ),
                        theme::yellow,
                    )));
                }
                self.update_footer();
                self.render_status();
            }
            _ => {}
        }
    }

    /// The view of a tool call; a subagent's calls get their own in the transcript.
    fn tool_view(&mut self, call: &ToolCall, sub: bool) -> Option<ToolHandle> {
        if let Some(v) = self.tools.get(&call.id) {
            return Some(v.clone());
        }
        if !sub {
            return None;
        }
        let v: ToolHandle = Arc::new(Mutex::new(ToolView::new(call.clone())));
        self.tools.insert(call.id.clone(), v.clone());
        self.add_chat(Box::new(v.clone()));
        Some(v)
    }

    fn update_current(&mut self, message: &sasacode_ai::AssistantMessage) {
        let Some(view) = self.current.as_ref().map(|c| c.view.clone()) else {
            return;
        };
        let tools = &mut self.tools;
        let plugins = &self.plugins;
        lock(&view).update(message, &mut |call, v| {
            lock(v).renderer = plugins.renderer(&call.name);
            tools.insert(call.id.clone(), v.clone());
        });
    }

    fn message_end(&mut self, message: Message) {
        match message {
            Message::User(m) => {
                // Delivered: the message holds the very blocks the plugins made of it.
                if let Some(i) = self.queued.iter().position(|q| {
                    q.content
                        .as_ref()
                        .is_some_and(|c| !c.is_empty() && c.iter().all(|b| m.content.contains(b)))
                }) {
                    self.queued.remove(i);
                }
                self.add_user(&m.content);
                self.render_status();
            }
            Message::Assistant(m) => {
                self.update_current(&m);
                for b in &m.content {
                    if let sasacode_ai::AssistantContent::ToolCall(c) = b {
                        self.describe_tool(c);
                    }
                }
                // Until each call starts, permission checks run unseen: a judge asks a model.
                self.checking = if m.stop_reason == StopReason::ToolUse {
                    m.content
                        .iter()
                        .filter_map(|b| match b {
                            sasacode_ai::AssistantContent::ToolCall(c) => Some(c.clone()),
                            _ => None,
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                self.show_checking();
                if let Some(a) = self.status.child.activity.as_mut() {
                    a.reply_done(m.usage.output);
                }
                self.total_cost += m.usage.cost;
                self.usage.add(&m);
                let ctx = m.usage.input + m.usage.cache_read + m.usage.cache_write + m.usage.output;
                if ctx > 0 {
                    self.context_tokens = ctx;
                }
                if m.stop_reason == StopReason::Refusal {
                    self.add_chat(Box::new(Notice::colored(
                        &t("モデルが応答を拒否しました", &[]),
                        theme::yellow,
                    )));
                }
                if m.stop_reason == StopReason::MaxTokens {
                    self.add_chat(Box::new(Notice::colored(
                        &t("出力が max_tokens に達しました", &[]),
                        theme::yellow,
                    )));
                }
                self.current = None;
                self.update_footer();
            }
            Message::Tool(m) => {
                if let Some(v) = self.tools.get(&m.tool_call_id) {
                    let mut v = lock(v);
                    if v.status != ToolStatus::Done && v.status != ToolStatus::Error {
                        v.output = text_of_user(&m.content);
                        v.status = if m.is_error {
                            ToolStatus::Error
                        } else {
                            ToolStatus::Done
                        };
                    }
                }
                // A call refused or interrupted before it started has no tool_end.
                let waiting = self.checking.len();
                self.checking.retain(|b| b.id != m.tool_call_id);
                if self.checking.len() != waiting {
                    self.show_checking();
                }
            }
        }
    }

    /// What the run waits for: the next call's permission check, or the model.
    fn show_checking(&mut self) {
        let detail = self
            .checking
            .first()
            .map(|c| t("{tool} の実行前の確認中…", &[("tool", &c.name)]));
        if let Some(a) = self.status.child.activity.as_mut() {
            a.set_detail(detail);
        }
    }

    fn describe_tool(&mut self, call: &ToolCall) {
        let Some(view) = self.tools.get(&call.id) else { return };
        if call.raw_input.is_some() {
            return;
        }
        let Some(tool) = self.agent.get_tools().into_iter().find(|t| t.name() == call.name) else {
            return;
        };
        let summary = std::panic::catch_unwind(AssertUnwindSafe(|| tool.summary(&call.input)))
            .ok()
            .flatten();
        if let Some(s) = summary {
            lock(view).summary = s;
        }
    }

    // ── dialogs ─────────────────────────────────────────────────────

    /// A dialog joins the queue (shown at once when nothing else is); its id.
    fn enqueue(&mut self, make: Make) -> u64 {
        let id = self.id();
        self.dialogs.push_back((id, make));
        self.show_next_dialog();
        id
    }

    fn show_next_dialog(&mut self) {
        while matches!(self.input.child.current, Current::Editor) {
            let Some((id, make)) = self.dialogs.pop_front() else {
                return;
            };
            match make(self) {
                Some(Shown::Dialog(mut d)) => {
                    d.set_focused(true);
                    self.input.child.editor.focused = false;
                    self.input.child.current = Current::Dialog(id, d);
                }
                Some(Shown::Viewer(v, tx)) => {
                    self.input.child.editor.focused = false;
                    self.input.child.current = Current::Viewer(id, v, Some(tx));
                }
                None => {}
            }
            self.dirty = true;
        }
    }

    /// The dialog shown answered: the editor (or the next dialog) comes back.
    fn close_dialog(&mut self) {
        let previous = std::mem::replace(&mut self.input.child.current, Current::Editor);
        if let Current::Viewer(_, _, Some(tx)) = previous {
            let _ = tx.send(());
        }
        // A dialog dropped without an answer answers None (deny, for an approval).
        self.input.child.editor.focused = true;
        self.dirty = true;
        self.show_next_dialog();
    }

    /// An approval whose run was interrupted: gone, shown or still waiting.
    fn cancel_dialog(&mut self, id: u64) {
        self.dialogs.retain(|(i, _)| *i != id);
        let shown = match &self.input.child.current {
            Current::Dialog(i, _) | Current::Viewer(i, _, _) => *i == id,
            Current::Editor => false,
        };
        if shown {
            self.close_dialog();
        }
    }

    fn make_approval(&mut self, req: ApprovalRequest, tx: oneshot::Sender<ApprovalAnswer>) -> Option<Shown> {
        if req.signal.is_cancelled() {
            let _ = tx.send(ApprovalAnswer::new(ApprovalDecision::Deny));
            return None;
        }
        let summary = std::panic::catch_unwind(AssertUnwindSafe(|| req.tool.summary(&req.args)))
            .ok()
            .flatten()
            .unwrap_or_default();
        let done = Box::new(move |a: ApprovalAnswer| {
            let _ = tx.send(a);
        });
        // Esc stops the run, as anywhere else while the agent works; the call itself is refused
        // (the dialog closes without an answer, which is a deny). The run is stopped here, before
        // the dialog closes: had the deny reached the agent first, it could send the next request
        // before `interrupt` (which runs later, on the app) stopped it.
        let h = self.handle.clone();
        let agent = self.agent.clone();
        let interrupt = Box::new(move || {
            agent.abort();
            h.send(|app| app.interrupt());
        });
        Some(Shown::Dialog(Box::new(ApprovalDialog::new(
            &req,
            &summary,
            done,
            interrupt,
            Some(self.input_rows_fn()),
        ))))
    }

    // ── view helpers ────────────────────────────────────────────────

    fn notify(&mut self, text: &str, color: fn(&str) -> String) {
        self.add_chat(Box::new(Spacer(1)));
        self.add_chat(Box::new(Notice::colored(text, color)));
    }

    fn render_status(&mut self) {
        self.status.child.queued = self
            .queued
            .iter()
            .map(|q| {
                Notice::new(&t(
                    "↳ 次のターンで送信: {text}",
                    &[("text", &q.text.split('\n').next().unwrap_or(""))],
                ))
            })
            .collect();
        self.dirty = true;
    }

    /// Esc / ctrl+c while running: stop, and give back what was queued for the next turn.
    fn interrupt(&mut self) {
        // What the plugins made of it, images included; text alone if they are still at it.
        let queued = std::mem::take(&mut self.queued);
        let back: Vec<String> = queued
            .into_iter()
            .map(|q| match q.content {
                Some(c) => self.draft_of(&c),
                None => q.text,
            })
            .collect();
        let agent = self.agent.clone();
        tokio::spawn(async move { agent.halt().await });
        // After the draft, not inside it at the cursor; set_text would forget the editor's folded pastes.
        if !back.is_empty() && !self.input.child.editor.get_text().is_empty() {
            self.editor().move_to_end();
            self.editor().insert_text_at_cursor(&format!("\n{}", back.join("\n")));
        } else if !back.is_empty() {
            self.editor().set_text(&back.join("\n"));
        }
        self.render_status();
    }

    /// A user message back as a draft: its text, with each image held behind an [Image #n] marker
    /// (as a pasted one is) and sent again with the next message while the marker is kept.
    fn draft_of(&mut self, content: &[UserContent]) -> String {
        let images: Vec<&UserContent> = content.iter().filter(|b| matches!(b, UserContent::Image(_))).collect();
        let mut taken: HashSet<u32> = self.held.keys().copied().collect();
        let draft = self.input.child.editor.get_text();
        taken.extend(
            IMAGE_MARKER
                .captures_iter(&draft)
                .filter_map(|c| c[1].parse::<u32>().ok()),
        );
        let mut used: HashSet<usize> = HashSet::new();
        let held = &mut self.held;
        let mut hold = |name: &str, image: &UserContent, taken: &mut HashSet<u32>| -> String {
            let mut n = 1;
            while taken.contains(&n) {
                n += 1;
            }
            taken.insert(n);
            held.insert(n, (name.to_string(), image.clone()));
            format!("[Image #{n}]")
        };
        let text = content
            .iter()
            .filter_map(|b| b.as_text())
            .filter(|t| !t.starts_with(INTERRUPTED))
            .collect::<Vec<_>>()
            .join("\n");
        let text = IMAGE_LABEL
            .replace_all(&text, |c: &regex::Captures| {
                let i: usize = c[1].parse().unwrap_or(0);
                match images.get(i.wrapping_sub(1)) {
                    Some(image) if !used.contains(&i) => {
                        used.insert(i);
                        hold(&c[2], image, &mut taken)
                    }
                    _ => c[0].to_string(),
                }
            })
            .into_owned();
        let rest: Vec<String> = images
            .iter()
            .enumerate()
            .filter(|(i, _)| !used.contains(&(i + 1)))
            .map(|(_, image)| hold("image", image, &mut taken))
            .collect();
        let mut parts = vec![text];
        parts.extend(rest);
        parts.retain(|p| !p.is_empty());
        parts.join(" ")
    }

    /// The draft's held images, labeled as the image plugin labels its own, go with it; the rest are dropped.
    fn with_held(&mut self, text: &str) -> Vec<UserContent> {
        if self.held.is_empty() {
            return vec![UserContent::text(text)];
        }
        let mut images: Vec<UserContent> = Vec::new();
        let held = std::mem::take(&mut self.held);
        let out = IMAGE_MARKER
            .replace_all(text, |c: &regex::Captures| {
                let n: u32 = c[1].parse().unwrap_or(0);
                match held.get(&n) {
                    Some((name, image)) => {
                        images.push(image.clone());
                        format!("[Image {}: {name}]", images.len())
                    }
                    None => c[0].to_string(),
                }
            })
            .into_owned();
        if images.is_empty() {
            return vec![UserContent::text(text)];
        }
        let mut content = vec![UserContent::text(out)];
        content.extend(images);
        content
    }

    /// `loaded`: a stored session was just opened (see render_history).
    fn reset_view(&mut self, loaded: bool) {
        self.queued.clear();
        self.held.clear();
        self.render_status();
        self.transcript.child.chat.clear();
        self.tools.clear();
        self.current = None;
        self.context_tokens = 0;
        self.total_cost = 0.0;
        self.render_history(loaded);
        self.update_footer();
        self.full_redraw();
    }

    fn add_user(&mut self, content: &[UserContent]) {
        if let Some(note) = note_text(content) {
            self.add_chat(Box::new(NoteView::new(note)));
            return;
        }
        self.add_chat(Box::new(Spacer(1)));
        self.add_chat(Box::new(UserView::new(content)));
    }

    /// Rebuild the transcript from the agent's messages; `loaded` (on startup with -c, after
    /// /resume): from everything said in the session, also what a compaction summarized.
    fn render_history(&mut self, loaded: bool) {
        let messages = loaded
            .then(|| self.host.transcript())
            .flatten()
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| self.agent.messages());
        for m in messages {
            match m {
                Message::User(u) => self.add_user(&u.content),
                Message::Assistant(a) => {
                    let view = Arc::new(Mutex::new(AssistantView::new(Some(self.agent.cwd()))));
                    self.add_chat(Box::new(Spacer(1)));
                    self.add_chat(Box::new(SharedView(view.clone())));
                    {
                        let tools = &mut self.tools;
                        let plugins = &self.plugins;
                        lock(&view).update(&a, &mut |call, v| {
                            lock(v).renderer = plugins.renderer(&call.name);
                            tools.insert(call.id.clone(), v.clone());
                        });
                    }
                    for b in &a.content {
                        if let sasacode_ai::AssistantContent::ToolCall(c) = b {
                            self.describe_tool(c);
                        }
                    }
                    let ctx = a.usage.input + a.usage.cache_read + a.usage.cache_write + a.usage.output;
                    if ctx > 0 {
                        self.context_tokens = ctx;
                    }
                    self.total_cost += a.usage.cost;
                }
                Message::Tool(r) => {
                    if let Some(v) = self.tools.get(&r.tool_call_id) {
                        let mut v = lock(v);
                        v.output = text_of_user(&r.content);
                        v.status = if r.is_error {
                            ToolStatus::Error
                        } else {
                            ToolStatus::Done
                        };
                    }
                }
            }
        }
    }

    fn header_text(&self) -> String {
        format!(
            "{} {}\n{}",
            theme::bold("sasacode"),
            theme::gray(&self.agent.cwd()),
            theme::gray(&t(
                "/help でコマンド一覧 · esc で中断 · ctrl+o で詳細表示 · shift+tab で権限モード切替",
                &[]
            ))
        )
    }

    fn update_footer(&mut self) {
        let model = self.agent.model();
        let mode = self.agent.permissions().mode();
        let pct = if model.context_window > 0 {
            ((self.context_tokens as f64 / model.context_window as f64) * 100.0 + 0.5).floor() as u64
        } else {
            0
        };
        // auto runs everything without asking: it stands out in red, so it is never on unnoticed.
        let mode_text = t("権限: {mode}", &[("mode", &t(permission_mode_label(mode), &[]))]);
        let level = if model.reasoning == Some(true) {
            self.agent.thinking().as_str().to_string()
        } else {
            t("なし", &[])
        };
        let mut parts = vec![
            theme::gray(&plain(&format!("{}/{}", model.provider, model.id))),
            theme::gray(&t("推論: {level}", &[("level", &level)])),
            if mode == PermissionMode::Auto {
                theme::red(&theme::bold(&mode_text))
            } else {
                theme::gray(&mode_text)
            },
            theme::gray(&format!(
                "ctx {}/{} ({pct}%)",
                fmt_tokens(self.context_tokens),
                fmt_tokens(model.context_window)
            )),
        ];
        if let Some(s) = self.agent.session() {
            parts.push(theme::gray(&format!("session {}", s.id())));
        }
        if self.total_cost > 0.0 {
            parts.push(theme::gray(&format!("${}", to_fixed(self.total_cost, 3))));
        }
        let plugins: Vec<String> = self.plugins.status().into_values().collect();
        let mut text = parts.join(&theme::gray(" · "));
        if !plugins.is_empty() {
            text.push('\n');
            text.push_str(&theme::cyan(&plugins.join(" · ")));
        }
        self.footer.child.text = text;
        self.dirty = true;
    }

    fn refresh_commands(&mut self) {
        let entries = self
            .plugins
            .commands()
            .into_iter()
            .map(|c| CommandEntry {
                name: c.command.name,
                description: c.command.description,
                argument_hint: c.command.argument_hint,
                completes: c.command.complete.is_some(),
            })
            .collect();
        let cwd = self.agent.cwd();
        self.editor()
            .set_autocomplete_provider(AutocompleteProvider::new(entries, cwd));
    }

    fn set_mode(&mut self, mode: PermissionMode) {
        self.agent.permissions().set_mode(mode);
        if let Some(s) = self.agent.session() {
            let _ = s.append(&SessionEntry::PermissionMode {
                mode: mode.as_str().to_string(),
            });
        }
        if mode == PermissionMode::Auto {
            self.notify(
                &t(
                    "権限モード auto: すべてのツールを確認なしで実行します（deny・ask のルールは効きます）",
                    &[],
                ),
                theme::red,
            );
        }
        self.update_footer();
    }

    fn load_history(&self) -> Vec<String> {
        let Some(path) = self.host.history_path() else {
            return Vec::new();
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        let lines: Vec<&str> = text.split('\n').filter(|l| !l.is_empty()).collect();
        let from = lines.len().saturating_sub(200);
        lines[from..]
            .iter()
            .map(|l| serde_json::from_str::<String>(l))
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_default()
    }

    fn save_history(&self, text: &str) {
        let Some(path) = self.host.history_path() else { return };
        let Ok(line) = serde_json::to_string(text) else { return };
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        if let Ok(mut f) = opts.open(path) {
            use std::io::Write;
            let _ = writeln!(f, "{line}");
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// JS `s.slice(-n)` on UTF-16 units (a char is never split).
fn keep_last_utf16(s: &str, n: usize) -> String {
    let total = utf16_len(s);
    if total <= n {
        return s.to_string();
    }
    let mut skip = total - n;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if skip == 0 {
            start = i;
            break;
        }
        skip = skip.saturating_sub(c.len_utf16());
        start = i + c.len_utf8();
    }
    s[start..].to_string()
}

fn key_of(id: &str) -> KeyEvent {
    use crossterm::event::KeyCode;
    let code = match id {
        "right" => KeyCode::Right,
        "left" => KeyCode::Left,
        "backspace" => KeyCode::Backspace,
        _ => KeyCode::Delete,
    };
    KeyEvent::new(code, KeyModifiers::NONE)
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

// ── commands ────────────────────────────────────────────────────────

fn command<F, Fut>(h: &Handle, name: &str, description: &str, run: F) -> Command
where
    F: Fn(Handle, String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let h = h.clone();
    let run = Arc::new(run);
    Command {
        name: name.to_string(),
        description: description.to_string(),
        argument_hint: None,
        run: Arc::new(move |ctx: CommandContext| {
            let h = h.clone();
            let run = run.clone();
            Box::pin(async move { run(h, ctx.args).await })
        }),
        complete: None,
    }
}

fn core_commands(h: &Handle) -> Vec<Command> {
    let mut model = command(
        h,
        "model",
        &t("モデルを切り替える（プロバイダーから取得した一覧）", &[]),
        |h, args| async move { model_command(&h, &args).await },
    );
    model.argument_hint = Some("<provider/model> | --refresh".into());
    let hm = h.clone();
    model.complete = Some(Arc::new(move |prefix: String| {
        let h = hm.clone();
        Box::pin(async move {
            // An exact match is left out, as /language does: enter then runs the command instead of completing it.
            let p = prefix.to_lowercase();
            model_choices(&h, false)
                .await
                .into_iter()
                .filter(|o| o.value.to_lowercase().contains(&p) && o.value.to_lowercase() != p)
                .take(50)
                .collect()
        })
    }));
    let mut effort = command(
        h,
        "effort",
        &t("推論の深さ（thinking）を切り替える", &[]),
        |h, args| async move { effort_command(&h, &args).await },
    );
    effort.argument_hint = Some(
        ThinkingLevel::ALL
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join("|"),
    );
    effort.complete = Some(Arc::new(|prefix: String| {
        Box::pin(async move {
            ThinkingLevel::ALL
                .iter()
                .filter(|l| l.as_str().starts_with(&prefix) && l.as_str() != prefix)
                .map(|l| SelectOption::new(l.as_str(), l.as_str()).describe(effort_label(*l)))
                .collect()
        })
    }));
    let mut permission = command(
        h,
        "permission",
        &t("権限モードを切り替える", &[]),
        |h, args| async move { permission_command(&h, &args).await },
    );
    permission.argument_hint = Some(
        PERMISSION_MODES
            .iter()
            .map(|m| m.as_str())
            .collect::<Vec<_>>()
            .join("|"),
    );
    let mut language = command(
        h,
        "language",
        &t("表示言語を切り替える（日本語 / English）", &[]),
        |h, args| async move { language_command(&h, &args).await },
    );
    language.argument_hint = Some("ja|en".into());
    // Nothing left to complete once it is typed out: enter then runs the command, not the completion.
    language.complete = Some(Arc::new(|prefix: String| {
        Box::pin(async move {
            languages()
                .into_iter()
                .filter(|o| o.value.starts_with(&prefix) && o.value != prefix)
                .collect()
        })
    }));
    vec![
        command(h, "help", &t("コマンドとキー操作を表示", &[]), |h, _| async move {
            h.send(|app| app.help());
            Ok(())
        }),
        model,
        effort,
        command(
            h,
            "copy",
            &t("直前の応答をクリップボードにコピー", &[]),
            |h, _| async move { copy_command(&h).await },
        ),
        command(h, "resume", &t("過去のセッションを再開", &[]), |h, _| async move {
            resume_command(&h).await
        }),
        command(h, "clear", &t("新しいセッションを始める", &[]), |h, _| async move {
            clear_command(&h).await
        }),
        permission,
        command(
            h,
            "fork",
            &t("過去のメッセージから会話を分岐する", &[]),
            |h, _| async move { fork_command(&h).await },
        ),
        command(
            h,
            "session",
            &t("このセッションの ID と保存先", &[]),
            |h, _| async move {
                h.send(|app| app.session_command());
                Ok(())
            },
        ),
        language,
        command(h, "exit", &t("終了する", &[]), |h, _| async move {
            h.send(|app| app.exit(0));
            Ok(())
        }),
        command(h, "quit", &t("終了する（/exit と同じ）", &[]), |h, _| async move {
            h.send(|app| app.exit(0));
            Ok(())
        }),
    ]
}

async fn run_command(h: &Handle, text: &str) {
    // The arguments as written, line breaks and indentation kept (a pasted traceback, code): only
    // their ends are trimmed (#112).
    let rest = &text[1..];
    let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = &rest[..name_end];
    let args = rest[name_end..].trim();
    let Some(cmd) = h.plugins.commands().into_iter().find(|c| c.command.name == name) else {
        h.notify(
            t("不明なコマンド: /{name}（/help で一覧）", &[("name", &name)]),
            theme::yellow,
        );
        return;
    };
    let run = cmd.command.run.clone();
    let outcome = AssertUnwindSafe(run(CommandContext { args: args.to_string() }))
        .catch_unwind()
        .await;
    let error = match outcome {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e.to_string()),
        Err(_) => Some("panic".to_string()),
    };
    if let Some(error) = error {
        h.notify(
            t("/{name} が失敗しました: {error}", &[("name", &name), ("error", &error)]),
            theme::red,
        );
    }
}

impl App {
    fn help(&mut self) {
        let mut lines = vec![theme::bold(&t("コマンド", &[]))];
        for c in self.plugins.commands() {
            let c = c.command;
            let hint = c
                .argument_hint
                .as_deref()
                .filter(|h| !h.is_empty())
                .map(|h| format!(" {}", theme::gray(h)))
                .unwrap_or_default();
            lines.push(format!("  /{}{hint}  {}", c.name, theme::gray(&c.description)));
        }
        lines.push(theme::bold(&t("キー", &[])));
        lines.push(format!(
            "  {}",
            t("enter 送信 · shift/alt+enter 改行 · ↑↓ 履歴 · tab 補完", &[])
        ));
        lines.push(format!(
            "  {}",
            t(
                "esc 中断 · ctrl+o 詳細表示 · shift+tab 権限モード · /exit・ctrl+c×2・ctrl+d 終了",
                &[]
            )
        ));
        if self.handle.full_screen {
            lines.push(format!(
                "  {}",
                t(
                    "pageup/pagedown・ホイール スクロール · ctrl+↑↓ 前後の入力へ · ctrl+home/end 先頭・最新へ",
                    &[]
                )
            ));
            lines.push(format!(
                "  {}",
                t(
                    "ctrl+shift+f 会話内を検索 · ドラッグで選択してコピー（端末の選択は option/alt を押しながら）",
                    &[]
                )
            ));
        }
        lines.push(theme::gray(&format!(
            "  {}",
            t("実行中に送ったメッセージは次のターンでモデルに届きます", &[])
        )));
        self.notify(&lines.join("\n"), theme::identity);
    }

    fn session_command(&mut self) {
        let Some(s) = self.agent.session() else {
            self.notify(&t("このセッションは保存していません（--no-session）", &[]), theme::gray);
            return;
        };
        let saved = s.path().exists();
        let text = [
            format!("session {}", s.id()),
            if saved {
                s.path().display().to_string()
            } else {
                t("（最初のメッセージを送ると保存されます）", &[])
            },
            t("再開: sasacode -r {id}", &[("id", &s.id())]),
        ]
        .join("\n");
        self.notify(&text, theme::identity);
    }
}

/// At the turn limit, the user decides whether the run goes on for another round of turns.
async fn offer_to_continue(h: &Handle) {
    h.agent.wait_for_idle().await;
    let n = h.agent.max_turns().unwrap_or(0);
    let title = t("最大ターン数（{n}）に達しました。続けますか？", &[("n", &n)]);
    let options = vec![
        SelectOption::new("yes", t("続ける（さらに {n} ターンまで）", &[("n", &n)])),
        SelectOption::new("no", t("ここで止める", &[])),
    ];
    let more = h.pick(title, options, 10).await;
    if more.as_deref() == Some("yes") {
        let agent = h.agent.clone();
        tokio::spawn(async move {
            let _ = agent.continue_run().await;
        });
    }
}

/// Switches everything that reads the language now; text already in the transcript stays as it was.
async fn language_command(h: &Handle, args: &str) -> anyhow::Result<()> {
    let named = args.trim().to_lowercase();
    let lang = if named.is_empty() {
        h.pick(t("表示言語", &[]), languages(), 10)
            .await
            .and_then(|v| language_named(&v))
    } else {
        language_named(&named)
    };
    let Some(lang) = lang else {
        if !named.is_empty() {
            h.notify(t("使い方: /language ja|en", &[]), theme::yellow);
        }
        return Ok(());
    };
    set_lang(lang);
    h.plugins.set_lang(lang);
    // Every command's description, in the new language: registered again under the same owner, in
    // the same order (a registration replaces the one of that name).
    for owned in h.plugins.commands() {
        let mut c = owned.command;
        c.description = retranslate(&c.description);
        h.plugins.api(&owned.owner).register_command(c);
    }
    let host = h.host.clone();
    h.with(move |app| {
        app.apply_search_text();
        app.refresh_commands();
        app.transcript.child.header = Notice::new(&app.header_text());
        app.update_footer();
        let saved = match host.save_lang(lang) {
            Ok(()) => true,
            Err(e) => {
                app.notify(
                    &t(
                        "設定に保存できませんでした: {error}",
                        &[("error", &plain(&e.to_string()))],
                    ),
                    theme::yellow,
                );
                false
            }
        };
        let name = LANGUAGES
            .iter()
            .find(|(v, _)| language_named(v) == Some(lang))
            .map(|(_, l)| *l)
            .unwrap_or("");
        app.notify(&t("表示言語: {name}", &[("name", &name)]), theme::gray);
        // SASACODE_LANG wins over the setting at the next start.
        let env = std::env::var("SASACODE_LANG").unwrap_or_default().to_lowercase();
        if saved && (env == "ja" || env == "en") && language_named(&env) != Some(lang) {
            app.notify(
                &t(
                    "次回の起動では環境変数 SASACODE_LANG={env} が優先されます",
                    &[("env", &env)],
                ),
                theme::yellow,
            );
        }
    })
    .await;
    Ok(())
}

/// Current model, the ones in config, then everything the providers list.
async fn model_choices(h: &Handle, refresh: bool) -> Vec<SelectOption> {
    let model = h.agent.model();
    let current = format!("{}/{}", model.provider, model.id);
    let host = h.host.clone();
    let mut pending = Box::pin(async move { host.list_models(refresh).await });
    let quick = tokio::select! {
        r = &mut pending => Some(r),
        _ = tokio::time::sleep(Duration::from_millis(300)) => None,
    };
    let result = match quick {
        Some(r) => r,
        None => {
            h.notify(t("プロバイダーからモデル一覧を取得しています…", &[]), theme::gray);
            pending.await
        }
    };
    let listed = match result {
        Ok(l) => l,
        Err(e) => {
            h.notify(
                t("モデル一覧を取得できませんでした: {error}", &[("error", &e)]),
                theme::yellow,
            );
            Vec::new()
        }
    };
    let size: HashMap<String, String> = listed
        .iter()
        .map(|m| {
            let s = match (m.context_window, m.max_context) {
                (Some(cw), _) if cw > 0 => format!("{} ctx", fmt_tokens(cw)),
                (_, Some(max)) if max > 0 => max_label(m.server.as_deref(), &fmt_tokens(max)),
                _ => String::new(),
            };
            (m.spec.clone(), s)
        })
        .collect();
    let mut specs: Vec<String> = vec![current.clone()];
    for s in h
        .host
        .config()
        .models
        .into_iter()
        .chain(listed.into_iter().map(|m| m.spec))
    {
        if !specs.contains(&s) {
            specs.push(s);
        }
    }
    specs
        .into_iter()
        .map(|s| {
            let parts: Vec<String> = [
                if s == current { t("現在", &[]) } else { String::new() },
                size.get(&s).cloned().unwrap_or_default(),
            ]
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect();
            let mut o = SelectOption::new(s.clone(), s);
            if !parts.is_empty() {
                o = o.describe(parts.join(" · "));
            }
            o
        })
        .collect()
}

async fn model_command(h: &Handle, args: &str) -> anyhow::Result<()> {
    let mut spec = if args == "--refresh" {
        String::new()
    } else {
        args.to_string()
    };
    if spec.is_empty() {
        let choices = model_choices(h, args == "--refresh").await;
        let Some(picked) = h
            .pick(
                t("モデルを選択（一覧にないモデルは /model <provider/model>）", &[]),
                choices,
                12,
            )
            .await
        else {
            return Ok(());
        };
        spec = picked;
    }
    let model = h.host.resolve(&spec)?;
    h.agent.set_model(model)?;
    h.notify(t("モデル: {spec}", &[("spec", &spec)]), theme::gray);
    h.send(|app| app.update_footer());
    warn_context(h).await;
    Ok(())
}

/// Ollama cuts an input longer than its own default short without a word when the model has no
/// num_ctx, while sasacode plans for the model's full size: say so, once for each model.
async fn warn_context(h: &Handle) {
    let model = h.agent.model();
    let spec = format!("{}/{}", model.provider, model.id);
    let s = spec.clone();
    if h.with(move |app| app.warned_context.contains(&s)).await != Some(false) {
        return;
    }
    let listed = h.host.list_models(false).await.unwrap_or_default();
    let Some(m) = listed.into_iter().find(|m| m.spec == spec) else {
        return;
    };
    let Some(max) = m.max_context.filter(|_| m.context_window.is_none_or(|c| c == 0)) else {
        return;
    };
    if m.server.as_deref() != Some("ollama") || max == 0 {
        return;
    }
    let text = t(
        "{spec} には num_ctx が設定されていません。Ollama を OLLAMA_CONTEXT_LENGTH で起動していなければ、Ollama の既定の長さ（数千トークン）を超えた入力は黙って切り詰められます。Modelfile に PARAMETER num_ctx 32768 などを書くか、OLLAMA_CONTEXT_LENGTH を設定してください（このモデルは最大 {max}）。",
        &[("spec", &plain(&spec)), ("max", &fmt_tokens(max))],
    );
    h.send(move |app| {
        if app.warned_context.insert(spec) {
            app.notify(&text, theme::yellow);
        }
    });
}

async fn resume_command(h: &Handle) -> anyhow::Result<()> {
    let current = h.agent.session().map(|s| s.path().to_path_buf());
    let sessions: Vec<_> = list_sessions(&h.host.sessions_dir(), Some(&h.agent.cwd()))
        .into_iter()
        .filter(|s| Some(&s.path) != current.as_ref())
        .collect();
    if sessions.is_empty() {
        h.notify(t("このディレクトリの過去セッションはありません", &[]), theme::gray);
        return Ok(());
    }
    // All of them: the filter searches the whole list, and an old session must stay reachable.
    let options = sessions
        .iter()
        .map(|s| {
            let first = if s.first_prompt.is_empty() {
                t("(空)", &[])
            } else {
                s.first_prompt.clone()
            };
            SelectOption::new(s.path.display().to_string(), clip(&one_line(&first), 60)).describe(format!(
                "{} · {} msgs",
                local_time(s.modified_at),
                s.message_count
            ))
        })
        .collect();
    let Some(picked) = h.pick(t("再開するセッション", &[]), options, 10).await else {
        return Ok(());
    };
    h.agent.halt().await;
    h.host.load_session(Path::new(&picked)).await?;
    h.with(|app| {
        app.reset_view(true);
        app.notify(&t("セッションを再開しました", &[]), theme::gray);
    })
    .await;
    Ok(())
}

async fn clear_command(h: &Handle) -> anyhow::Result<()> {
    h.agent.halt().await;
    h.host.new_session().await?;
    h.with(|app| {
        app.reset_view(false);
        app.context_tokens = 0;
        app.total_cost = 0.0;
        app.update_footer();
        app.notify(&t("新しいセッションを開始しました", &[]), theme::gray);
    })
    .await;
    Ok(())
}

async fn fork_command(h: &Handle) -> anyhow::Result<()> {
    let messages = h.agent.messages();
    let users: Vec<(usize, String)> = messages
        .iter()
        .enumerate()
        .filter_map(|(i, m)| match m {
            Message::User(u) => {
                let text = text_of_user(&u.content);
                (!text.starts_with("[Summary of")).then_some((i, text))
            }
            _ => None,
        })
        .collect();
    if users.is_empty() {
        h.notify(t("分岐できるメッセージがありません", &[]), theme::gray);
        return Ok(());
    }
    let options = users
        .iter()
        .rev()
        .map(|(i, text)| SelectOption::new(i.to_string(), clip(&one_line(text), 70)))
        .collect();
    let Some(picked) = h
        .pick(
            t(
                "どのメッセージから分岐しますか（その直前までの会話で新しいセッションを作り、入力欄に戻します）",
                &[],
            ),
            options,
            10,
        )
        .await
    else {
        return Ok(());
    };
    h.agent.halt().await;
    let index: usize = picked.parse()?;
    let content = match h.agent.messages().get(index) {
        Some(Message::User(u)) => u.content.clone(),
        _ => Vec::new(),
    };
    h.host.fork(index).await?;
    h.with(move |app| {
        app.reset_view(false);
        let draft = app.draft_of(&content);
        app.editor().set_text(&draft);
        app.notify(&t("分岐しました。編集して送信してください", &[]), theme::gray);
    })
    .await;
    Ok(())
}

async fn permission_command(h: &Handle, args: &str) -> anyhow::Result<()> {
    let mut mode = args.to_string();
    if mode.is_empty() {
        let current = h.agent.permissions().mode();
        let options = PERMISSION_MODES
            .iter()
            .map(|m| {
                let o = SelectOption::new(
                    m.as_str(),
                    format!("{}  {}", m.as_str(), t(permission_mode_label(*m), &[])),
                );
                if *m == current { o.describe(t("現在", &[])) } else { o }
            })
            .collect();
        let Some(picked) = h.pick(t("権限モード", &[]), options, 10).await else {
            return Ok(());
        };
        mode = picked;
    }
    let Some(mode) = parse_mode(&mode) else {
        let modes = PERMISSION_MODES
            .iter()
            .map(|m| m.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(t("モードは {modes} のいずれかです", &[("modes", &modes)]));
    };
    h.send(move |app| app.set_mode(mode));
    Ok(())
}

/// The last response's text, to the system clipboard (pbcopy / wl-copy / xclip, else OSC 52).
async fn copy_command(h: &Handle) -> anyhow::Result<()> {
    let last = h
        .agent
        .messages()
        .into_iter()
        .rev()
        .find(|m| matches!(m, Message::Assistant(_)) && !m.text().trim().is_empty());
    let text = last.map(|m| m.text().trim().to_string()).unwrap_or_default();
    if text.is_empty() {
        h.notify(t("コピーする応答がありません", &[]), theme::yellow);
        return Ok(());
    }
    let h2 = h.clone();
    let how = copy_to_clipboard(&text, move |seq| h2.send(move |app| app.screen.write(&seq))).await;
    let n = utf16_len(&text);
    h.notify(
        if how == Copied::Terminal {
            t("直前の応答をコピーしました（端末経由・{n} 文字）", &[("n", &n)])
        } else {
            t("直前の応答をコピーしました（{n} 文字）", &[("n", &n)])
        },
        theme::gray,
    );
    Ok(())
}

async fn effort_command(h: &Handle, args: &str) -> anyhow::Result<()> {
    let mut level = args.trim().to_string();
    if level.is_empty() {
        let model = h.agent.model();
        let current = h.agent.thinking();
        let title = if model.reasoning == Some(true) {
            t("推論の深さ", &[])
        } else {
            t(
                "推論の深さ（{model} は推論に対応していないため、変えても効果はありません）",
                &[("model", &model.id)],
            )
        };
        let options = ThinkingLevel::ALL
            .iter()
            .map(|l| {
                let o = SelectOption::new(l.as_str(), format!("{}  {}", l.as_str(), effort_label(*l)));
                if *l == current { o.describe(t("現在", &[])) } else { o }
            })
            .collect();
        let Some(picked) = h.pick(title, options, 10).await else {
            return Ok(());
        };
        level = picked;
    }
    let Some(level) = ThinkingLevel::parse(&level) else {
        let levels = ThinkingLevel::ALL
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(t("推論の深さは {levels} のいずれかです", &[("levels", &levels)]));
    };
    h.agent.set_thinking(level);
    h.send(|app| app.update_footer());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_cuts_between_whole_characters() {
        assert_eq!(clip("abc", 3), "abc");
        assert_eq!(clip("abcd", 3), "ab…");
        assert_eq!(clip("👨‍👩‍👧x日本", 3), "👨‍👩‍👧x…");
    }

    #[test]
    fn keep_last_counts_utf16_units() {
        assert_eq!(keep_last_utf16("abcdef", 3), "def");
        assert_eq!(keep_last_utf16("ab😀", 2), "😀");
        assert_eq!(keep_last_utf16("ab", 5), "ab");
    }

    #[test]
    fn language_names_and_whitespace() {
        assert_eq!(language_named("英語"), Some(Lang::En));
        assert_eq!(language_named("jp"), Some(Lang::Ja));
        assert_eq!(language_named("fr"), None);
        assert_eq!(one_line("a \n\t b"), "a b");
    }
}
