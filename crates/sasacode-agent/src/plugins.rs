//! The plugin host (`packages/agent/src/plugins.ts`): hands each bundled plugin its own
//! [`PluginApi`] and keeps what they register.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde_json::{Value, json};

use sasacode_ai::{Message, ModelInfo, Provider, ProviderConfig, UserContent};
use sasacode_plugin_api::{
    Command, CompleteOptions, HookRunner, HostServices, InjectWhen, InputOptions, Lang, NotifyLevel, PasteHandler,
    PermissionMode, PermissionRules, PluginApi, SelectManyOptions, SelectOption, ShowTextOptions, SubagentOptions,
    SubagentResult, ToolRef, ToolRenderer, UiBridge,
};

use crate::agent::Agent;
use crate::session::SessionEntry;

/// The front end of a non-interactive run: notices go to stderr, questions get "no".
pub struct HeadlessUi;

#[async_trait]
impl UiBridge for HeadlessUi {
    fn interactive(&self) -> bool {
        false
    }
    fn notify(&self, message: &str, level: NotifyLevel) {
        let prefix = match level {
            NotifyLevel::Info => "",
            NotifyLevel::Warning => "warning: ",
            NotifyLevel::Error => "error: ",
        };
        eprintln!("{prefix}{message}");
    }
    async fn confirm(&self, _title: &str, _message: Option<&str>) -> bool {
        false
    }
    async fn select(&self, _title: &str, _options: &[SelectOption]) -> Option<String> {
        None
    }
}

pub type SettingsFn = Arc<dyn Fn(&str) -> Value + Send + Sync>;
pub type RegisterProviderFn =
    Arc<dyn Fn(&str, ProviderConfig, Option<Arc<dyn Provider>>) -> anyhow::Result<()> + Send + Sync>;

pub struct PluginHostOptions {
    pub agent: Agent,
    pub cwd: String,
    pub settings: Option<SettingsFn>,
    pub register_provider: Option<RegisterProviderFn>,
    /// Tools switched off in config (P3); registrations under these names are ignored.
    pub disabled_tools: Vec<String>,
    /// The language the user reads (PluginUI.lang).
    pub lang: Lang,
}

#[derive(Clone)]
pub struct OwnedCommand {
    pub command: Command,
    pub owner: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedPlugin {
    pub name: String,
    pub error: Option<String>,
}

struct Custom {
    plugin: String,
    kind: String,
    data: Value,
}

pub struct PluginHost {
    agent: Agent,
    cwd: String,
    settings: Option<SettingsFn>,
    register_provider: Option<RegisterProviderFn>,
    disabled_tools: Vec<String>,
    lang: RwLock<Lang>,
    commands: Mutex<Vec<OwnedCommand>>,
    renderers: Mutex<HashMap<String, ToolRenderer>>,
    paste_handlers: Mutex<Vec<PasteHandler>>,
    status: Mutex<IndexMap<String, String>>,
    plugins: Mutex<Vec<LoadedPlugin>>,
    tool_owners: Mutex<HashMap<String, String>>,
    /// Background startup work reported through api.ready().
    pending: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    on_change: RwLock<Option<Arc<dyn Fn() + Send + Sync>>>,
    ui: RwLock<Option<Arc<dyn UiBridge>>>,
    pending_notices: Mutex<Vec<(String, NotifyLevel)>>,
    custom: Mutex<Vec<Custom>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `s.replace(/\x1b/g, "␛").replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, "")`: text from a plugin
/// cannot drive the terminal.
fn clean(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\x1b' => Some('␛'),
            '\x00'..='\x08' | '\x0b'..='\x1f' | '\x7f'..='\u{9f}' => None,
            c => Some(c),
        })
        .collect()
}

impl PluginHost {
    pub fn new(opts: PluginHostOptions) -> Arc<Self> {
        Arc::new(PluginHost {
            agent: opts.agent,
            cwd: opts.cwd,
            settings: opts.settings,
            register_provider: opts.register_provider,
            disabled_tools: opts.disabled_tools,
            lang: RwLock::new(opts.lang),
            commands: Mutex::default(),
            renderers: Mutex::default(),
            paste_handlers: Mutex::default(),
            status: Mutex::default(),
            plugins: Mutex::default(),
            tool_owners: Mutex::default(),
            pending: Mutex::default(),
            on_change: RwLock::default(),
            ui: RwLock::default(),
            pending_notices: Mutex::default(),
            custom: Mutex::default(),
        })
    }

    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    /// The front end; None is the headless one (the default). Notices held until now are shown.
    /// Under the headless UI, info notices are held, never printed (as in the TS).
    pub fn set_ui(&self, ui: Option<Arc<dyn UiBridge>>) {
        *self.ui.write().unwrap_or_else(|e| e.into_inner()) = ui.clone();
        let target: Arc<dyn UiBridge> = ui.unwrap_or_else(|| Arc::new(HeadlessUi));
        for (m, l) in std::mem::take(&mut *lock(&self.pending_notices)) {
            target.notify(&m, l);
        }
    }

