//! A plugin host for tests, standing in for sasacode-agent's PluginHost (not ported yet). It keeps
//! the PluginHost semantics the tests rely on: a command of the same name is removed and pushed at
//! the end, tools are replaced by name and only their owner takes them away, status keys are
//! "<plugin>:<key>", ready work is spawned at once, and the UI language is Japanese.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::future::BoxFuture;
use serde_json::{Value, json};

use sasacode_plugin_api::hooks::{SessionEndEvent, SystemPromptEvent};
use sasacode_plugin_api::*;

#[derive(Default)]
pub struct Recorded {
    pub tools: Vec<(String, ToolRef)>,
    pub commands: Vec<(String, Command)>,
    pub notes: Vec<(String, NotifyLevel)>,
    pub status: HashMap<String, String>,
    pub rules: Vec<PermissionRules>,
    pub injected: Vec<(String, InjectWhen)>,
    pub ready: Vec<tokio::task::JoinHandle<()>>,
}

pub struct FakeHost {
    pub cwd: PathBuf,
    pub settings: Value,
    pub hooks: HookRunner,
    pub lang: Lang,
    pub rec: Mutex<Recorded>,
}

impl FakeHost {
    pub fn new(cwd: impl Into<PathBuf>, settings: Value) -> Arc<FakeHost> {
        Arc::new(FakeHost {
            cwd: cwd.into(),
            settings,
            hooks: HookRunner::default(),
            lang: Lang::Ja,
            rec: Mutex::new(Recorded::default()),
        })
    }

    pub fn api(self: &Arc<Self>, name: &str) -> PluginApi {
        PluginApi::new(name, self.clone())
    }

    fn rec(&self) -> std::sync::MutexGuard<'_, Recorded> {
        self.rec.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.rec().tools.iter().map(|(_, t)| t.name().to_string()).collect()
    }
    pub fn tool(&self, name: &str) -> Option<ToolRef> {
        self.rec()
            .tools
            .iter()
            .find(|(_, t)| t.name() == name)
            .map(|(_, t)| t.clone())
    }
    pub fn command_names(&self) -> Vec<String> {
        self.rec().commands.iter().map(|(_, c)| c.name.clone()).collect()
    }
    pub fn command(&self, name: &str) -> Option<Command> {
        self.rec()
            .commands
            .iter()
            .find(|(_, c)| c.name == name)
            .map(|(_, c)| c.clone())
    }
    pub fn notes(&self) -> Vec<String> {
        self.rec().notes.iter().map(|(n, _)| n.clone()).collect()
    }
    pub fn status(&self, key: &str) -> Option<String> {
        self.rec().status.get(key).cloned()
    }
    pub fn rules(&self) -> Vec<PermissionRules> {
        self.rec().rules.clone()
    }
    pub fn injected(&self) -> Vec<(String, InjectWhen)> {
        self.rec().injected.clone()
    }

    /// `agent.hooks.run("system_prompt", { prompt })`, folding each result in.
    pub async fn system_prompt(&self, prompt: &str) -> String {
        let ev = self
            .hooks
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
            .await;
        ev.prompt
    }

    /// `agent.hooks.run("session_end", { reason })`.
    pub async fn session_end(&self, reason: Option<SessionEndReason>) {
        self.hooks
            .emit::<hook::SessionEnd>(SessionEndEvent {
                session_id: None,
                reason,
            })
            .await;
    }

    /// Wait for the ready work, like headless does before its first request.
    pub async fn settle(&self) {
        let handles: Vec<_> = std::mem::take(&mut self.rec().ready);
        for h in handles {
            let _ = h.await;
        }
    }

    /// Load a plugin: run its setup with its own API handle.
    pub async fn load<F, Fut>(self: &Arc<Self>, name: &str, plugin: F) -> anyhow::Result<()>
    where
        F: FnOnce(PluginApi) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>>,
    {
        plugin(self.api(name)).await
    }
}

