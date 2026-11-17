//! Shared set-up for the `net_*` tests: bundled.test.ts `setup()`, plugins/test-support.ts
//! `loadPlugin`, adopted.test.ts `fake()`, local HTTP servers, the fake browsr/rustxng binaries and
//! a child test process for what needs the environment changed.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use serde_json::{Value, json};

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::permission::{PermissionCheck, PermissionPolicy};
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_agent::session::SessionFile;
use sasacode_ai::{
    AssistantContent, AssistantMessage, JsonObject, ModelInfo, StopReason, ToolCall, Usage, register_api,
};
use sasacode_plugin_api::hooks::{SessionEndEvent, SystemPromptEvent};
use sasacode_plugin_api::*;
use sasacode_testing::{ReplayOptions, ReplayProvider};

// ── language ────────────────────────────────────────────────────────────────

/// Held by a test that asserts texts in one language: the language is process-wide.
pub static LANG: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub async fn lang(l: Lang) -> tokio::sync::MutexGuard<'static, ()> {
    let g = LANG.lock().await;
    sasacode_host::set_lang(l);
    g
}

/// For a sync test.
pub fn lang_blocking(l: Lang) -> tokio::sync::MutexGuard<'static, ()> {
    let g = LANG.blocking_lock();
    sasacode_host::set_lang(l);
    g
}

// ── values ──────────────────────────────────────────────────────────────────

/// A replay api name no other test uses.
pub fn api_name() -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    format!(
        "replay-net-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn obj(v: Value) -> JsonObject {
    v.as_object().cloned().unwrap_or_default()
}

pub fn say(text: &str) -> AssistantContent {
    AssistantContent::text(text)
}

pub fn call(id: &str, name: &str, input: Value) -> AssistantContent {
    AssistantContent::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        input: obj(input),
        raw_input: None,
    })
}

/// bundled.test.ts `reply`: usage.input 10, timestamp 0.
pub fn reply(content: Vec<AssistantContent>) -> AssistantMessage {
    let tool = content.iter().any(|c| matches!(c, AssistantContent::ToolCall(_)));
    AssistantMessage {
        content,
        api: "replay".into(),
        provider: "t".into(),
        model: "m".into(),
        usage: Usage {
            input: 10,
            ..Usage::default()
        },
        stop_reason: if tool { StopReason::ToolUse } else { StopReason::Stop },
        timestamp: 0,
        replay_scope: None,
        error_message: None,
    }
}

pub fn model(api: &str, extra: Value) -> ModelInfo {
    let mut m = json!({ "id": "m", "provider": "t", "api": api, "contextWindow": 100_000, "maxOutput": 1000 });
    if let (Some(m), Some(extra)) = (m.as_object_mut(), extra.as_object()) {
        m.extend(extra.clone());
    }
    serde_json::from_value(m).unwrap_or_else(|e| panic!("model: {e}"))
}

pub fn policy(mode: PermissionMode) -> Arc<PermissionPolicy> {
    Arc::new(PermissionPolicy::new(mode, &PermissionRules::default()).unwrap_or_else(|e| panic!("policy: {e}")))
}

pub fn bundled(name: &str) -> PluginFn {
    sasacode_bundled::find(name)
        .unwrap_or_else(|| panic!("no bundled plugin {name}"))
        .plugin
}

pub fn builtin_tools(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(sasacode_tools::plugin(api))
}

/// The text of a tool result's content.
pub fn text_of(r: &ToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text())
        .collect::<Vec<_>>()
        .join("")
}

pub fn temp_dir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .unwrap_or_else(|e| panic!("tempdir: {e}"))
}

/// A temp dir by its real path (macOS: /var is /private/var), as `realpathSync(mkdtempSync())`.
pub fn real_temp_dir(prefix: &str) -> (tempfile::TempDir, PathBuf) {
    let d = temp_dir(prefix);
    let real = std::fs::canonicalize(d.path()).unwrap_or_else(|e| panic!("canonicalize: {e}"));
    (d, real)
}