    fn current_ui(&self) -> Option<Arc<dyn UiBridge>> {
        self.ui.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_lang(&self, lang: Lang) {
        *self.lang.write().unwrap_or_else(|e| e.into_inner()) = lang;
    }

    pub fn set_on_change(&self, f: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.on_change.write().unwrap_or_else(|e| e.into_inner()) = f;
    }

    /// What runs when the status items or the commands change.
    pub fn on_change(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        self.on_change.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn changed(&self) {
        let f = self.on_change.read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(f) = f {
            f();
        }
    }

    /// Custom entries of the session now loaded (call after resume / clear).
    pub fn set_session_entries(&self, entries: &[SessionEntry]) {
        *lock(&self.custom) = entries
            .iter()
            .filter_map(|e| match e {
                SessionEntry::Custom { plugin, kind, data } => Some(Custom {
                    plugin: plugin.clone(),
                    kind: kind.clone(),
                    data: data.clone(),
                }),
                _ => None,
            })
            .collect();
    }

    pub fn commands(&self) -> Vec<OwnedCommand> {
        lock(&self.commands).clone()
    }
    pub fn renderer(&self, tool: &str) -> Option<ToolRenderer> {
        lock(&self.renderers).get(tool).cloned()
    }
    pub fn paste_handlers(&self) -> Vec<PasteHandler> {
        lock(&self.paste_handlers).clone()
    }
    /// Status items, `plugin:key` → text, in the order they were first set.
    pub fn status(&self) -> IndexMap<String, String> {
        lock(&self.status).clone()
    }
    pub fn plugins(&self) -> Vec<LoadedPlugin> {
        lock(&self.plugins).clone()
    }
    pub fn tool_owner(&self, tool: &str) -> Option<String> {
        lock(&self.tool_owners).get(tool).cloned()
    }

    pub fn api(self: &Arc<Self>, name: &str) -> PluginApi {
        PluginApi::new(name, self.clone() as Arc<dyn HostServices>)
    }

    /// Run a plugin's setup. A failure or panic is reported and recorded, never propagated.
    pub async fn load<F>(self: &Arc<Self>, name: &str, plugin: F) -> bool
    where
        F: FnOnce(PluginApi) -> BoxFuture<'static, anyhow::Result<()>>,
    {
        let api = self.api(name);
        let outcome = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| plugin(api))) {
            Ok(setup) => std::panic::AssertUnwindSafe(setup).catch_unwind().await,
            Err(panic) => Err(panic),
        };
        let error = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(format!("{e:#}")),
            Err(_) => Some("panic".to_string()),
        };
        let ok = error.is_none();
        lock(&self.plugins).push(LoadedPlugin {
            name: name.to_string(),
            error: error.clone(),
        });
        if let Some(error) = error {
            let msg = match HostServices::lang(self.as_ref()) {
                Lang::En => format!("plugin {name} failed to load: {error}"),
                Lang::Ja => format!("プラグイン {name} を読み込めなかった: {error}"),
            };
            HostServices::notify(self.as_ref(), &msg, NotifyLevel::Error);
        }
        ok
    }

    /// Wait for background startup work, at most `timeout`.
    pub async fn settle(&self, timeout: Duration) {
        let handles = std::mem::take(&mut *lock(&self.pending));
        let _ = tokio::time::timeout(timeout, futures::future::join_all(handles)).await;
    }
}

#[async_trait]
impl HostServices for PluginHost {
    fn cwd(&self) -> PathBuf {
        PathBuf::from(&self.cwd)
    }
    fn settings(&self, plugin: &str) -> Value {
        self.settings.as_ref().map(|f| f(plugin)).unwrap_or_else(|| json!({}))
    }
    fn hooks(&self) -> HookRunner {
        self.agent.hooks().clone()
    }

    fn register_tool(&self, plugin: &str, tool: ToolRef) {
        if self.disabled_tools.iter().any(|d| d == tool.name()) {
            return;
        }
        let name = tool.name().to_string();
        self.agent.set_tool(tool);
        lock(&self.tool_owners).insert(name, plugin.to_string());
    }
    fn unregister_tool(&self, plugin: &str, name: &str) {
        // Only the plugin that registered a tool may take it away; a name another plugin owns now
        // (register_tool replaces) is left alone.
        let mut owners = lock(&self.tool_owners);
        if owners.get(name).map(String::as_str) != Some(plugin) {
            return;
        }
        owners.remove(name);
        drop(owners);
        self.agent.remove_tool(name);
    }
    fn register_command(&self, plugin: &str, command: Command) {
        {
            let mut commands = lock(&self.commands);
            commands.retain(|c| c.command.name != command.name);
            commands.push(OwnedCommand {
                command,
                owner: plugin.to_string(),
            });
        }
        self.changed();
    }
    fn register_provider(
        &self,
        name: &str,
        config: ProviderConfig,
        implementation: Option<Arc<dyn Provider>>,
    ) -> anyhow::Result<()> {
        match &self.register_provider {
            Some(f) => f(name, config, implementation),
            None => anyhow::bail!("registerProvider is not available"),
        }
    }
    fn ready(&self, work: BoxFuture<'static, ()>) {
        lock(&self.pending).push(tokio::spawn(work));
    }
    fn add_permission_rules(&self, rules: PermissionRules) -> anyhow::Result<()> {
        self.agent.permissions().add_rules(&rules)
    }
    fn permission_mode(&self) -> PermissionMode {
        self.agent.permissions().mode()
    }

