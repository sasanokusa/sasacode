//! packages/tui/test/app.test.ts: the TUI driven through a stand-in terminal, keys in and rendered
//! text out. No model: a replay provider.
//!
//! Every test holds one lock: they share the UI language, the expanded-view toggle and the other
//! process-wide settings, which the TS ran one test at a time.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use tokio::sync::MutexGuard;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::permission::PermissionPolicy;
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_agent::session::{SessionEntry, SessionFile};
use sasacode_agent::tool_exec::{ApprovalDecision, ApprovalRequest};
use sasacode_ai::{
    AssistantContent, AssistantMessage, Message, ModelInfo, Provider, Request, StopReason, StreamEvent,
    ThinkingContent, ToolCall, Usage, UserContent, UserMessage, register_api, text_of_user,
};
use sasacode_host::{current_lang, set_lang};
use sasacode_plugin_api::{
    Command, CommandContext, Lang, PermissionMode, PermissionRules, PluginApi, SelectOption, ShowTextOptions,
    SubagentOptions, TextFormat, UiBridge, hook,
};
use sasacode_testing::{ReplayItem, ReplayOptions, ReplayProvider};
use sasacode_tui::activity::PHRASES_JA;
use sasacode_tui::summary::{UsageTally, usage_lines};
use sasacode_tui::testing::FakeTerminal;
use sasacode_tui::{ListedModel, TuiConfig, TuiHost, run_tui_with};

// ── set-up ──────────────────────────────────────────────────────────

static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// One test at a time, each starting from the same settings (ctrl+o and /language change them).
async fn pin() -> MutexGuard<'static, ()> {
    let g = LOCK.lock().await;
    // These assertions quote the Japanese strings.
    set_lang(Lang::Ja);
    sasacode_tui::views::set_expanded(false);
    sasacode_tui::ui::markdown::set_hyperlinks_override(Some(false));
    sasacode_tui::theme::set_no_color(false);
    sasacode_tui::text::set_ambiguous_width(1);
    g
}

/// A replay api name of this test's own: the provider registry is global.
fn api_name() -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    format!("replay-tui-{}", N.fetch_add(1, Ordering::Relaxed))
}

fn model(api: &str, context_window: u64) -> ModelInfo {
    serde_json::from_value(json!({
        "id": "m", "provider": "t", "api": api, "contextWindow": context_window, "maxOutput": 1000,
    }))
    .unwrap_or_else(|e| panic!("model: {e}"))
}

fn say(text: &str) -> AssistantContent {
    AssistantContent::text(text)
}

fn call(id: &str, name: &str, input: Value) -> AssistantContent {
    AssistantContent::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        input: input.as_object().cloned().unwrap_or_default(),
        raw_input: None,
    })
}

fn bash(id: &str, command: &str) -> AssistantContent {
    call(id, "bash", json!({ "command": command }))
}

fn reply(content: Vec<AssistantContent>) -> AssistantMessage {
    let tool = content.iter().any(|c| matches!(c, AssistantContent::ToolCall(_)));
    AssistantMessage {
        content,
        api: "replay".into(),
        provider: "t".into(),
        model: "m".into(),
        usage: Usage::default(),
        stop_reason: if tool { StopReason::ToolUse } else { StopReason::Stop },
        timestamp: 0,
        replay_scope: None,
        error_message: None,
    }
}

fn with_usage(mut m: AssistantMessage, usage: Usage) -> AssistantMessage {
    m.usage = usage;
    m
}

fn replay(api: &str, script: Vec<AssistantMessage>, chunk_size: Option<usize>) -> Arc<ReplayProvider> {
    let provider = Arc::new(ReplayProvider::new(
        script.into_iter().map(ReplayItem::from),
        ReplayOptions {
            api: Some(api.to_string()),
            chunk_size,
        },
    ));
    register_api(api, provider.clone());
    provider
}

/// `JSON.stringify(v)`.
fn json_of<T: serde::Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| panic!("json: {e}"))
}

fn last_user(agent: &Agent) -> Vec<UserContent> {
    agent
        .messages()
        .into_iter()
        .rev()
        .find_map(|m| match m {
            Message::User(u) => Some(u.content),
            _ => None,
        })
        .unwrap_or_default()
}

fn has_tool_result(agent: &Agent) -> bool {
    agent.messages().iter().any(|m| matches!(m, Message::Tool(_)))
}

/// The keys and input as a terminal sends them; a short pause after each lets the app react and render.
#[derive(Clone)]
struct Term(FakeTerminal);

const PAUSE: Duration = Duration::from_millis(30);

impl Term {
    async fn key(&self, id: &str) {
        self.0.key(id);
        tokio::time::sleep(PAUSE).await;
    }
    async fn keys(&self, ids: &[&str]) {
        for id in ids {
            self.key(id).await;
        }
    }
    async fn typ(&self, text: &str) {
        self.0.type_text(text);
        tokio::time::sleep(PAUSE).await;
    }
    async fn paste(&self, text: &str) {
        self.0.paste(text);
        tokio::time::sleep(PAUSE).await;
    }
    fn out(&self) -> String {
        self.0.output()
    }
    fn mark(&self) -> usize {
        self.0.mark()
    }
    /// Everything written since `from`, colors removed.
    fn text(&self, from: usize) -> String {
        self.0.text(from)
    }
    /// Wait (up to three seconds) until the output since `from` holds `needle`.
    async fn until(&self, from: usize, needle: &str) -> bool {
        until(|| self.text(from).contains(needle)).await
    }
}

async fn until(mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    f()
}

async fn sleep(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

struct TestHost {
    agent: Agent,
    host: Arc<PluginHost>,
    model: ModelInfo,
    listed: Vec<ListedModel>,
    alt: bool,
    sessions_dir: PathBuf,
    history: Option<PathBuf>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl TestHost {
    fn call(&self, what: impl Into<String>) {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).push(what.into());
    }
}

#[async_trait]
impl TuiHost for TestHost {
    fn agent(&self) -> Agent {
        self.agent.clone()
    }
    fn host(&self) -> Arc<PluginHost> {
        self.host.clone()
    }
    fn config(&self) -> TuiConfig {
        TuiConfig {
            models: Vec::new(),
            alt_screen: self.alt,
            bell: false,
        }
    }
    fn resolve(&self, _spec: &str) -> anyhow::Result<ModelInfo> {
        Ok(self.model.clone())
    }
    fn sessions_dir(&self) -> PathBuf {
        self.sessions_dir.clone()
    }
    fn history_path(&self) -> Option<PathBuf> {
        self.history.clone()
    }
    async fn list_models(&self, _refresh: bool) -> anyhow::Result<Vec<ListedModel>> {
        Ok(self.listed.clone())
    }
    async fn load_plugins(&self, ui: Arc<dyn UiBridge>) -> anyhow::Result<()> {
        self.host.set_ui(Some(ui));
        Ok(())
    }
    async fn load_session(&self, _path: &Path) -> anyhow::Result<()> {
        self.call("loadSession");
        Ok(())
    }
    async fn new_session(&self) -> anyhow::Result<()> {
        self.call("newSession");
        self.agent.reset(Vec::new());
        Ok(())
    }
    async fn fork(&self, _index: usize) -> anyhow::Result<()> {
        self.call("fork");
        Ok(())
    }
    async fn shutdown(&self) {
        self.call("shutdown");
    }
    fn save_lang(&self, lang: Lang) -> anyhow::Result<()> {
        self.call(format!("saveLang:{}", if lang == Lang::En { "en" } else { "ja" }));
        Ok(())
    }
}

struct Started {
    term: Term,
    agent: Agent,
    host: Arc<PluginHost>,
    calls: Arc<Mutex<Vec<String>>>,
    exited: JoinHandle<anyhow::Result<i32>>,
    api: String,
    _dir: tempfile::TempDir,
}

impl Started {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn cwd(&self) -> PathBuf {
        PathBuf::from(self.agent.cwd())
    }
    /// Type /exit, send it and wait for the exit code.
    async fn exit(self) -> i32 {
        self.term.typ("/exit").await;
        self.term.key("enter").await;
        self.exited().await
    }
    async fn exited(self) -> i32 {
        match tokio::time::timeout(Duration::from_secs(5), self.exited).await {
            Ok(Ok(Ok(code))) => code,
            other => panic!("the app did not exit: {other:?}"),
        }
    }
}

struct Opts {
    mode: PermissionMode,
    listed: Vec<ListedModel>,
    alt: bool,
    sessions_dir: Option<PathBuf>,
    cwd: Option<PathBuf>,
    history: Option<PathBuf>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            mode: PermissionMode::Edits,
            listed: Vec::new(),
            alt: false,
            sessions_dir: None,
            cwd: None,
            history: None,
        }
    }
}

