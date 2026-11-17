//! `PluginAPI`: the handle each bundled plugin gets. It forwards to [`HostServices`], which the
//! plugin host (sasacode-agent) implements, so this crate does not depend on the agent.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use sasacode_ai::{Message, ModelInfo, Provider, ProviderConfig, ToolCall, UserContent};

use crate::hooks::{Hook, HookContext, HookRunner, PermissionMode, StopCause};
use crate::tool::{ToolRef, ToolResult};

pub const PLUGIN_API_VERSION: &str = "1.15.0";

// ── commands ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct CommandContext {
    pub args: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl SelectOption {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        SelectOption { value: value.into(), label: label.into(), description: None }
    }
    pub fn describe(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }
}

pub type CommandRun = Arc<dyn Fn(CommandContext) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
pub type CommandComplete = Arc<dyn Fn(String) -> BoxFuture<'static, Vec<SelectOption>> + Send + Sync>;

#[derive(Clone)]
pub struct Command {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    pub run: CommandRun,
    /// Candidates for the argument typed so far, offered by tab completion.
    pub complete: Option<CommandComplete>,
}

impl Command {
    pub fn new<F, Fut>(name: &str, description: &str, run: F) -> Self
    where
        F: Fn(CommandContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        Command {
            name: name.to_string(),
            description: description.to_string(),
            argument_hint: None,
            run: Arc::new(move |c| Box::pin(run(c))),
            complete: None,
        }
    }
    pub fn hint(mut self, h: &str) -> Self {
        self.argument_hint = Some(h.to_string());
        self
    }
    pub fn complete<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Vec<SelectOption>> + Send + 'static,
    {
        self.complete = Some(Arc::new(move |p| Box::pin(f(p))));
        self
    }
}