    fn interactive(&self) -> bool {
        self.current_ui().is_some_and(|u| u.interactive())
    }
    fn lang(&self) -> Lang {
        *self.lang.read().unwrap_or_else(|e| e.into_inner())
    }
    fn notify(&self, message: &str, level: NotifyLevel) {
        match self.current_ui() {
            Some(ui) => ui.notify(message, level),
            None if level == NotifyLevel::Info => lock(&self.pending_notices).push((message.to_string(), level)),
            None => HeadlessUi.notify(message, level),
        }
    }
    async fn show_text(&self, options: ShowTextOptions) -> anyhow::Result<()> {
        let safe = ShowTextOptions {
            title: clean(&options.title),
            text: clean(&options.text),
            format: options.format,
        };
        match self.current_ui() {
            Some(ui) => ui.show_text(safe).await,
            None => HeadlessUi.show_text(safe).await,
        }
        Ok(())
    }
    async fn confirm(&self, title: &str, message: Option<&str>) -> bool {
        match self.current_ui() {
            Some(ui) => ui.confirm(title, message).await,
            None => false,
        }
    }
    async fn select(&self, title: &str, options: &[SelectOption]) -> Option<String> {
        self.current_ui()?.select(title, options).await
    }
    async fn input(&self, title: &str, options: &InputOptions) -> Option<String> {
        let ui = self.current_ui()?;
        if !ui.interactive() {
            return None;
        }
        ui.input(title, options).await
    }
    async fn select_many(
        &self,
        title: &str,
        options: &[SelectOption],
        opts: &SelectManyOptions,
    ) -> Option<Vec<String>> {
        let ui = self.current_ui()?;
        if !ui.interactive() {
            return None;
        }
        let picked = ui.select_many(title, options, opts).await?;
        // Only values that were offered, once each, in option order.
        let mut out: Vec<String> = Vec::new();
        for o in options {
            if picked.contains(&o.value) && !out.contains(&o.value) {
                out.push(o.value.clone());
            }
        }
        Some(out)
    }
    fn set_status(&self, plugin: &str, key: &str, text: Option<String>) {
        let k = format!("{plugin}:{key}");
        {
            let mut status = lock(&self.status);
            match text {
                None => {
                    status.shift_remove(&k);
                }
                Some(t) => {
                    status.insert(k, t);
                }
            }
        }
        self.changed();
    }
    fn register_tool_renderer(&self, tool: &str, renderer: ToolRenderer) {
        lock(&self.renderers).insert(tool.to_string(), renderer);
    }
    fn on_paste(&self, handler: PasteHandler) {
        lock(&self.paste_handlers).push(handler);
    }

    fn session_id(&self) -> Option<String> {
        self.agent.session().map(|s| s.id().to_string())
    }
    fn session_append(&self, plugin: &str, kind: &str, data: Value) -> anyhow::Result<()> {
        lock(&self.custom).push(Custom {
            plugin: plugin.to_string(),
            kind: kind.to_string(),
            data: data.clone(),
        });
        if let Some(s) = self.agent.session() {
            s.append(&SessionEntry::Custom {
                plugin: plugin.to_string(),
                kind: kind.to_string(),
                data,
            })?;
        }
        Ok(())
    }
    fn session_entries(&self, plugin: &str, kind: &str) -> Vec<Value> {
        lock(&self.custom)
            .iter()
            .filter(|e| e.plugin == plugin && e.kind == kind)
            .map(|e| e.data.clone())
            .collect()
    }
    fn messages(&self) -> Vec<Message> {
        self.agent.messages()
    }
    fn replace_messages(&self, messages: Vec<Message>) -> anyhow::Result<()> {
        self.agent.replace_messages(messages)
    }
    fn inject(&self, content: Vec<UserContent>, when: InjectWhen) {
        self.agent.inject(content, when);
    }

    fn model(&self) -> ModelInfo {
        self.agent.model()
    }
    fn tools(&self) -> Vec<ToolRef> {
        self.agent.get_tools()
    }
    async fn run_subagent(&self, options: SubagentOptions) -> anyhow::Result<SubagentResult> {
        self.agent.run_subagent(options).await
    }
    async fn complete(&self, options: CompleteOptions) -> anyhow::Result<String> {
        self.agent.complete(options).await
    }
}
