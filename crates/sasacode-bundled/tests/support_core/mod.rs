//! Shared set-up for the `core_*` tests: the `setup()` of `packages/bundled/test/bundled.test.ts`,
//! the `run()` of `repair.test.ts`, and `loadPlugin` of `plugins/test-support.ts`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::permission::PermissionPolicy;
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_agent::session::SessionFile;
use sasacode_agent::tool_exec::{ExecOptions, ToolRunContext, execute_tools};
use sasacode_ai::{
    AssistantContent, AssistantMessage, JsonObject, Message, ModelInfo, Request, StopReason, ToolCall, Usage,
    register_api,
};
use sasacode_plugin_api::{
    Lang, NotifyLevel, PermissionMode, PermissionRules, PluginApi, SelectOption, SessionStartEvent, ToolRef,
    ToolResult, UiBridge, hook,
};
use sasacode_testing::{ReplayOptions, ReplayProvider};

/// Held by tests that assert language text while they set the process-wide language.
pub static LANG: Mutex<()> = Mutex::new(());

pub fn lang_lock(lang: Lang) -> MutexGuard<'static, ()> {
    let g = LANG.lock().unwrap_or_else(|e| e.into_inner());
    sasacode_host::set_lang(lang);
    g
}

/// A replay api name no other test uses.
pub fn api_name() -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    format!(
        "replay-core-{}-{}",
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

pub fn raw_call(id: &str, name: &str, raw: &str) -> AssistantContent {
    AssistantContent::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        input: JsonObject::new(),
        raw_input: Some(raw.into()),
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

/// repair.test.ts `reply`: empty usage, the given timestamp.
pub fn reply_at(content: Vec<AssistantContent>, ts: i64) -> AssistantMessage {
    let mut m = reply(content);
    m.usage = Usage::default();
    m.timestamp = ts;
    m
}

pub fn model(api: &str, context_window: u64, max_output: u64) -> ModelInfo {
    serde_json::from_value(json!({
        "id": "m", "provider": "t", "api": api, "contextWindow": context_window, "maxOutput": max_output,
    }))
    .unwrap_or_else(|e| panic!("model: {e}"))
}

pub fn policy(mode: PermissionMode) -> Arc<PermissionPolicy> {
    Arc::new(PermissionPolicy::new(mode, &PermissionRules::default()).unwrap_or_else(|e| panic!("policy: {e}")))
}

pub fn bundled(name: &str) -> sasacode_plugin_api::PluginFn {
    sasacode_bundled::find(name)
        .unwrap_or_else(|| panic!("no bundled plugin {name}"))
        .plugin
}

pub fn builtin_tools(api: PluginApi) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(sasacode_tools::plugin(api))
}

/// A front end that records notices and says no to everything.
#[derive(Default)]
pub struct NoticeUi {
    pub notices: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl UiBridge for NoticeUi {
    fn interactive(&self) -> bool {
        false
    }
    fn notify(&self, message: &str, _level: NotifyLevel) {
        self.notices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(message.to_string());
    }
    async fn confirm(&self, _t: &str, _m: Option<&str>) -> bool {
        false
    }
    async fn select(&self, _t: &str, _o: &[SelectOption]) -> Option<String> {
        None
    }
}

#[derive(Default)]
pub struct SetupOpts {
    pub cwd: Option<String>,
    pub session: Option<Arc<SessionFile>>,
    pub settings: HashMap<String, Value>,
    pub context_window: Option<u64>,
    pub max_output: Option<u64>,
}

pub struct Setup {
    pub agent: Agent,
    pub host: Arc<PluginHost>,
    pub provider: Arc<ReplayProvider>,
    pub notices: Arc<Mutex<Vec<String>>>,
    pub api: String,
}

impl Setup {
    pub fn requests(&self) -> Vec<Request> {
        self.provider.requests()
    }
}

/// bundled.test.ts `setup()`: mode "edits", system prompt "base", a 100k window, the built-in
/// tools, then `names`, then session_start.
pub async fn setup(script: Vec<AssistantMessage>, names: &[&str], opts: SetupOpts) -> Setup {
    let api = api_name();
    let provider = Arc::new(ReplayProvider::new(script, ReplayOptions::api(&api)));
    register_api(&api, provider.clone());
    let cwd = opts
        .cwd
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
    let mut o = AgentOptions::new(
        model(
            &api,
            opts.context_window.unwrap_or(100_000),
            opts.max_output.unwrap_or(1000),
        ),
        cwd.clone(),
        "base",
    );
    o.permissions = Some(policy(PermissionMode::Edits));
    o.session = opts.session;
    let agent = Agent::new(o);
    let settings = opts.settings;
    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd,
        settings: Some(Arc::new(move |n: &str| {
            settings.get(n).cloned().unwrap_or_else(|| json!({}))
        })),
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: Lang::Ja,
    });
    let ui = NoticeUi::default();
    let notices = ui.notices.clone();
    host.set_ui(Some(Arc::new(ui)));
    host.load("builtin-tools", builtin_tools).await;
    for n in names {
        assert!(
            host.load(n, bundled(n)).await,
            "{n} failed to load: {:?}",
            notices.lock().unwrap()
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
        provider,
        notices,
        api,
    }
}

/// repair.test.ts `run()`: mode "auto", an empty system prompt, the built-in tools and one plugin.
pub async fn run(script: Vec<AssistantMessage>, plugin: &str, cwd: &str) -> Setup {
    let api = api_name();
    let provider = Arc::new(ReplayProvider::new(script, ReplayOptions::api(&api)));
    register_api(&api, provider.clone());
    let mut o = AgentOptions::new(model(&api, 100_000, 1000), cwd, "");
    o.permissions = Some(policy(PermissionMode::Auto));
    let agent = Agent::new(o);
    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd: cwd.into(),
        settings: None,
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: Lang::Ja,
    });
    let ui = NoticeUi::default();
    let notices = ui.notices.clone();
    host.set_ui(Some(Arc::new(ui)));
    host.load("builtin-tools", builtin_tools).await;
    assert!(host.load(plugin, bundled(plugin)).await);
    let _ = agent.prompt_text("go").await;
    Setup {
        agent,
        host,
        provider,
        notices,
        api,
    }
}