pub fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Wait until `cond` holds, `step` ms at a time, at most `times` times.
pub async fn until(times: usize, step: u64, mut cond: impl FnMut() -> bool) {
    for _ in 0..times {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(step)).await;
    }
}

pub async fn sleep(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

// ── a front end ─────────────────────────────────────────────────────────────

/// Records notices (with their level) and says no to everything.
#[derive(Default)]
pub struct NoticeUi {
    pub notices: Arc<Mutex<Vec<(String, NotifyLevel)>>>,
}

#[async_trait]
impl UiBridge for NoticeUi {
    fn interactive(&self) -> bool {
        false
    }
    fn notify(&self, message: &str, level: NotifyLevel) {
        self.notices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((message.to_string(), level));
    }
    async fn confirm(&self, _t: &str, _m: Option<&str>) -> bool {
        false
    }
    async fn select(&self, _t: &str, _o: &[SelectOption]) -> Option<String> {
        None
    }
}

pub type Notices = Arc<Mutex<Vec<(String, NotifyLevel)>>>;

pub fn texts(n: &Notices) -> Vec<String> {
    n.lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(t, _)| t.clone())
        .collect()
}

// ── an agent with plugins ───────────────────────────────────────────────────

/// An agent and its plugin host.
pub struct Setup {
    pub agent: Agent,
    pub host: Arc<PluginHost>,
    pub provider: Option<Arc<ReplayProvider>>,
    pub notices: Notices,
    pub cwd: String,
}

impl Setup {
    pub fn tool_names(&self) -> Vec<String> {
        self.agent.get_tools().iter().map(|t| t.name().to_string()).collect()
    }

    pub fn find_tool(&self, name: &str) -> Option<ToolRef> {
        self.agent.get_tools().into_iter().find(|t| t.name() == name)
    }

    pub fn tool(&self, name: &str) -> ToolRef {
        self.find_tool(name)
            .unwrap_or_else(|| panic!("Missing tool {name}; have {:?}", self.tool_names()))
    }

    /// `tool.execute(args, { cwd, signal })`.
    pub async fn exec(&self, name: &str, args: Value) -> ToolResult {
        self.tool(name)
            .execute(obj(args), ToolContext::new(&self.cwd))
            .await
            .unwrap_or_else(|e| panic!("{name}: {e:#}"))
    }

    /// `agent.permissions.check({ tool, args, cwd }).decision`.
    pub fn decision(&self, name: &str, args: Value) -> Decision {
        let check = PermissionCheck {
            tool: self.tool(name),
            args: obj(args),
            cwd: self.cwd.clone(),
        };
        self.agent.permissions().check(&check).decision
    }

    pub fn notes(&self) -> Vec<String> {
        texts(&self.notices)
    }

    pub fn command(&self, name: &str) -> Command {
        self.host
            .commands()
            .into_iter()
            .find(|c| c.command.name == name)
            .map(|c| c.command)
            .unwrap_or_else(|| panic!("no command {name}"))
    }

    pub async fn run_command(&self, name: &str, args: &str) -> anyhow::Result<()> {
        (self.command(name).run)(CommandContext { args: args.into() }).await
    }

    /// `agent.hooks.run("system_prompt", { prompt })`, folding each result in.
    pub async fn system_prompt(&self, prompt: &str) -> String {
        system_prompt(self.agent.hooks(), prompt).await
    }

    pub async fn session_end(&self) {
        self.agent
            .hooks()
            .emit::<hook::SessionEnd>(SessionEndEvent {
                session_id: None,
                reason: None,
            })
            .await;
    }
}

pub async fn system_prompt(hooks: &HookRunner, prompt: &str) -> String {
    hooks
        .run::<hook::SystemPrompt>(
            SystemPromptEvent {
                prompt: prompt.to_string(),
            },
            |r, e, _| {
                if let Some(p) = r.prompt {
                    e.prompt = p;
                }
                true
            },
        )
        .await
        .prompt
}

#[derive(Default)]
pub struct SetupOpts {
    pub cwd: Option<String>,
    pub session: Option<Arc<SessionFile>>,
    pub settings: HashMap<String, Value>,
}