// ── ui ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NotifyLevel {
    #[default]
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    Ja,
    En,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TextFormat {
    #[default]
    Text,
    Markdown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShowTextOptions {
    pub title: String,
    pub text: String,
    /// Presentation only; no HTML, scripts or automatic link opening.
    pub format: TextFormat,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputOptions {
    /// A line under the title, e.g. what the answer is for.
    pub message: Option<String>,
    pub placeholder: Option<String>,
    /// Text the field starts with.
    pub initial: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SelectManyOptions {
    /// Values checked when the list opens.
    pub selected: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    pub expanded: bool,
    pub width: u16,
}

/// Returns body lines (ANSI allowed, each at most `width` wide) or None for the default rendering.
pub type ToolRenderer = Arc<dyn Fn(&ToolCall, Option<&ToolResult>, RenderOptions) -> Option<Vec<String>> + Send + Sync>;

/// Pasted or dropped text before it reaches the input; "" for ctrl+v or a paste without text.
/// `input` is what the input line holds now. Returns what to insert instead.
pub type PasteHandler = Arc<dyn Fn(String, String) -> BoxFuture<'static, Option<String>> + Send + Sync>;

/// What a front end (TUI or headless) provides to plugins (`UIBridge`).
#[async_trait]
pub trait UiBridge: Send + Sync {
    fn interactive(&self) -> bool;
    fn notify(&self, message: &str, level: NotifyLevel);
    /// Read-only text. The default shows it as a notice.
    async fn show_text(&self, options: ShowTextOptions) {
        self.notify(&format!("{}\n{}", options.title, options.text), NotifyLevel::Info);
    }
    async fn confirm(&self, title: &str, message: Option<&str>) -> bool;
    async fn select(&self, title: &str, options: &[SelectOption]) -> Option<String>;
    /// One-line input; None on cancel or when the front end has no input dialog.
    async fn input(&self, _title: &str, _options: &InputOptions) -> Option<String> {
        None
    }
    /// Checked values; None on cancel or when the front end has no multi-select.
    async fn select_many(&self, _title: &str, _options: &[SelectOption], _opts: &SelectManyOptions) -> Option<Vec<String>> {
        None
    }
}

// ── session and agent services ───────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InjectWhen {
    /// Delivered with the next turn (or the next prompt).
    #[default]
    Next,
    /// Also starts a run if idle.
    Now,
}

/// Controls of a running subagent: `send` adds a user message at its next turn, `stop` aborts it.
#[derive(Clone)]
pub struct SubagentControl {
    pub send: Arc<dyn Fn(String) + Send + Sync>,
    pub stop: Arc<dyn Fn() + Send + Sync>,
}

#[derive(Clone, Default)]
pub struct SubagentOptions {
    pub prompt: String,
    pub system_prompt: Option<String>,
    /// Tool names to offer; defaults to every tool except those in `exclude_tools`.
    pub tools: Option<Vec<String>>,
    pub exclude_tools: Vec<String>,
    /// "<provider>/<model>"; defaults to the current model.
    pub model: Option<String>,
    pub signal: Option<CancellationToken>,
    pub on_progress: Option<Arc<dyn Fn(String) + Send + Sync>>,
    /// Called once the loop starts.
    pub on_start: Option<Arc<dyn Fn(SubagentControl) + Send + Sync>>,
}

#[derive(Debug, Clone)]
pub struct SubagentResult {
    pub text: String,
    pub messages: Vec<Message>,
    pub cause: StopCause,
    /// Why it stopped, when cause is Error.
    pub error: Option<String>,
}

#[derive(Clone, Default)]
pub struct CompleteOptions {
    pub system: String,
    pub messages: Vec<Message>,
    pub model: Option<String>,
    /// Default 8192, lowered to the room left in the model's window and to its maxOutput.
    pub max_tokens: Option<u64>,
    pub signal: Option<CancellationToken>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRules {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ask: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
    /// Denied like `deny`, unless a `permission` hook hands the call to the user.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub soft_deny: Vec<String>,
}

/// Everything a plugin can reach, implemented by the plugin host. Methods that act for a plugin
/// take its name.
#[async_trait]
pub trait HostServices: Send + Sync {
    fn cwd(&self) -> PathBuf;
    /// `plugins.settings.<plugin>` from config (an object; `{}` when unset).
    fn settings(&self, plugin: &str) -> Value;
    fn hooks(&self) -> HookRunner;

    fn register_tool(&self, plugin: &str, tool: ToolRef);
    /// Only the plugin that registered a tool may take it away.
    fn unregister_tool(&self, plugin: &str, name: &str);
    fn register_command(&self, plugin: &str, command: Command);
    fn register_provider(&self, name: &str, config: ProviderConfig, implementation: Option<Arc<dyn Provider>>)
    -> anyhow::Result<()>;
    /// Background startup work: headless waits for it before the first request.
    fn ready(&self, work: BoxFuture<'static, ()>);
    /// Errs on a rule that does not parse.
    fn add_permission_rules(&self, rules: PermissionRules) -> anyhow::Result<()>;
    fn permission_mode(&self) -> PermissionMode;

    // ui
    fn interactive(&self) -> bool;
    fn lang(&self) -> Lang;
    fn notify(&self, message: &str, level: NotifyLevel);
    async fn show_text(&self, options: ShowTextOptions) -> anyhow::Result<()>;
    async fn confirm(&self, title: &str, message: Option<&str>) -> bool;
    async fn select(&self, title: &str, options: &[SelectOption]) -> Option<String>;
    async fn input(&self, title: &str, options: &InputOptions) -> Option<String>;
    async fn select_many(&self, title: &str, options: &[SelectOption], opts: &SelectManyOptions) -> Option<Vec<String>>;
    fn set_status(&self, plugin: &str, key: &str, text: Option<String>);
    fn register_tool_renderer(&self, tool: &str, renderer: ToolRenderer);
    fn on_paste(&self, handler: PasteHandler);

    // session
    fn session_id(&self) -> Option<String>;
    /// Errs when the log cannot be written.
    fn session_append(&self, plugin: &str, kind: &str, data: Value) -> anyhow::Result<()>;
    fn session_entries(&self, plugin: &str, kind: &str) -> Vec<Value>;
    fn messages(&self) -> Vec<Message>;
    fn replace_messages(&self, messages: Vec<Message>) -> anyhow::Result<()>;
    fn inject(&self, content: Vec<UserContent>, when: InjectWhen);

    // agent
    fn model(&self) -> ModelInfo;
    fn tools(&self) -> Vec<ToolRef>;
    async fn run_subagent(&self, options: SubagentOptions) -> anyhow::Result<SubagentResult>;
    async fn complete(&self, options: CompleteOptions) -> anyhow::Result<String>;
}

/// The handle a plugin gets (`PluginAPI`).
#[derive(Clone)]
pub struct PluginApi {
    name: String,
    host: Arc<dyn HostServices>,
}

impl PluginApi {
    pub fn new(name: impl Into<String>, host: Arc<dyn HostServices>) -> Self {
        PluginApi { name: name.into(), host }
    }
    pub fn version(&self) -> &'static str {
        PLUGIN_API_VERSION
    }
    /// This plugin's name.
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn cwd(&self) -> PathBuf {
        self.host.cwd()
    }
    pub fn host(&self) -> &Arc<dyn HostServices> {
        &self.host
    }
    /// This plugin's settings from config (`plugins.settings.<name>`).
    pub fn settings(&self) -> Value {
        self.host.settings(&self.name)
    }
    /// Validate settings against `schema` merged over `defaults`; errors name the field, never the value.
    pub fn define_settings(&self, schema: &Value, defaults: Option<&Value>) -> anyhow::Result<Value> {
        crate::settings::define_settings(&self.name, &self.settings(), schema, defaults)
    }
    /// Adding a tool with an existing name replaces it (P3).
    pub fn register_tool(&self, tool: ToolRef) {
        self.host.register_tool(&self.name, tool);
    }
    pub fn unregister_tool(&self, name: &str) {
        self.host.unregister_tool(&self.name, name);
    }
    pub fn register_command(&self, command: Command) {
        self.host.register_command(&self.name, command);
    }
    /// Add a provider (`name`) and, optionally, the implementation for its api (FR-P06).
    pub fn register_provider(
        &self,
        name: &str,
        config: ProviderConfig,
        implementation: Option<Arc<dyn Provider>>,
    ) -> anyhow::Result<()> {
        self.host.register_provider(name, config, implementation)
    }
    /// Register a handler for hook `H` (see [`crate::hooks::hook`]).
    pub fn on<H, F, Fut>(&self, f: F)
    where
        H: Hook,
        F: Fn(H::Event, HookContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<Option<H::Result>>> + Send + 'static,
    {
        self.host.hooks().on::<H, F, Fut>(&self.name, f);
    }
    /// Register a synchronous handler (stream_delta).
    pub fn on_sync<H, F>(&self, f: F)
    where
        H: Hook,
        F: Fn(&H::Event, &HookContext) -> anyhow::Result<Option<H::Result>> + Send + Sync + 'static,
    {
        self.host.hooks().on_sync::<H, F>(&self.name, f);
    }
    /// Headless waits for ready work before requests; the TUI stays responsive.
    pub fn ready(&self, work: impl std::future::Future<Output = ()> + Send + 'static) {
        self.host.ready(Box::pin(work));
    }
    pub fn add_permission_rules(&self, rules: PermissionRules) -> anyhow::Result<()> {
        self.host.add_permission_rules(rules)
    }
    pub fn ui(&self) -> Ui<'_> {
        Ui { api: self }
    }
    pub fn session(&self) -> Session<'_> {
        Session { api: self }
    }
    pub fn agent(&self) -> Agent<'_> {
        Agent { api: self }
    }
    /// Debug output, shown only with SASACODE_DEBUG set.
    pub fn log(&self, message: &str) {
        if std::env::var_os("SASACODE_DEBUG").is_some() {
            eprintln!("[{}] {message}", self.name);
        }
    }
}

/// `api.ui`.
pub struct Ui<'a> {
    api: &'a PluginApi,
}