/// plugins/test-support.ts `loadPlugin`.
pub struct Loaded {
    pub agent: Agent,
    pub host: Arc<PluginHost>,
    pub notices: Arc<Mutex<Vec<String>>>,
    pub cwd: String,
}

pub async fn load_plugin<F>(name: &str, plugin: F, cwd: &str, settings: Value) -> Loaded
where
    F: FnOnce(PluginApi) -> futures::future::BoxFuture<'static, anyhow::Result<()>>,
{
    // The api is never called: these tests run tools directly.
    let mut o = AgentOptions::new(model("replay", 10_000, 1000), cwd, "test");
    o.model.id = "test".into();
    o.model.provider = "test".into();
    o.permissions = Some(policy(PermissionMode::Edits));
    let agent = Agent::new(o);
    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd: cwd.into(),
        settings: Some(Arc::new(move |_| settings.clone())),
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: Lang::En,
    });
    let ui = NoticeUi::default();
    let notices = ui.notices.clone();
    host.set_ui(Some(Arc::new(ui)));
    if !host.load(name, plugin).await {
        let error = host.plugins().last().and_then(|p| p.error.clone());
        panic!("{name} failed to load: {error:?}");
    }
    Loaded {
        agent,
        host,
        notices,
        cwd: cwd.into(),
    }
}

impl Loaded {
    pub fn tool(&self, name: &str) -> ToolRef {
        self.agent
            .get_tools()
            .into_iter()
            .find(|t| t.name() == name)
            .unwrap_or_else(|| panic!("Missing tool {name}"))
    }

    pub async fn invoke(&self, name: &str, input: Value) -> ToolResult {
        self.invoke_with(name, input, CancellationToken::new()).await
    }

    pub async fn invoke_with(&self, name: &str, input: Value, signal: CancellationToken) -> ToolResult {
        let (t1, a2) = (self.agent.clone(), self.agent.clone());
        let ctx = Arc::new(ToolRunContext {
            cwd: self.cwd.clone(),
            hooks: self.agent.hooks().clone(),
            events: self.agent.events().clone(),
            permissions: self.agent.permissions(),
            approve: self.agent.approve(),
            session: None,
            find_tool: Arc::new(move |n| t1.get_tools().into_iter().find(|t| t.name() == n)),
            all_tools: Arc::new(move || a2.get_tools()),
        });
        let out: Arc<Mutex<Option<ToolResult>>> = Arc::default();
        let o = out.clone();
        let call = ToolCall {
            id: "test-parent".into(),
            name: name.into(),
            input: obj(input),
            raw_input: None,
        };
        execute_tools(
            ctx,
            vec![call],
            ExecOptions {
                truncated: false,
                signal,
                repairs: HashMap::new(),
                on_result: Arc::new(|_| Ok(())),
                ancestors: Vec::new(),
                parent_call_id: None,
                on_tool_result: Some(Arc::new(move |r| {
                    *o.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                })),
            },
        )
        .await
        .unwrap_or_else(|e| panic!("execute_tools: {e}"));
        let r = out.lock().unwrap_or_else(|e| e.into_inner()).take();
        r.unwrap_or_else(|| panic!("{name} gave no result"))
    }
}

/// The text of a tool result's content.
pub fn text_of(r: &ToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text())
        .collect::<Vec<_>>()
        .join("")
}

/// JSON.stringify of a message.
pub fn json_of<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// The last message of a request.
pub fn last(req: &Request) -> &Message {
    req.messages.last().unwrap_or_else(|| panic!("empty request"))
}

pub fn temp_dir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .unwrap_or_else(|e| panic!("tempdir: {e}"))
}

pub fn path_str(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}