impl SetupOpts {
    pub fn settings(pairs: &[(&str, Value)]) -> Self {
        SetupOpts {
            settings: pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
            ..SetupOpts::default()
        }
    }
}

/// A host for `agent` with a recording front end and per-plugin settings.
pub fn host_for(agent: &Agent, cwd: &str, settings: HashMap<String, Value>, lang: Lang) -> (Arc<PluginHost>, Notices) {
    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd: cwd.into(),
        settings: Some(Arc::new(move |n: &str| {
            settings.get(n).cloned().unwrap_or_else(|| json!({}))
        })),
        register_provider: None,
        disabled_tools: Vec::new(),
        lang,
    });
    let ui = NoticeUi::default();
    let notices = ui.notices.clone();
    host.set_ui(Some(Arc::new(ui)));
    (host, notices)
}

/// bundled.test.ts `setup()`: mode "edits", system prompt "base", a 100k window, the built-in
/// tools, then `names`, then session_start.
pub async fn setup(script: Vec<AssistantMessage>, names: &[&str], opts: SetupOpts) -> Setup {
    let api = api_name();
    let provider = Arc::new(ReplayProvider::new(script, ReplayOptions::api(&api)));
    register_api(&api, provider.clone());
    let cwd = opts.cwd.unwrap_or_else(|| s(&std::env::temp_dir()));
    let mut o = AgentOptions::new(model(&api, json!({})), cwd.clone(), "base");
    o.permissions = Some(policy(PermissionMode::Edits));
    o.session = opts.session;
    let agent = Agent::new(o);
    let (host, notices) = host_for(&agent, &cwd, opts.settings, Lang::Ja);
    host.load("builtin-tools", builtin_tools).await;
    for n in names {
        assert!(
            host.load(n, bundled(n)).await,
            "{n} failed to load: {:?}",
            texts(&notices)
        );
    }
    agent
        .hooks()
        .emit::<hook::SessionStart>(SessionStartEvent {
            session_id: None,
            resumed: false,
        })
        .await;
    Setup {
        agent,
        host,
        provider: Some(provider),
        notices,
        cwd,
    }
}

/// plugins/test-support.ts `loadPlugin`: one plugin, mode "edits", the host in English.
pub async fn load_plugin<F>(name: &str, plugin: F, cwd: &str, settings: Value) -> Setup
where
    F: FnOnce(PluginApi) -> BoxFuture<'static, anyhow::Result<()>>,
{
    let mut o = AgentOptions::new(
        model(
            "replay",
            json!({ "id": "test", "provider": "test", "contextWindow": 10_000 }),
        ),
        cwd,
        "test",
    );
    o.permissions = Some(policy(PermissionMode::Edits));
    let agent = Agent::new(o);
    let mut all = HashMap::new();
    all.insert(name.to_string(), settings);
    let (host, notices) = host_for(&agent, cwd, all, Lang::En);
    if !host.load(name, plugin).await {
        let error = host.plugins().last().and_then(|p| p.error.clone());
        panic!("{name} failed to load: {error:?}");
    }
    Setup {
        agent,
        host,
        provider: None,
        notices,
        cwd: cwd.into(),
    }
}

// ── adopted.test.ts `fake()` ────────────────────────────────────────────────

/// What the fake host recorded.
#[derive(Default)]
pub struct Recorded {
    pub tools: Vec<ToolRef>,
    pub commands: Vec<Command>,
    pub notices: Vec<String>,
    pub status: HashMap<String, String>,
    pub injected: Vec<String>,
}

pub type Entries = Arc<Mutex<HashMap<String, Vec<Value>>>>;

/// Just enough of a host to drive one plugin: session "s1" with the given entries, an interactive
/// front end, an anthropic model without a baseUrl. Status keys are the plugin's own (no prefix).
pub struct FakeHost {
    pub cwd: PathBuf,
    pub settings: Value,
    pub hooks: HookRunner,
    pub entries: Entries,
    pub rec: Mutex<Recorded>,
}

impl FakeHost {
    pub fn new(cwd: &Path, settings: Value, entries: Entries) -> Arc<FakeHost> {
        Arc::new(FakeHost {
            cwd: cwd.to_path_buf(),
            settings,
            hooks: HookRunner::default(),
            entries,
            rec: Mutex::new(Recorded::default()),
        })
    }