/// Wait until `cond` holds, 50 ms at a time, at most 100 times (the TS tests' `until`).
pub async fn until(mut cond: impl FnMut() -> bool) {
    for _ in 0..100 {
        if cond() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[async_trait]
impl HostServices for FakeHost {
    fn cwd(&self) -> PathBuf {
        self.cwd.clone()
    }
    fn settings(&self, plugin: &str) -> Value {
        self.settings.get(plugin).cloned().unwrap_or_else(|| json!({}))
    }
    fn hooks(&self) -> HookRunner {
        self.hooks.clone()
    }
    fn register_tool(&self, plugin: &str, tool: ToolRef) {
        let mut r = self.rec();
        if let Some(slot) = r.tools.iter_mut().find(|(_, t)| t.name() == tool.name()) {
            *slot = (plugin.to_string(), tool);
        } else {
            r.tools.push((plugin.to_string(), tool));
        }
    }
    fn unregister_tool(&self, plugin: &str, name: &str) {
        self.rec()
            .tools
            .retain(|(owner, t)| !(t.name() == name && owner == plugin));
    }
    fn register_command(&self, plugin: &str, command: Command) {
        let mut r = self.rec();
        r.commands.retain(|(_, c)| c.name != command.name);
        r.commands.push((plugin.to_string(), command));
    }
    fn register_provider(
        &self,
        _name: &str,
        _config: ProviderConfig,
        _implementation: Option<Arc<dyn Provider>>,
    ) -> anyhow::Result<()> {
        anyhow::bail!("registerProvider is not available")
    }
    fn ready(&self, work: BoxFuture<'static, ()>) {
        let h = tokio::spawn(work);
        self.rec().ready.push(h);
    }
    fn add_permission_rules(&self, rules: PermissionRules) -> anyhow::Result<()> {
        self.rec().rules.push(rules);
        Ok(())
    }
    fn permission_mode(&self) -> PermissionMode {
        PermissionMode::Auto
    }
    fn interactive(&self) -> bool {
        false
    }
    fn lang(&self) -> Lang {
        self.lang
    }
    fn notify(&self, message: &str, level: NotifyLevel) {
        self.rec().notes.push((message.to_string(), level));
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
    fn set_status(&self, plugin: &str, key: &str, text: Option<String>) {
        let k = format!("{plugin}:{key}");
        let mut r = self.rec();
        match text {
            Some(t) => r.status.insert(k, t),
            None => r.status.remove(&k),
        };
    }
    fn register_tool_renderer(&self, _tool: &str, _renderer: ToolRenderer) {}
    fn on_paste(&self, _handler: PasteHandler) {}
    fn session_id(&self) -> Option<String> {
        None
    }
    fn session_append(&self, _plugin: &str, _kind: &str, _data: Value) -> anyhow::Result<()> {
        Ok(())
    }
    fn session_entries(&self, _plugin: &str, _kind: &str) -> Vec<Value> {
        Vec::new()
    }
    fn messages(&self) -> Vec<Message> {
        Vec::new()
    }
    fn replace_messages(&self, _messages: Vec<Message>) -> anyhow::Result<()> {
        Ok(())
    }
    fn inject(&self, content: Vec<UserContent>, when: InjectWhen) {
        let text: String = content.iter().filter_map(UserContent::as_text).collect();
        self.rec().injected.push((text, when));
    }
    fn model(&self) -> ModelInfo {
        ModelInfo {
            id: "m".into(),
            provider: "t".into(),
            api: "replay".into(),
            base_url: None,
            context_window: 100_000,
            max_output: 1000,
            price: None,
            reasoning: None,
            images: None,
            headers: None,
            server: None,
        }
    }
    fn tools(&self) -> Vec<ToolRef> {
        self.rec().tools.iter().map(|(_, t)| t.clone()).collect()
    }
    async fn run_subagent(&self, _options: SubagentOptions) -> anyhow::Result<SubagentResult> {
        anyhow::bail!("no subagents in tests")
    }
    async fn complete(&self, _options: CompleteOptions) -> anyhow::Result<String> {
        anyhow::bail!("no model in tests")
    }
}