async fn start(script: Vec<AssistantMessage>) -> Started {
    start_with(script, Opts::default()).await
}

async fn start_with(script: Vec<AssistantMessage>, o: Opts) -> Started {
    let api = api_name();
    replay(&api, script, None);
    let dir = tempfile::Builder::new()
        .prefix("sasacode-tui-")
        .tempdir()
        .unwrap_or_else(|e| panic!("tempdir: {e}"));
    let cwd = o.cwd.clone().unwrap_or_else(|| dir.path().to_path_buf());
    let cwd_str = cwd.display().to_string();
    let m = model(&api, 100_000);
    let mut opts = AgentOptions::new(m.clone(), cwd_str.clone(), "");
    opts.permissions = Some(Arc::new(
        PermissionPolicy::new(o.mode, &PermissionRules::default()).unwrap_or_else(|e| panic!("policy: {e}")),
    ));
    let agent = Agent::new(opts);
    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd: cwd_str,
        settings: None,
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: current_lang(),
    });
    assert!(
        host.load("builtin-tools", |api| Box::pin(sasacode_tools::plugin(api)))
            .await
    );
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let tui_host = Arc::new(TestHost {
        agent: agent.clone(),
        host: host.clone(),
        model: m,
        listed: o.listed,
        alt: o.alt,
        sessions_dir: o.sessions_dir.unwrap_or(cwd),
        history: o.history,
        calls: calls.clone(),
    });
    let fake = FakeTerminal::new(100, 40);
    let exited = tokio::spawn(run_tui_with(tui_host, String::new(), Box::new(fake.clone())));
    sleep(50).await;
    Started {
        term: Term(fake),
        agent,
        host,
        calls,
        exited,
        api,
        _dir: dir,
    }
}

// ── the tests ───────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_prompt_is_sent_and_the_answer_shown_clear_starts_a_new_session_exit_quits() {
    let _g = pin().await;
    let s = start(vec![reply(vec![say("hello from the model")])]).await;
    s.term.typ("hi").await;
    s.term.key("enter").await;
    assert!(s.term.until(0, "hello from the model").await, "{}", s.term.text(0));
    s.term.typ("/clear").await;
    s.term.key("enter").await;
    assert!(until(|| s.calls().contains(&"newSession".to_string())).await);
    let calls = s.calls.clone();
    assert_eq!(s.exit().await, 0);
    assert!(
        calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&"shutdown".to_string())
    );
}