    /// Run the plugin's setup.
    pub async fn load<F>(self: &Arc<Self>, name: &str, plugin: F) -> anyhow::Result<()>
    where
        F: FnOnce(PluginApi) -> BoxFuture<'static, anyhow::Result<()>>,
    {
        plugin(PluginApi::new(name, self.clone())).await
    }

    fn rec(&self) -> std::sync::MutexGuard<'_, Recorded> {
        self.rec.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn tool(&self, name: &str) -> ToolRef {
        self.rec()
            .tools
            .iter()
            .find(|t| t.name() == name)
            .cloned()
            .unwrap_or_else(|| panic!("no tool {name}"))
    }

    pub async fn exec(&self, name: &str, args: Value) -> ToolResult {
        self.tool(name)
            .execute(obj(args), ToolContext::new(&self.cwd))
            .await
            .unwrap_or_else(|e| panic!("{name}: {e:#}"))
    }

    pub async fn run_command(&self, name: &str, args: &str) -> anyhow::Result<()> {
        let c = self.rec().commands.iter().find(|c| c.name == name).cloned();
        let c = c.unwrap_or_else(|| panic!("no command {name}"));
        (c.run)(CommandContext { args: args.into() }).await
    }

    pub fn injected(&self) -> Vec<String> {
        self.rec().injected.clone()
    }

    pub fn status(&self, key: &str) -> Option<String> {
        self.rec().status.get(key).cloned()
    }

    pub fn status_len(&self) -> usize {
        self.rec().status.len()
    }

    pub fn notices(&self) -> Vec<String> {
        self.rec().notices.clone()
    }

    pub async fn session_start(&self) {
        self.hooks
            .emit::<hook::SessionStart>(SessionStartEvent {
                session_id: Some("s1".into()),
                resumed: false,
            })
            .await;
    }

    pub async fn session_end(&self) {
        self.hooks
            .emit::<hook::SessionEnd>(SessionEndEvent {
                session_id: Some("s1".into()),
                reason: None,
            })
            .await;
    }

    pub async fn system_prompt(&self, prompt: &str) -> String {
        system_prompt(&self.hooks, prompt).await
    }

    pub async fn agent_end(&self, cause: StopCause, error: Option<&str>) {
        self.hooks
            .emit::<hook::AgentEnd>(AgentEndEvent {
                cause,
                stopped: None,
                error: error.map(str::to_string),
            })
            .await;
    }
}