impl Ui<'_> {
    /// False in headless runs: confirm answers false and select None.
    pub fn interactive(&self) -> bool {
        self.api.host.interactive()
    }
    /// The language the user reads the interface in (read live: /language switches it).
    pub fn lang(&self) -> Lang {
        self.api.host.lang()
    }
    pub fn notify(&self, message: &str, level: NotifyLevel) {
        self.api.host.notify(message, level);
    }
    pub async fn show_text(&self, options: ShowTextOptions) -> anyhow::Result<()> {
        self.api.host.show_text(options).await
    }
    pub async fn confirm(&self, title: &str, message: Option<&str>) -> bool {
        self.api.host.confirm(title, message).await
    }
    pub async fn select(&self, title: &str, options: &[SelectOption]) -> Option<String> {
        self.api.host.select(title, options).await
    }
    pub async fn input(&self, title: &str, options: &InputOptions) -> Option<String> {
        self.api.host.input(title, options).await
    }
    pub async fn select_many(
        &self,
        title: &str,
        options: &[SelectOption],
        opts: &SelectManyOptions,
    ) -> Option<Vec<String>> {
        self.api.host.select_many(title, options, opts).await
    }
    /// Show (or clear with None) a status line item.
    pub fn set_status(&self, key: &str, text: Option<String>) {
        self.api.host.set_status(&self.api.name, key, text);
    }
    pub fn register_tool_renderer(&self, tool: &str, renderer: ToolRenderer) {
        self.api.host.register_tool_renderer(tool, renderer);
    }
    pub fn on_paste(&self, handler: PasteHandler) {
        self.api.host.on_paste(handler);
    }
}