// Not in the TS: the full screen (the default) end to end, which the TS tests only touched for the
// search box.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_the_full_screen_a_prompt_is_answered_and_exit_leaves_the_alternate_screen() {
    let _g = pin().await;
    let s = start_with(
        vec![reply(vec![say("hello on the full screen")])],
        Opts {
            alt: true,
            ..Opts::default()
        },
    )
    .await;
    assert!(s.term.out().starts_with("\x1b[?1049h"));
    s.term.typ("hi").await;
    s.term.key("enter").await;
    assert!(s.term.until(0, "hello on the full screen").await, "{}", s.term.text(0));
    let term = s.term.clone();
    assert_eq!(s.exit().await, 0);
    let out = term.out();
    let left = out
        .rfind("\x1b[?1049l")
        .unwrap_or_else(|| panic!("never left the alternate screen"));
    // The usage summary is written on the normal screen, after leaving it.
    let after = sasacode_tui::testing::strip_for_tests(&out[left..]);
    assert!(after.contains("を終了しました"), "{after:?}");
    assert!(after.contains("1 回"), "{after:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approval_prompt_the_tool_runs_only_after_it_is_allowed() {
    let _g = pin().await;
    let s = start(vec![
        reply(vec![bash("1", "echo approved-run")]),
        reply(vec![say("done")]),
    ])
    .await;
    s.term.typ("run it").await;
    s.term.key("enter").await;
    assert!(s.term.until(0, "bash を実行しますか？").await, "{}", s.term.text(0));
    assert!(!has_tool_result(&s.agent));
    s.term.key("enter").await; // はい
    let agent = s.agent.clone();
    assert!(
        until(|| agent
            .messages()
            .iter()
            .any(|m| matches!(m, Message::Tool(t) if json_of(&t.content).contains("approved-run"))))
        .await
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shift_tab_cycles_the_permission_mode_auto_is_shown_in_red() {
    let _g = pin().await;
    let s = start(vec![]).await;
    for _ in 0..3 {
        s.term.key("shift+tab").await;
    }
    assert_eq!(s.agent.permissions().mode(), PermissionMode::Auto);
    let red = regex::Regex::new(r"\x1b\[31m(\x1b\[1m)?権限: 自動許可").unwrap_or_else(|e| panic!("{e}"));
    assert!(red.is_match(&s.term.out()), "{:?}", s.term.out());
    assert!(s.term.text(0).contains("権限モード auto"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_context_limit_notice_appears_only_when_the_run_really_stops_there() {
    let _g = pin().await;
    let full = with_usage(
        reply(vec![say("long")]),
        Usage {
            input: 99_000,
            output: 10,
            ..Usage::default()
        },
    );
    let s = start(vec![full]).await;
    s.term.typ("go").await;
    s.term.key("enter").await;
    assert!(
        s.term.until(0, "コンテキストの上限に達したため停止しました").await,
        "{}",
        s.term.text(0)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lang_en_the_approval_prompt_the_footer_and_help_are_in_english() {
    let _g = pin().await;
    set_lang(Lang::En);
    let s = start(vec![reply(vec![bash("1", "echo hi")]), reply(vec![say("done")])]).await;
    assert!(
        s.term.until(0, "permissions: reads and edits only").await,
        "{}",
        s.term.text(0)
    );
    s.term.typ("run it").await;
    s.term.key("enter").await;
    assert!(s.term.until(0, "Run bash?").await, "{}", s.term.text(0));
    assert!(s.term.text(0).contains("Always allow (until exit)"));
    s.term.key("enter").await;
    sleep(300).await;
    s.term.typ("/help").await;
    s.term.key("enter").await;
    assert!(s.term.until(0, "show commands and keys").await, "{}", s.term.text(0));
    let hiragana = regex::Regex::new(r"[ぁ-ん]").unwrap_or_else(|e| panic!("{e}"));
    let text = s.term.text(0);
    assert!(!hiragana.is_match(&text), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_without_num_ctx_gets_one_warning_at_startup_and_not_again_on_model() {
    let _g = pin().await;
    let s = start_with(
        vec![],
        Opts {
            listed: vec![ListedModel {
                spec: "t/m".into(),
                max_context: Some(262_144),
                server: Some("ollama".into()),
                ..ListedModel::default()
            }],
            ..Opts::default()
        },
    )
    .await;
    assert!(
        s.term.until(0, "t/m には num_ctx が設定されていません").await,
        "{}",
        s.term.text(0)
    );
    let before = s.term.mark();
    s.term.typ("/model t/m").await;
    s.term.key("enter").await; // takes the completion
    s.term.key("enter").await;
    assert!(s.term.until(before, "モデル: t/m").await, "{}", s.term.text(before));
    sleep(50).await;
    assert!(!s.term.text(before).contains("num_ctx が設定されていません"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_completion_shows_a_long_model_id_whole_as_the_picker_does() {
    let _g = pin().await;
    let long = "commandcode/deepseek/deepseek-v4.1-flash-fast";
    let listed = |spec: String| ListedModel {
        spec,
        context_window: Some(1_000_000),
        ..ListedModel::default()
    };
    let s = start_with(
        vec![],
        Opts {
            listed: vec![listed(long.into()), listed(format!("{long}-2"))],
            ..Opts::default()
        },
    )
    .await;
    let from = s.term.mark();
    s.term.typ("/model Deep").await;
    assert!(s.term.until(from, &format!("{long}-2")).await, "{}", s.term.text(from));
    s.term.key("ctrl+c").await;
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_lm_studio_model_not_loaded_yet_gets_no_num_ctx_warning_and_the_picker_says_so() {
    let _g = pin().await;
    let s = start_with(
        vec![],
        Opts {
            listed: vec![ListedModel {
                spec: "t/m".into(),
                max_context: Some(32_768),
                server: Some("lmstudio".into()),
                ..ListedModel::default()
            }],
            ..Opts::default()
        },
    )
    .await;
    sleep(50).await;
    assert!(!s.term.text(0).contains("num_ctx"));
    let before = s.term.mark();
    s.term.typ("/model").await;
    s.term.key("enter").await;
    assert!(
        s.term.until(before, "最大 32k（未ロード）").await,
        "{}",
        s.term.text(before)
    );
    assert!(!s.term.text(before).contains("num_ctx"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_context_window_learned_at_a_turns_end_shows_in_the_footer_when_the_run_ends() {
    let _g = pin().await;
    let s = start(vec![reply(vec![say("ok")])]).await;
    let agent = s.agent.clone();
    let small = model(&s.api, 8192);
    s.agent.hooks().on::<hook::TurnEnd, _, _>("test", move |_e, _ctx| {
        let (agent, small) = (agent.clone(), small.clone());
        async move {
            agent.set_model(small)?;
            Ok(None)
        }
    });
    assert!(s.term.until(0, "/100k").await, "{}", s.term.text(0));
    let before = s.term.mark();
    s.term.typ("hi").await;
    s.term.key("enter").await;
    assert!(s.term.until(before, "/8k").await, "{}", s.term.text(before));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_whose_size_is_known_gets_no_warning() {
    let _g = pin().await;
    let s = start_with(
        vec![],
        Opts {
            listed: vec![ListedModel {
                spec: "t/m".into(),
                context_window: Some(32_768),
                max_context: Some(262_144),
                ..ListedModel::default()
            }],
            ..Opts::default()
        },
    )
    .await;
    sleep(80).await;
    assert!(!s.term.text(0).contains("num_ctx"));
}

fn bash_request(agent: &Agent, id: &str, command: &str, signal: CancellationToken) -> ApprovalRequest {
    let tool = agent
        .get_tools()
        .into_iter()
        .find(|t| t.name() == "bash")
        .unwrap_or_else(|| panic!("no bash tool"));
    let input = json!({ "command": command }).as_object().cloned().unwrap_or_default();
    ApprovalRequest {
        tool,
        args: input.clone(),
        cwd: agent.cwd(),
        signal,
        call: ToolCall {
            id: id.into(),
            name: "bash".into(),
            input,
            raw_input: None,
        },
        reason: "test".into(),
    }
}

fn ask(agent: &Agent, req: ApprovalRequest) -> JoinHandle<ApprovalDecision> {
    let approve = agent.approve().unwrap_or_else(|| panic!("no approve set"));
    tokio::spawn(async move { approve(req).await.map(|a| a.decision).unwrap_or(ApprovalDecision::Deny) })
}

async fn joined<T: std::fmt::Debug>(h: JoinHandle<T>) -> T {
    match tokio::time::timeout(Duration::from_secs(5), h).await {
        Ok(Ok(v)) => v,
        other => panic!("did not finish: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_text_viewer_owns_input_serializes_with_approvals_and_returns_to_the_editor() {
    let _g = pin().await;
    let s = start(vec![]).await;
    let api = s.host.api("viewer-test");
    let text: Vec<String> = (0..60).map(|i| format!("entry-{i}")).collect();
    let viewing = tokio::spawn(async move {
        api.ui()
            .show_text(ShowTextOptions {
                title: "Long preview".into(),
                text: text.join("\n"),
                format: TextFormat::Text,
            })
            .await
    });
    assert!(s.term.until(0, "Long preview").await, "{}", s.term.text(0));
    let approval = ask(
        &s.agent,
        bash_request(&s.agent, "queued", "echo queued", CancellationToken::new()),
    );
    let from = s.term.mark();
    s.term.typ("G").await;
    assert!(s.term.until(from, "entry-59").await, "{}", s.term.text(from));
    assert!(!viewing.is_finished());
    s.term.key("escape").await;
    assert!(joined(viewing).await.is_ok());
    assert!(s.term.until(0, "echo queued").await, "{}", s.term.text(0));
    s.term.key("escape").await;
    assert_eq!(joined(approval).await, ApprovalDecision::Deny);
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_approval_closes_and_restores_input() {
    let _g = pin().await;
    let s = start(vec![]).await;
    let signal = CancellationToken::new();
    let approval = ask(
        &s.agent,
        bash_request(&s.agent, "cancelled", "echo cancelled", signal.clone()),
    );
    sleep(30).await;
    signal.cancel();
    assert_eq!(joined(approval).await, ApprovalDecision::Deny);
    assert_eq!(s.exit().await, 0);
}

type Events = futures::channel::mpsc::UnboundedSender<anyhow::Result<StreamEvent>>;
type Play = Arc<dyn Fn(usize, Events) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

/// A provider written out by hand: `play(request number, events)` sends the events of each request.
struct Scripted {
    api: String,
    requests: AtomicUsize,
    play: Play,
}

#[async_trait]
impl Provider for Scripted {
    fn api(&self) -> &str {
        &self.api
    }
    fn stream(&self, _req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        let n = self.requests.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = futures::channel::mpsc::unbounded();
        tokio::spawn((self.play)(n, tx));
        Box::pin(rx)
    }
}

/// A gate a scripted stream waits at until the test opens it.
#[derive(Clone)]
struct Gate(tokio::sync::watch::Sender<bool>);

impl Gate {
    fn new() -> Gate {
        Gate(tokio::sync::watch::channel(false).0)
    }
    fn open(&self) {
        self.0.send_replace(true);
    }
    async fn wait(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|v| *v).await;
    }
}

fn partial(content: Vec<AssistantContent>) -> Arc<AssistantMessage> {
    Arc::new(reply(content))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_tool_call_shows_its_arguments_arriving_then_that_its_permission_check_is_running() {
    // A model writing a large file sends the content as the call's arguments for many seconds, and
    // an agent-mode judge or jev-guard then asks a model again: neither may look like a stall.
    let _g = pin().await;
    let input = json!({ "path": "big.txt", "content": "x".repeat(3000) });
    let json = json_of(&input);
    let mid_stream = Gate::new();
    let checked = Gate::new();
    let s = start(vec![]).await;
    let (mid, js, inp) = (mid_stream.clone(), json.clone(), input.clone());
    register_api(
        &s.api,
        Arc::new(Scripted {
            api: s.api.clone(),
            requests: AtomicUsize::new(0),
            play: Arc::new(move |n, tx| {
                let (mid, js, inp) = (mid.clone(), js.clone(), inp.clone());
                Box::pin(async move {
                    let send = |e: StreamEvent| {
                        let _ = tx.unbounded_send(Ok(e));
                    };
                    send(StreamEvent::Start {
                        partial: partial(vec![]),
                    });
                    if n > 0 {
                        send(StreamEvent::Done {
                            message: reply(vec![say("written")]),
                        });
                        return;
                    }
                    let p = partial(vec![call("w1", "write", json!({}))]);
                    send(StreamEvent::ToolcallStart {
                        index: 0,
                        id: "w1".into(),
                        name: "write".into(),
                        partial: p.clone(),
                    });
                    send(StreamEvent::ToolcallDelta {
                        index: 0,
                        delta: js[..2000].to_string(),
                        partial: p.clone(),
                    });
                    mid.wait().await;
                    send(StreamEvent::ToolcallDelta {
                        index: 0,
                        delta: js[2000..].to_string(),
                        partial: p,
                    });
                    send(StreamEvent::Done {
                        message: reply(vec![call("w1", "write", inp)]),
                    });
                })
            }),
        }),
    );
    let gate = checked.clone();
    s.host
        .load("slow-check", move |api: PluginApi| {
            api.on::<hook::Permission, _, _>(move |_e, _ctx| {
                let gate = gate.clone();
                async move {
                    gate.wait().await;
                    Ok(None)
                }
            });
            Box::pin(async { Ok(()) })
        })
        .await;
    s.term.typ("write it").await;
    s.term.key("enter").await;
    assert!(s.term.until(0, "受信中… 2,000 字").await, "{}", s.term.text(0));
    let before = s.term.mark();
    mid_stream.open();
    assert!(
        s.term.until(before, "write の実行前の確認中").await,
        "{}",
        s.term.text(before)
    );
    assert!(!has_tool_result(&s.agent));
    checked.open();
    assert!(s.term.until(0, "written").await, "{}", s.term.text(0));
    let agent = s.agent.clone();
    assert!(
        until(|| agent
            .messages()
            .iter()
            .any(|m| matches!(m, Message::Tool(t) if !t.is_error)))
        .await
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn while_the_model_streams_the_activity_line_counts_its_tokens_up_then_shows_the_real_count() {
    let _g = pin().await;
    let mid_stream = Gate::new();
    let second_reply = Gate::new();
    let s = start(vec![]).await;
    let (mid, second) = (mid_stream.clone(), second_reply.clone());
    register_api(
        &s.api,
        Arc::new(Scripted {
            api: s.api.clone(),
            requests: AtomicUsize::new(0),
            play: Arc::new(move |n, tx| {
                let (mid, second) = (mid.clone(), second.clone());
                Box::pin(async move {
                    let send = |e: StreamEvent| {
                        let _ = tx.unbounded_send(Ok(e));
                    };
                    send(StreamEvent::Start {
                        partial: partial(vec![]),
                    });
                    if n > 0 {
                        second.wait().await;
                        send(StreamEvent::Done {
                            message: reply(vec![say("the end")]),
                        });
                        return;
                    }
                    let thinking = |t: String| {
                        AssistantContent::Thinking(ThinkingContent {
                            thinking: t,
                            signature: None,
                            redacted: None,
                            reasoning_field: None,
                        })
                    };
                    // About 500 tokens.
                    send(StreamEvent::ThinkingDelta {
                        index: 0,
                        delta: "a".repeat(2000),
                        partial: partial(vec![thinking(String::new())]),
                    });
                    mid.wait().await;
                    let done = with_usage(
                        reply(vec![
                            thinking("a".repeat(2000)),
                            call("r1", "read", json!({ "path": "notes.txt" })),
                        ]),
                        Usage {
                            output: 1234,
                            ..Usage::default()
                        },
                    );
                    send(StreamEvent::Done { message: done });
                })
            }),
        }),
    );
    s.term.typ("go").await;
    s.term.key("enter").await;
    let count_re = regex::Regex::new(r"↓ ([\d.]+k?) トークン").unwrap_or_else(|e| panic!("{e}"));
    let counts = || -> Vec<f64> {
        count_re
            .captures_iter(&s.term.text(0))
            .map(|c| {
                let n = &c[1];
                match n.strip_suffix('k') {
                    Some(k) => k.parse::<f64>().unwrap_or(0.0) * 1000.0,
                    None => n.parse().unwrap_or(0.0),
                }
            })
            .collect()
    };
    sleep(1500).await;
    let seen = counts();
    assert!(seen.len() > 3, "{seen:?}");
    assert!(seen[0] < 500.0, "{seen:?}"); // it climbs, not jumps
    assert_eq!(seen.iter().cloned().fold(0.0, f64::max), 500.0, "{seen:?}");
    let mut sorted = seen.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    assert_eq!(seen, sorted);
    // The line names something to be doing, from the list for the current language.
    let text = s.term.text(0);
    assert!(PHRASES_JA.iter().any(|p| text.contains(p)), "{text}");
    let line = regex::Regex::new(r"\d+秒 · ↓ [\d.]+k? トークン · esc で中断").unwrap_or_else(|e| panic!("{e}"));
    assert!(line.is_match(&text), "{text}");
    // The reply ends: the provider's own count (1,234) replaces the estimate, and the next reply adds to it.
    mid_stream.open();
    sleep(1500).await;
    assert!(s.term.text(0).contains("↓ 1.2k トークン"), "{}", s.term.text(0));
    second_reply.open();
    assert!(s.term.until(0, "the end").await, "{}", s.term.text(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn language_en_switches_the_interface_at_once_and_saves_it_language_ja_switches_back() {
    let _g = pin().await;
    let s = start(vec![]).await;
    let plugin = s.host.api("probe");
    assert_eq!(plugin.ui().lang(), Lang::Ja);
    let description = |name: &str| {
        s.host
            .commands()
            .into_iter()
            .find(|c| c.command.name == name)
            .map(|c| c.command.description)
    };
    s.term.typ("/language en").await;
    s.term.key("enter").await;
    assert!(
        s.term.until(0, "Interface language: English").await,
        "{}",
        s.term.text(0)
    );
    assert_eq!(current_lang(), Lang::En);
    assert_eq!(plugin.ui().lang(), Lang::En); // plugins read it live
    assert!(s.calls().contains(&"saveLang:en".to_string()));
    assert!(s.term.until(0, "permissions: reads and edits only").await); // the footer
    assert_eq!(description("clear").as_deref(), Some("start a new session"));
    s.term.typ("/help").await;
    s.term.key("enter").await;
    assert!(
        s.term.until(0, "switch the interface language").await,
        "{}",
        s.term.text(0)
    );
    s.term.typ("/language ja").await;
    s.term.key("enter").await;
    assert!(until(|| current_lang() == Lang::Ja).await);
    assert!(until(|| description("clear").as_deref() == Some("新しいセッションを始める")).await);
    assert!(until(|| s.calls().contains(&"saveLang:ja".to_string())).await);
    s.term.typ("/language klingon").await;
    s.term.key("enter").await;
    assert!(s.term.until(0, "使い方: /language ja|en").await, "{}", s.term.text(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pastes_go_to_the_plugins_first_ctrl_v_and_text_no_plugin_claims() {
    let _g = pin().await;
    let s = start(vec![reply(vec![say("seen")])]).await;
    let offered: Arc<Mutex<Vec<String>>> = Arc::default();
    let o = offered.clone();
    s.host
        .load("paste", move |api: PluginApi| {
            api.ui().on_paste(Arc::new(move |text: String, _input: String| {
                o.lock().unwrap_or_else(|e| e.into_inner()).push(text.clone());
                Box::pin(async move {
                    if text.is_empty() {
                        Some("[Image #1] ".to_string())
                    } else if text.ends_with(".png") {
                        Some("[Image #2] ".to_string())
                    } else {
                        None
                    }
                })
            }));
            Box::pin(async { Ok(()) })
        })
        .await;
    let before = s.term.mark();
    s.term.key("ctrl+v").await;
    assert!(s.term.until(before, "[Image #1]").await, "{}", s.term.text(before)); // shown at once, not on the next key
    // crossterm hands over a bracketed paste whole: the TS's paste split over two reads is one here.
    s.term.paste("/tmp/shot.png").await;
    s.term.paste("plain").await;
    s.term.typ(" ok").await;
    s.term.key("enter").await;
    let agent = s.agent.clone();
    assert!(
        until(|| json_of(&last_user(&agent)).contains("[Image #1] [Image #2] plain ok")).await,
        "{}",
        json_of(&last_user(&agent))
    );
    assert_eq!(
        *offered.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["".to_string(), "/tmp/shot.png".into(), "plain".into()]
    );
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subagents_spending_shows_in_the_footers_cost_and_the_summary_counted_apart() {
    let _g = pin().await;
    let s = start(vec![]).await;
    let before = s.term.mark();
    replay(
        &s.api,
        vec![with_usage(
            reply(vec![say("sub")]),
            Usage {
                input: 50,
                cost: 1.25,
                ..Usage::default()
            },
        )],
        None,
    );
    s.agent
        .run_subagent(SubagentOptions {
            prompt: "x".into(),
            ..SubagentOptions::default()
        })
        .await
        .unwrap_or_else(|e| panic!("subagent: {e}"));
    assert!(s.term.until(before, "$1.250").await, "{}", s.term.text(before));
    let mut tally = UsageTally::new();
    tally.add(&with_usage(
        reply(vec![say("main")]),
        Usage {
            input: 10,
            cost: 0.5,
            ..Usage::default()
        },
    ));
    tally.add_usage(
        "t/m",
        &Usage {
            input: 50,
            cost: 1.25,
            ..Usage::default()
        },
        true,
    );
    let lines = sasacode_tui::testing::strip_for_tests(&usage_lines(&tally).join("\n"));
    assert!(
        lines.contains("2 回（うちサブエージェントなど 1 回） · $1.750"),
        "{lines}"
    );
    assert_eq!(s.exit().await, 0);
}

// The TS checked that nothing reached console.error; here: no error in the transcript, no panic,
// and the app leaves normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_while_a_reasoning_reply_streams_leaves_no_error_and_ends_the_reply() {
    let _g = pin().await;
    let script = vec![with_usage(
        reply(vec![
            AssistantContent::Thinking(ThinkingContent {
                thinking: "let me think about this".into(),
                signature: None,
                redacted: None,
                reasoning_field: None,
            }),
            say(&"a".repeat(3000)),
        ]),
        Usage {
            input: 100,
            output: 10,
            ..Usage::default()
        },
    )];
    let s = start(script.clone()).await;
    replay(&s.api, script, Some(50));
    // Esc as soon as the answer starts, after the thinking block.
    let agent = s.agent.clone();
    let _sub = s.agent.events().on(move |e| {
        if let sasacode_agent::events::AgentEvent::MessageUpdate {
            event: StreamEvent::TextDelta { .. },
            ..
        } = e
        {
            agent.abort();
        }
    });
    s.term.typ("hi").await;
    s.term.key("enter").await;
    sleep(300).await;
    let term = s.term.clone();
    assert_eq!(s.exit().await, 0);
    let text = term.text(0);
    // The reply really was cut short.
    assert!(text.contains("中断しました"), "{text}");
    assert!(!text.contains(&"a".repeat(3000)));
    assert!(!text.contains("エラー"), "{text}");
    assert!(!text.contains("panic"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_gives_a_queued_message_back_to_the_input_instead_of_sending_it() {
    let _g = pin().await;
    let script = vec![reply(vec![bash("1", "sleep 1")]), reply(vec![say("never")])];
    let s = start_with(
        script.clone(),
        Opts {
            mode: PermissionMode::Auto,
            ..Opts::default()
        },
    )
    .await;
    let provider = replay(&s.api, script, None);
    s.term.typ("go").await;
    s.term.key("enter").await;
    sleep(150).await; // the sleep is running
    s.term.typ("also this").await;
    s.term.key("enter").await;
    assert!(s.term.text(0).contains("also this"));
    s.term.key("escape").await;
    sleep(1200).await;
    assert_eq!(provider.requests().len(), 1);
    assert!(!json_of(&s.agent.messages()).contains("also this"));
    let from = s.term.mark();
    s.term.typ("!").await;
    assert!(s.term.until(from, "also this!").await, "{}", s.term.text(from));
    s.term.key("ctrl+u").await; // clear the line
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pasted_text_that_fits_is_shown_as_it_is_a_long_one_is_folded_explained_once_and_sent_in_full() {
    let _g = pin().await;
    let s = start(vec![reply(vec![say("ok")]), reply(vec![say("ok")])]).await;
    let lines = |n: usize| {
        (1..=n)
            .map(|i| format!("copied line {i}"))
            .collect::<Vec<_>>()
            .join("\r\n")
    };
    let before = s.term.mark();
    s.term.paste(&lines(12)).await;
    assert!(s.term.until(before, "copied line 12").await, "{}", s.term.text(before));
    assert!(!s.term.text(before).contains("[paste #"));
    s.term.key("enter").await;
    let agent = s.agent.clone();
    assert!(until(|| json_of(&last_user(&agent)).contains("copied line 1\\ncopied line 2")).await);

    let before = s.term.mark();
    s.term.paste(&lines(60)).await;
    assert!(
        s.term.until(before, "[paste #1 +60 lines]").await,
        "{}",
        s.term.text(before)
    );
    assert!(s.term.text(before).contains("送信すると全文が送られます"));
    s.term.key("enter").await;
    assert!(until(|| json_of(&last_user(&agent)).contains("copied line 60")).await);
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folded_paste_shows_at_once_not_on_the_next_key() {
    let _g = pin().await;
    let s = start(vec![reply(vec![say("ok")]), reply(vec![say("ok")])]).await;
    let long = (0..60).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
    s.term.paste(&long).await; // the first one also shows a note, which renders anyway
    s.term.key("enter").await;
    sleep(100).await;
    let before = s.term.mark();
    s.term.paste(&long).await;
    assert!(s.term.text(before).contains("[paste #"), "{}", s.term.text(before));
    s.term.key("enter").await;
    sleep(100).await;
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backspace_or_delete_at_an_image_marker_removes_the_whole_marker() {
    let _g = pin().await;
    let s = start((0..3).map(|_| reply(vec![say("ok")])).collect()).await;
    let cases: [&[&str]; 3] = [
        &["left", "left", "backspace"],
        &["left", "left", "left", "left", "backspace"],
        &["ctrl+a", "right", "right", "right", "right", "delete"],
    ];
    for (i, keys) in cases.iter().enumerate() {
        s.term.typ("see [Image #1] x").await;
        s.term.keys(keys).await;
        s.term.key("enter").await;
        let agent = s.agent.clone();
        let users = move || {
            agent
                .messages()
                .iter()
                .filter(|m| matches!(m, Message::User(_)))
                .count()
        };
        assert!(until(|| users() > i).await);
        assert!(
            json_of(&last_user(&s.agent)).contains("see  x"),
            "{}",
            json_of(&last_user(&s.agent))
        );
        sleep(100).await;
    }
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_gives_queued_text_back_without_dropping_a_folded_paste_already_in_the_input() {
    let _g = pin().await;
    let script = vec![reply(vec![bash("1", "sleep 1")]), reply(vec![say("ok")])];
    let s = start_with(
        script.clone(),
        Opts {
            mode: PermissionMode::Auto,
            ..Opts::default()
        },
    )
    .await;
    replay(&s.api, script, None);
    s.term.typ("go").await;
    s.term.key("enter").await;
    sleep(150).await;
    s.term.typ("queued").await;
    s.term.key("enter").await;
    s.term
        .paste(&(0..60).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n"))
        .await;
    s.term.key("escape").await;
    sleep(1200).await;
    s.term.key("enter").await;
    let agent = s.agent.clone();
    assert!(
        until(|| json_of(&last_user(&agent)).contains("l59")).await,
        "{}",
        json_of(&last_user(&agent))
    );
    let last = json_of(&last_user(&s.agent));
    assert!(last.contains("queued"), "{last}");
    assert!(!last.contains("[paste #"), "{last}");
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paste_narrows_a_pickers_list_as_typing_does() {
    let _g = pin().await;
    let s = start(vec![]).await;
    let api = s.host.api("picker-test");
    let picked = tokio::spawn(async move {
        api.ui()
            .select(
                "Pick",
                &[SelectOption::new("first", "first"), SelectOption::new("other", "other")],
            )
            .await
    });
    sleep(30).await;
    // One paste: crossterm does not split a bracketed paste as the TS input could.
    s.term.paste("other\n").await;
    assert!(s.term.until(0, "絞り込み: other").await, "{}", s.term.text(0));
    s.term.key("enter").await;
    assert_eq!(joined(picked).await.as_deref(), Some("other"));
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paste_while_the_conversation_search_is_open_goes_to_the_search_not_the_draft() {
    let _g = pin().await;
    let s = start_with(
        vec![],
        Opts {
            alt: true,
            ..Opts::default()
        },
    )
    .await;
    s.term.key("ctrl+shift+f").await;
    s.term.paste("hello").await;
    s.term.key("escape").await; // closes the search
    let from = s.term.mark();
    s.term.typ("!").await;
    assert!(!s.term.text(from).contains("hello"), "{}", s.term.text(from));
    s.term.key("ctrl+u").await;
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_at_an_approval_stops_the_run_esc_in_the_instructions_goes_back_to_the_choices() {
    let _g = pin().await;
    let script = vec![reply(vec![bash("1", "echo nope")]), reply(vec![say("never")])];
    let s = start(script.clone()).await;
    let provider = replay(&s.api, script, None);
    s.term.typ("go").await;
    s.term.key("enter").await;
    assert!(s.term.until(0, "esc で中断").await, "{}", s.term.text(0));
    s.term.keys(&["down", "down", "down"]).await;
    s.term.key("enter").await; // いいえ、代わりの指示を伝える
    assert!(s.term.until(0, "esc で選択に戻る").await, "{}", s.term.text(0));
    let from = s.term.mark();
    s.term.key("escape").await;
    assert!(s.term.until(from, "はい").await, "{}", s.term.text(from));
    s.term.key("escape").await;
    sleep(300).await;
    assert!(!s.agent.is_running());
    assert_eq!(provider.requests().len(), 1);
    assert!(!json_of(&s.agent.messages()).contains("never"));
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_command_scrolls_inside_the_approval_the_choices_stay_on_screen() {
    let _g = pin().await;
    let command = format!("printf {}", "x".repeat(4000));
    let s = start(vec![]).await;
    let approval = ask(
        &s.agent,
        bash_request(&s.agent, "long", &command, CancellationToken::new()),
    );
    assert!(s.term.until(0, "常に許可").await, "{}", s.term.text(0));
    let shown = s.term.text(0);
    assert!(shown.contains("いいえ、代わりの指示を伝える"));
    let first = regex::Regex::new(r"1–\d+ / 4\d 行").unwrap_or_else(|e| panic!("{e}"));
    assert!(first.is_match(&shown), "{shown}");
    let from = s.term.mark();
    s.term.key("shift+down").await;
    let end = regex::Regex::new(r"[1-9]\d–4\d / 4\d 行").unwrap_or_else(|e| panic!("{e}"));
    assert!(end.is_match(&s.term.text(from)), "{}", s.term.text(from)); // the window moved down to the end
    s.term.keys(&["down", "down"]).await;
    s.term.key("enter").await;
    assert_eq!(joined(approval).await, ApprovalDecision::Deny);
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn picker_backspace_takes_a_whole_emoji_no_match_is_said_in_the_interfaces_language() {
    let _g = pin().await;
    let s = start(vec![]).await;
    let api = s.host.api("picker-test");
    let picked = tokio::spawn(async move { api.ui().select("Pick", &[SelectOption::new("first", "first")]).await });
    sleep(30).await;
    s.term.typ("🐱").await;
    assert!(s.term.until(0, "一致するものがありません").await, "{}", s.term.text(0));
    let from = s.term.mark();
    s.term.key("backspace").await;
    let text = s.term.text(from);
    assert!(text.contains("first"), "{text}");
    assert!(!text.contains('\u{fffd}'), "{text}");
    assert!(text.contains("入力で絞り込み"), "{text}");
    s.term.key("escape").await;
    assert_eq!(joined(picked).await, None);
    assert_eq!(s.exit().await, 0);
}

fn command(name: &str, got: Arc<Mutex<Vec<String>>>) -> Command {
    Command::new(name, "x", move |CommandContext { args }| {
        got.lock().unwrap_or_else(|e| e.into_inner()).push(args);
        async { Ok(()) }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_command_whose_argument_is_typed_out_runs_on_the_first_enter() {
    let _g = pin().await;
    let s = start(vec![]).await;
    let got: Arc<Mutex<Vec<String>>> = Arc::default();
    let g = got.clone();
    s.host
        .load("complete-test", move |api: PluginApi| {
            // As jev-guard and workflows do: the exact match too, and a longer one starting with it.
            api.register_command(command("toggle", g).complete(|prefix: String| async move {
                ["on", "off", "offline"]
                    .into_iter()
                    .filter(|v| v.starts_with(&prefix))
                    .map(|v| SelectOption::new(v, v))
                    .collect()
            }));
            Box::pin(async { Ok(()) })
        })
        .await;
    s.term.typ("/toggle of").await;
    assert!(s.term.until(0, "offline").await, "{}", s.term.text(0)); // completion is up
    s.term.typ("f").await;
    s.term.key("enter").await;
    assert!(until(|| !got.lock().unwrap_or_else(|e| e.into_inner()).is_empty()).await);
    assert_eq!(*got.lock().unwrap_or_else(|e| e.into_inner()), vec!["off".to_string()]);
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fully_typed_effort_runs_on_the_first_enter() {
    let _g = pin().await;
    let s = start(vec![]).await;
    s.term.typ("/effort low").await;
    s.term.key("enter").await;
    let agent = s.agent.clone();
    assert!(until(|| agent.thinking() == sasacode_ai::ThinkingLevel::Low).await);
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commands_arguments_keep_their_line_breaks_and_indentation() {
    let _g = pin().await;
    let s = start(vec![]).await;
    let got: Arc<Mutex<Vec<String>>> = Arc::default();
    let g = got.clone();
    s.host
        .load("args-test", move |api: PluginApi| {
            api.register_command(command("argstest", g));
            Box::pin(async { Ok(()) })
        })
        .await;
    s.term.typ("/argstest ").await;
    s.term
        .paste("Traceback:\n  File \"a.py\", line 12\n    total = price * qty")
        .await;
    s.term.key("enter").await;
    assert!(until(|| !got.lock().unwrap_or_else(|e| e.into_inner()).is_empty()).await);
    assert_eq!(
        *got.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["Traceback:\n  File \"a.py\", line 12\n    total = price * qty".to_string()]
    );
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_lists_every_session_and_a_title_is_cut_between_whole_characters() {
    let _g = pin().await;
    let root = tempfile::Builder::new()
        .prefix("sasacode-resume-")
        .tempdir()
        .unwrap_or_else(|e| panic!("{e}"));
    let dir = root.path().join("sessions");
    let cwd = tempfile::Builder::new()
        .prefix("sasacode-tui-")
        .tempdir()
        .unwrap_or_else(|e| panic!("{e}"));
    let cwd_str = cwd.path().display().to_string();
    for i in 0..55 {
        let f = SessionFile::create(&dir, &cwd_str, Vec::new()).unwrap_or_else(|e| panic!("{e}"));
        let text = if i == 54 {
            format!("{}🐱 suffix", "a".repeat(58))
        } else {
            format!("SESSION_{i:02}")
        };
        f.append(&SessionEntry::Message {
            message: Message::User(UserMessage {
                content: vec![UserContent::text(text)],
                timestamp: i,
            }),
        })
        .unwrap_or_else(|e| panic!("{e}"));
        // Written within the same instant otherwise: the list is newest first, so the long title must be newest.
        let at = std::time::UNIX_EPOCH + Duration::from_millis(1_000_000_000_000 + i as u64 * 1000);
        std::fs::File::options()
            .write(true)
            .open(f.path())
            .and_then(|file| file.set_modified(at))
            .unwrap_or_else(|e| panic!("{e}"));
    }
    let s = start_with(
        vec![],
        Opts {
            sessions_dir: Some(dir),
            ..Opts::default()
        },
    )
    .await;
    s.agent.set_cwd(cwd_str);
    s.term.typ("/resume").await;
    s.term.key("enter").await;
    let long = format!("{}🐱…", "a".repeat(58));
    assert!(s.term.until(0, &long).await, "{}", s.term.text(0));
    assert!(!s.term.text(0).contains('\u{fffd}'));
    let from = s.term.mark();
    s.term.typ("SESSION_00").await;
    assert!(s.term.until(from, "SESSION_00").await, "{}", s.term.text(from));
    assert!(!s.term.text(from).contains("一致するものがありません"));
    s.term.key("escape").await;
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_search_box_speaks_japanese_in_a_japanese_interface() {
    let _g = pin().await;
    let s = start_with(
        vec![],
        Opts {
            alt: true,
            ..Opts::default()
        },
    )
    .await;
    s.term.key("ctrl+shift+f").await;
    assert!(s.term.until(0, "会話内を検索").await, "{}", s.term.text(0));
    s.term.typ("zzz-nothing").await;
    assert!(s.term.until(0, "一致するものがありません").await, "{}", s.term.text(0));
    s.term.key("escape").await;
    assert_eq!(s.exit().await, 0);
}

fn last_message_content(request: &Request) -> Vec<UserContent> {
    match request.messages.last() {
        Some(Message::User(u)) => u.content.clone(),
        Some(Message::Tool(t)) => t.content.clone(),
        _ => Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_real_paste_marker_is_expanded_a_typed_one_is_sent_as_written() {
    let _g = pin().await;
    let script = vec![reply(vec![say("ok")])];
    let s = start(script.clone()).await;
    let provider = replay(&s.api, script, None);
    s.term.typ("literal [paste #1] -- ").await;
    s.term
        .paste(&(0..41).map(|i| format!("second-{i}")).collect::<Vec<_>>().join("\n"))
        .await;
    assert!(s.term.until(0, "[paste #1 +41 lines]").await, "{}", s.term.text(0));
    s.term.key("enter").await;
    assert!(until(|| !provider.requests().is_empty()).await);
    let sent = json_of(&last_message_content(&provider.requests()[0]));
    assert!(sent.contains("literal [paste #1] -- second-0"), "{sent}");
    assert_eq!(sent.matches("second-40").count(), 1);
    assert_eq!(s.exit().await, 0);
}

// ── queued input and images (#27, #35, #36, #37) ─────────────────────

const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn png_bytes() -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(PNG)
        .unwrap_or_else(|e| panic!("{e}"))
}

fn images_in(content: &[UserContent]) -> usize {
    json_of(content).matches("\"type\":\"image\"").count()
}

/// The session in auto mode with the image-attach plugin (its clipboard empty) and tiny.png in the cwd.
async fn start_with_images(script: Vec<AssistantMessage>) -> (Started, Arc<ReplayProvider>, String) {
    let s = start_with(
        script.clone(),
        Opts {
            mode: PermissionMode::Auto,
            ..Opts::default()
        },
    )
    .await;
    let provider = replay(&s.api, script, None);
    assert!(
        s.host
            .load("image-attach", |api| {
                sasacode_bundled::image_attach::plugin_with(
                    api,
                    Arc::new(|| Box::pin(async { anyhow::bail!("no clipboard") })),
                )
            })
            .await
    );
    let png = s.cwd().join("tiny.png");
    std::fs::write(&png, png_bytes()).unwrap_or_else(|e| panic!("{e}"));
    (s, provider, png.display().to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_puts_queued_messages_after_the_draft_not_into_it_at_the_cursor() {
    let _g = pin().await;
    let script = vec![reply(vec![bash("1", "sleep 1")]), reply(vec![say("never")])];
    let s = start_with(
        script.clone(),
        Opts {
            mode: PermissionMode::Auto,
            ..Opts::default()
        },
    )
    .await;
    replay(&s.api, script, None);
    s.term.typ("go").await;
    s.term.key("enter").await;
    sleep(150).await;
    s.term.typ("first queued").await;
    s.term.key("enter").await;
    s.term.typ("current draft").await;
    for _ in 0..5 {
        s.term.key("left").await;
    }
    let from = s.term.mark();
    s.term.key("escape").await;
    sleep(1200).await;
    s.term.typ("!").await;
    let shown = s.term.text(from);
    let order = regex::Regex::new(r"current draft\s*\r?\n.*first queued").unwrap_or_else(|e| panic!("{e}"));
    assert!(order.is_match(&shown), "{shown:?}");
    assert!(!shown.contains("queueddraft"));
    assert!(shown.contains("first queued!"), "{shown:?}"); // the cursor ends after what came back
    s.term.key("ctrl+c").await; // ctrl+c clears the input
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_message_with_an_image_leaves_the_queue_once_delivered_esc_then_gives_nothing_back() {
    let _g = pin().await;
    let sleep_for = |id: &str, secs: &str| reply(vec![bash(id, &format!("sleep {secs}"))]);
    let (s, provider, _png) = start_with_images(vec![
        sleep_for("1", "0.4"),
        sleep_for("2", "1.5"),
        reply(vec![say("never")]),
    ])
    .await;
    s.term.typ("go").await;
    s.term.key("enter").await;
    sleep(150).await;
    s.term.typ("please inspect @tiny.png").await;
    s.term.key("enter").await;
    // The first sleep ends, the queued message goes with the next request.
    assert!(until(|| provider.requests().len() > 1).await);
    let requests = provider.requests();
    assert_eq!(images_in(&last_message_content(&requests[1])), 1);
    s.term.key("escape").await;
    sleep(1500).await;
    let from = s.term.mark();
    s.term.typ("!").await;
    assert!(!s.term.text(from).contains("please inspect"), "{}", s.term.text(from));
    assert_eq!(json_of(&s.agent.messages()).matches("please inspect").count(), 1);
    s.term.key("ctrl+u").await;
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_queued_message_keeps_its_image_when_sent_again() {
    let _g = pin().await;
    let (s, provider, png) = start_with_images(vec![reply(vec![bash("1", "sleep 1")]), reply(vec![say("seen")])]).await;
    s.term.typ("go").await;
    s.term.key("enter").await;
    sleep(150).await;
    s.term.paste(&png).await;
    assert!(s.term.until(0, "[Image #1]").await, "{}", s.term.text(0));
    s.term.key("enter").await;
    s.term.key("escape").await;
    sleep(1200).await;
    s.term.key("enter").await; // send what came back
    assert!(until(|| provider.requests().len() > 1).await);
    let last = last_message_content(provider.requests().last().unwrap_or_else(|| panic!("no request")));
    assert_eq!(images_in(&last), 1);
    assert!(
        text_of_user(&last).contains(&format!("[Image 1: {png}]")),
        "{}",
        text_of_user(&last)
    );
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_puts_a_messages_image_back_with_it_also_when_its_file_is_gone_without_doubling_its_label() {
    let _g = pin().await;
    let (s, provider, png) = start_with_images(vec![reply(vec![say("a cat")]), reply(vec![say("again")])]).await;
    s.term.paste(&png).await;
    s.term.typ("what is this image?").await;
    s.term.key("enter").await;
    assert!(until(|| !provider.requests().is_empty()).await);
    assert_eq!(images_in(&last_message_content(&provider.requests()[0])), 1);
    assert!(s.term.until(0, "a cat").await);
    std::fs::remove_file(&png).unwrap_or_else(|e| panic!("{e}"));
    s.term.typ("/fork").await;
    s.term.key("enter").await;
    s.term.key("enter").await; // the only message
    sleep(50).await;
    let from = s.term.mark();
    s.term.typ("!").await;
    assert!(s.term.until(from, "[Image #1]").await, "{}", s.term.text(from));
    s.term.key("backspace").await; // the "!"
    s.term.key("enter").await;
    assert!(until(|| provider.requests().len() > 1).await);
    let last = last_message_content(provider.requests().last().unwrap_or_else(|| panic!("no request")));
    assert_eq!(images_in(&last), 1);
    assert!(
        text_of_user(&last).contains(&format!("[Image 1: {png}]")),
        "{}",
        text_of_user(&last)
    );
    assert!(!json_of(&last).contains("[Image 1: [Image"));
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_image_put_back_by_fork_is_not_sent_for_a_later_pasted_one_once_its_draft_is_cleared() {
    let _g = pin().await;
    let (s, provider, _png) = start_with_images(vec![reply(vec![say("red")]), reply(vec![say("blue")])]).await;
    let red = s.cwd().join("red.png").display().to_string();
    let blue = s.cwd().join("blue.png").display().to_string();
    std::fs::write(&red, png_bytes()).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(&blue, png_bytes()).unwrap_or_else(|e| panic!("{e}"));
    s.term.paste(&red).await;
    s.term.typ("what is this?").await;
    s.term.key("enter").await;
    assert!(s.term.until(0, "red").await);
    sleep(100).await;
    s.term.typ("/fork").await;
    s.term.key("enter").await;
    s.term.key("enter").await; // the only message: "[Image #1] what is this?" comes back
    sleep(50).await;
    s.term.key("ctrl+c").await; // ctrl+c clears the draft
    let from = s.term.mark();
    s.term.paste(&blue).await;
    assert!(s.term.until(from, "[Image #1]").await, "{}", s.term.text(from));
    s.term.typ("and this one?").await;
    s.term.key("enter").await;
    assert!(until(|| provider.requests().len() > 1).await);
    let last = last_message_content(provider.requests().last().unwrap_or_else(|| panic!("no request")));
    assert_eq!(images_in(&last), 1);
    let text = text_of_user(&last);
    assert!(text.contains(&format!("[Image 1: {blue}] and this one?")), "{text}");
    assert!(!text.contains("red.png"), "{text}");
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_a_compaction_what_was_said_stays_on_screen_the_model_gets_the_summary() {
    let _g = pin().await;
    let s = start(vec![reply(vec![say("first answer")])]).await;
    s.term.typ("hello").await;
    s.term.key("enter").await;
    assert!(s.term.until(0, "first answer").await);
    sleep(100).await;
    s.host
        .api("compaction-test")
        .session()
        .replace_messages(vec![Message::User(UserMessage {
            content: vec![UserContent::text(
                "[Summary of the earlier conversation, compacted to save context]\n\nsaid hello",
            )],
            timestamp: 0,
        })])
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(s.agent.messages().len(), 1);
    let from = s.term.mark();
    s.term.key("ctrl+o").await; // ctrl+o redraws everything
    assert!(s.term.until(from, "first answer").await, "{}", s.term.text(from));
    assert_eq!(s.exit().await, 0);
}

// ── not in the TS tests: what the brief lists and app.test.ts never drove ──

/// Each `ESC [ row ; col H` the output since `from` moves to.
fn cursor_moves(out: &str) -> Vec<(usize, usize)> {
    let re = regex::Regex::new(r"\x1b\[(\d+);(\d+)H").unwrap_or_else(|e| panic!("{e}"));
    re.captures_iter(out)
        .map(|c| (c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0)))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resize_redraws_the_inline_ui_at_the_new_width() {
    let _g = pin().await;
    let s = start(vec![reply(vec![say(&"word ".repeat(60))])]).await;
    s.term.typ("hi").await;
    s.term.key("enter").await;
    assert!(s.term.until(0, "word word").await);
    sleep(100).await;
    let from = s.term.mark();
    s.term.0.resize(40, 20);
    assert!(s.term.until(from, "sasacode").await, "{}", s.term.text(from));
    let out = s.term.out()[from..].to_string();
    assert!(
        out.contains("\x1b[2J"),
        "a new width is drawn on a cleared screen: {out:?}"
    );
    let text = s.term.text(from);
    for line in text.split("\r\n") {
        assert!(
            sasacode_tui::text::visible_width(line) <= 40,
            "{line:?} is wider than 40"
        );
    }
    assert!(text.contains("word word"), "{text}");
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resize_redraws_the_full_screen_within_the_new_size() {
    let _g = pin().await;
    let s = start_with(
        vec![reply(vec![say("resized answer")])],
        Opts {
            alt: true,
            ..Opts::default()
        },
    )
    .await;
    s.term.typ("hi").await;
    s.term.key("enter").await;
    assert!(s.term.until(0, "resized answer").await);
    sleep(100).await;
    let from = s.term.mark();
    s.term.0.resize(50, 12);
    assert!(s.term.until(from, "resized answer").await, "{}", s.term.text(from));
    let out = s.term.out()[from..].to_string();
    assert!(out.contains("\x1b[2J"), "{out:?}");
    let moves = cursor_moves(&out);
    assert!(!moves.is_empty());
    assert!(moves.iter().all(|&(r, c)| r <= 12 && c <= 50), "{moves:?}");
    assert_eq!(s.exit().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sent_prompts_are_kept_in_the_history_file_and_recalled_with_up_after_a_restart() {
    let _g = pin().await;
    let dir = tempfile::Builder::new()
        .prefix("sasacode-history-")
        .tempdir()
        .unwrap_or_else(|e| panic!("{e}"));
    let path = dir.path().join("history.jsonl");
    std::fs::write(&path, "\"an older\\nprompt\"\n").unwrap_or_else(|e| panic!("{e}"));
    let opts = || Opts {
        history: Some(path.clone()),
        ..Opts::default()
    };
    let s = start_with(vec![reply(vec![say("ok")])], opts()).await;
    s.term.typ("remember me").await;
    s.term.key("enter").await;
    assert!(s.term.until(0, "ok").await);
    assert_eq!(s.exit().await, 0);
    let saved = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{e}"));
    // Commands too, as the TS kept them.
    assert_eq!(saved, "\"an older\\nprompt\"\n\"remember me\"\n\"/exit\"\n");
    #[cfg(unix)]
    {
        // Created by the app with 0600: write a fresh file to see the mode it gets.
        use std::os::unix::fs::PermissionsExt;
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("{e}"));
        let s = start_with(vec![reply(vec![say("ok")])], opts()).await;
        s.term.typ("private").await;
        s.term.key("enter").await;
        assert!(s.term.until(0, "ok").await);
        assert_eq!(s.exit().await, 0);
        let mode = std::fs::metadata(&path)
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or(0);
        assert_eq!(mode, 0o600);
    }
    std::fs::write(&path, "\"an older\\nprompt\"\n\"remember me\"\n").unwrap_or_else(|e| panic!("{e}"));
    let s = start_with(vec![], opts()).await;
    let from = s.term.mark();
    s.term.key("up").await;
    assert!(s.term.until(from, "remember me").await, "{}", s.term.text(from));
    let from = s.term.mark();
    s.term.key("up").await;
    assert!(s.term.until(from, "an older").await, "{}", s.term.text(from));
    s.term.key("ctrl+c").await;
    assert_eq!(s.exit().await, 0);
}

/// Every SGR in `out` that sets a colour (resets to the default colour are fine).
fn colour_codes(out: &str) -> Vec<String> {
    let re = regex::Regex::new(r"\x1b\[([0-9;:]*)m").unwrap_or_else(|e| panic!("{e}"));
    re.captures_iter(out)
        .filter(|c| {
            let params: Vec<&str> = c[1].split([';', ':']).collect();
            let mut i = 0;
            while i < params.len() {
                let n: u32 = params[i].parse().unwrap_or(0);
                match n {
                    30..=37 | 40..=47 | 90..=97 | 100..=107 | 38 | 48 | 58 => return true,
                    _ => {}
                }
                i += 1;
            }
            false
        })
        .map(|c| c[0].to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_color_leaves_every_colour_out_of_the_inline_ui() {
    let _g = pin().await;
    sasacode_tui::theme::set_no_color(true);
    let s = start_with(
        vec![
            reply(vec![bash("1", "echo hi")]),
            reply(vec![say(
                "# Title\n\n`code` and [a link](https://example.com)\n\n```rust\nfn x() {}\n```",
            )]),
        ],
        Opts {
            mode: PermissionMode::Auto,
            ..Opts::default()
        },
    )
    .await;
    s.term.typ("go").await;
    s.term.key("enter").await;
    assert!(s.term.until(0, "fn x()").await, "{}", s.term.text(0));
    for _ in 0..3 {
        s.term.key("shift+tab").await;
    }
    let term = s.term.clone();
    assert_eq!(s.exit().await, 0);
    let codes = colour_codes(&term.out());
    assert!(codes.is_empty(), "{codes:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_color_leaves_every_colour_out_of_the_full_screen_and_its_scrollbar() {
    let _g = pin().await;
    sasacode_tui::theme::set_no_color(true);
    let long = (0..80).map(|i| format!("row {i}")).collect::<Vec<_>>().join("\n\n");
    let s = start_with(
        vec![reply(vec![say(&long)])],
        Opts {
            alt: true,
            ..Opts::default()
        },
    )
    .await;
    s.term.typ("go").await;
    s.term.key("enter").await;
    assert!(s.term.until(0, "row 79").await, "{}", s.term.text(0));
    let from = s.term.mark();
    s.term.key("pageup").await; // shows the scrollbar
    assert!(s.term.until(from, "┃").await, "{}", s.term.text(from));
    let term = s.term.clone();
    assert_eq!(s.exit().await, 0);
    let codes = colour_codes(&term.out());
    assert!(codes.is_empty(), "{codes:?}");
}