#[async_trait]
impl HostServices for FakeHost {
    fn cwd(&self) -> PathBuf {
        self.cwd.clone()
    }
    fn settings(&self, _plugin: &str) -> Value {
        self.settings.clone()
    }
    fn hooks(&self) -> HookRunner {
        self.hooks.clone()
    }
    fn register_tool(&self, _plugin: &str, tool: ToolRef) {
        let mut r = self.rec();
        r.tools.retain(|t| t.name() != tool.name());
        r.tools.push(tool);
    }
    fn unregister_tool(&self, _plugin: &str, name: &str) {
        self.rec().tools.retain(|t| t.name() != name);
    }
    fn register_command(&self, _plugin: &str, command: Command) {
        let mut r = self.rec();
        r.commands.retain(|c| c.name != command.name);
        r.commands.push(command);
    }
    fn register_provider(&self, _n: &str, _c: ProviderConfig, _i: Option<Arc<dyn Provider>>) -> anyhow::Result<()> {
        Ok(())
    }
    fn ready(&self, work: BoxFuture<'static, ()>) {
        tokio::spawn(work);
    }
    fn add_permission_rules(&self, _rules: PermissionRules) -> anyhow::Result<()> {
        Ok(())
    }
    fn permission_mode(&self) -> PermissionMode {
        PermissionMode::Edits
    }
    fn interactive(&self) -> bool {
        true
    }
    fn lang(&self) -> Lang {
        Lang::Ja
    }
    fn notify(&self, message: &str, _level: NotifyLevel) {
        self.rec().notices.push(message.to_string());
    }
    async fn show_text(&self, options: ShowTextOptions) -> anyhow::Result<()> {
        self.notify(&format!("{}\n{}", options.title, options.text), NotifyLevel::Info);
        Ok(())
    }
    async fn confirm(&self, _title: &str, _message: Option<&str>) -> bool {
        false
    }
    async fn select(&self, _title: &str, _options: &[SelectOption]) -> Option<String> {
        None
    }
    async fn input(&self, _title: &str, _options: &InputOptions) -> Option<String> {
        None
    }
    async fn select_many(&self, _t: &str, _o: &[SelectOption], _s: &SelectManyOptions) -> Option<Vec<String>> {
        None
    }
    fn set_status(&self, _plugin: &str, key: &str, text: Option<String>) {
        let mut r = self.rec();
        match text.filter(|t| !t.is_empty()) {
            Some(t) => r.status.insert(key.to_string(), t),
            None => r.status.remove(key),
        };
    }
    fn register_tool_renderer(&self, _tool: &str, _renderer: ToolRenderer) {}
    fn on_paste(&self, _handler: PasteHandler) {}
    fn session_id(&self) -> Option<String> {
        Some("s1".into())
    }
    fn session_append(&self, _plugin: &str, kind: &str, data: Value) -> anyhow::Result<()> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(kind.to_string())
            .or_default()
            .push(data);
        Ok(())
    }
    fn session_entries(&self, _plugin: &str, kind: &str) -> Vec<Value> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(kind)
            .cloned()
            .unwrap_or_default()
    }
    fn messages(&self) -> Vec<Message> {
        Vec::new()
    }
    fn replace_messages(&self, _messages: Vec<Message>) -> anyhow::Result<()> {
        Ok(())
    }
    fn inject(&self, content: Vec<UserContent>, _when: InjectWhen) {
        let text: String = content.iter().filter_map(UserContent::as_text).collect();
        self.rec().injected.push(text);
    }
    fn model(&self) -> ModelInfo {
        model("anthropic", json!({}))
    }
    fn tools(&self) -> Vec<ToolRef> {
        self.rec().tools.clone()
    }
    async fn run_subagent(&self, _options: SubagentOptions) -> anyhow::Result<SubagentResult> {
        anyhow::bail!("no subagents in tests")
    }
    async fn complete(&self, _options: CompleteOptions) -> anyhow::Result<String> {
        anyhow::bail!("no model in tests")
    }
}

// ── local HTTP servers ──────────────────────────────────────────────────────

/// A server on 127.0.0.1 (`Bun.serve`). Dropping it stops it.
pub struct Server {
    pub port: u16,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Server {
    /// `http://localhost:<port>`
    pub fn origin(&self) -> String {
        format!("http://localhost:{}", self.port)
    }

    /// `http://127.0.0.1:<port>/`
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    /// `server.stop(true)`: no more connections, open ones closed.
    pub async fn stop(mut self) {
        self.shut().await;
    }

    async fn shut(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(mut task) = self.task.take()
            && tokio::time::timeout(Duration::from_secs(1), &mut task).await.is_err()
        {
            task.abort();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn listener_on(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    let socket = tokio::net::TcpSocket::new_v4()?;
    socket.set_reuseaddr(true)?;
    socket.bind(std::net::SocketAddr::from(([127, 0, 0, 1], port)))?;
    socket.listen(1024)
}

/// Serve `app` on a free port.
pub async fn serve(app: axum::Router) -> Server {
    serve_on(0, app).await
}

/// Serve `app` on `port` (0: a free one).
pub async fn serve_on(port: u16, app: axum::Router) -> Server {
    let listener = listener_on(port).unwrap_or_else(|e| panic!("listen on {port}: {e}"));
    serve_listener(listener, app)
}

fn serve_listener(listener: tokio::net::TcpListener, app: axum::Router) -> Server {
    let port = listener
        .local_addr()
        .map(|a| a.port())
        .unwrap_or_else(|e| panic!("local_addr: {e}"));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = stopped.await;
            })
            .await;
    });
    Server {
        port,
        stop: Some(stop),
        task: Some(task),
    }
}