/// `api.session`.
pub struct Session<'a> {
    api: &'a PluginApi,
}

impl Session<'_> {
    pub fn id(&self) -> Option<String> {
        self.api.host.session_id()
    }
    /// Persist plugin state in the session log (FR-S03).
    pub fn append(&self, kind: &str, data: Value) -> anyhow::Result<()> {
        self.api.host.session_append(&self.api.name, kind, data)
    }
    /// Entries this plugin appended under `kind`, oldest first, including restored ones.
    pub fn entries(&self, kind: &str) -> Vec<Value> {
        self.api.host.session_entries(&self.api.name, kind)
    }
    pub fn messages(&self) -> Vec<Message> {
        self.api.host.messages()
    }
    /// Replace the conversation (e.g. after compaction). Persisted.
    pub fn replace_messages(&self, messages: Vec<Message>) -> anyhow::Result<()> {
        self.api.host.replace_messages(messages)
    }
    pub fn inject(&self, content: Vec<UserContent>, when: InjectWhen) {
        self.api.host.inject(content, when);
    }
    pub fn inject_text(&self, text: &str, when: InjectWhen) {
        self.inject(vec![UserContent::text(text)], when);
    }
}

/// `api.agent`.
pub struct Agent<'a> {
    api: &'a PluginApi,
}

impl Agent<'_> {
    pub fn model(&self) -> ModelInfo {
        self.api.host.model()
    }
    pub fn tools(&self) -> Vec<ToolRef> {
        self.api.host.tools()
    }
    /// Run an independent agent loop with its own history (subagents).
    pub async fn run(&self, options: SubagentOptions) -> anyhow::Result<SubagentResult> {
        self.api.host.run_subagent(options).await
    }
    /// One model call without tools; returns the text. Fails when aborted or cut before its end.
    pub async fn complete(&self, options: CompleteOptions) -> anyhow::Result<String> {
        self.api.host.complete(options).await
    }
}

/// A bundled plugin: called once at startup with its own API handle.
pub type PluginFn = fn(PluginApi) -> BoxFuture<'static, anyhow::Result<()>>;