/// A port nothing listens on now.
pub fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("bind: {e}"));
    l.local_addr()
        .map(|a| a.port())
        .unwrap_or_else(|e| panic!("local_addr: {e}"))
}

/// A server that answers every request with "ok".
pub async fn ok_server_on(port: u16) -> Server {
    serve_on(port, axum::Router::new().fallback(|| async { "ok" })).await
}

/// A port held by a socket that is bound but does not listen: connections to it are refused, and
/// no other server (another test's, another process's) can take it before `serve_ok`, as one freed
/// by `free_port` and bound again later could be.
pub struct ReservedPort {
    socket: tokio::net::TcpSocket,
    pub port: u16,
}

pub fn reserve_port() -> ReservedPort {
    let socket = tokio::net::TcpSocket::new_v4().unwrap_or_else(|e| panic!("socket: {e}"));
    socket
        .bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
        .unwrap_or_else(|e| panic!("bind: {e}"));
    let port = socket
        .local_addr()
        .map(|a| a.port())
        .unwrap_or_else(|e| panic!("local_addr: {e}"));
    ReservedPort { socket, port }
}

impl ReservedPort {
    /// Start answering every request on the port with "ok".
    pub fn serve_ok(self) -> Server {
        let listener = self.socket.listen(1024).unwrap_or_else(|e| panic!("listen: {e}"));
        serve_listener(listener, axum::Router::new().fallback(|| async { "ok" }))
    }
}

// ── the stand-in binaries ───────────────────────────────────────────────────

/// An example of this crate, built by `cargo test -p sasacode-bundled`.
pub fn example_bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|e| panic!("test binary path: {e}"));
    // target/<profile>/deps/<test> → target/<profile>/examples/<name>
    let profile = exe
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| panic!("target dir"));
    let bin = profile
        .join("examples")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(
        bin.exists(),
        "{} is missing: `cargo test -p sasacode-bundled` builds it (or cargo build -p sasacode-bundled --example {name})",
        bin.display()
    );
    bin
}

/// fake-browsr as a command; with `env`, a wrapper script that sets it first (unix).
pub fn fake_browsr(dir: &Path, name: &str, env: &str) -> String {
    let bin = example_bin("net-fake-browsr");
    if env.is_empty() {
        return s(&bin);
    }
    let wrapper = dir.join(name);
    write_script(&wrapper, &format!("#!/bin/sh\n{env} exec '{}' \"$@\"\n", bin.display()));
    s(&wrapper)
}

pub fn fake_rustxng() -> String {
    s(&example_bin("net-fake-rustxng"))
}

/// An executable file (unix).
pub fn write_script(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
    }
}

// ── a child test process ────────────────────────────────────────────────────

const CHILD: &str = "SASACODE_NET_TEST_CHILD";

/// True in a child test process started by [`run_child`] / [`in_child`].
pub fn is_child() -> bool {
    std::env::var_os(CHILD).is_some()
}

/// Run `test` again in a child test process with `env` added (`process.env.X = …` in the TS, which
/// this process cannot do safely), and check it passed.
pub fn run_child_blocking(test: &str, env: &[(&str, String)]) {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap_or_else(|e| panic!("exe: {e}")));
    cmd.args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD, "1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap_or_else(|e| panic!("child: {e}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "{test} in its child process:\n{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// [`run_child_blocking`] from an async test, its runtime (and servers) going on meanwhile.
pub async fn run_child(test: &str, env: &[(&str, String)]) {
    let test = test.to_string();
    let env: Vec<(String, String)> = env.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    let r = tokio::task::spawn_blocking(move || {
        let env: Vec<(&str, String)> = env.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        run_child_blocking(&test, &env);
    })
    .await;
    match r {
        Ok(()) => {}
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("child: {e}"),
    }
}

/// For a test whose whole body needs the environment changed: true in the child, where the body
/// runs; in the parent, runs that child and returns false.
pub fn in_child(test: &str, env: &[(&str, String)]) -> bool {
    if is_child() {
        return true;
    }
    run_child_blocking(test, env);
    false
}
