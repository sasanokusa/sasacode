//! The interactive session: hands the harness to `sasacode-tui` (`runTui(harness, …)` in main.ts,
//! where the harness itself was the TUI's host).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use sasacode_agent::agent::Agent;
use sasacode_agent::plugins::PluginHost;
use sasacode_ai::{Message, ModelInfo};
use sasacode_plugin_api::{Lang, UiBridge};
use sasacode_tui::term::Terminal;
use sasacode_tui::{ListedModel, TuiConfig, TuiHost};

use crate::catalog::ModelChoice;
use crate::setup::Harness;

/// Runs the TUI on the process's terminal until the user leaves; the exit code.
pub async fn run(harness: Arc<Harness>, initial_prompt: String) -> anyhow::Result<i32> {
    notify_updates(&harness);
    sasacode_tui::run_tui(Arc::new(HarnessHost(harness)), initial_prompt).await
}

/// [`run`] on another terminal (the tests' stand-in).
pub async fn run_with(
    harness: Arc<Harness>,
    initial_prompt: String,
    terminal: Box<dyn Terminal>,
) -> anyhow::Result<i32> {
    notify_updates(&harness);
    sasacode_tui::run_tui_with(Arc::new(HarnessHost(harness)), initial_prompt, terminal).await
}

/// A quiet heads-up when a newer release is out; `"updateCheck": false` in config turns it off. An
/// info notice waits for the UI, so it shows in the transcript once the plugins load.
fn notify_updates(harness: &Harness) {
    if harness.settings.update_check == Some(false) {
        return;
    }
    let host = harness.host.clone();
    tokio::spawn(crate::update::notify_if_newer(Arc::new(move |message, level| {
        host.api("sasacode").ui().notify(&message, level);
    })));
}

/// A provider's listing as the TUI's /model shows it.
pub fn listed_model(m: ModelChoice) -> ListedModel {
    ListedModel {
        spec: m.spec,
        context_window: m.context_window,
        max_context: m.max_context,
        server: m.server,
    }
}

/// The harness as the TUI's host: every method is the harness's own (setup.ts's `Harness` was the
/// `TuiHost`).
pub struct HarnessHost(pub Arc<Harness>);

#[async_trait]
impl TuiHost for HarnessHost {
    fn agent(&self) -> Agent {
        self.0.agent.clone()
    }

    fn host(&self) -> Arc<PluginHost> {
        self.0.host.clone()
    }

    fn warnings(&self) -> Vec<String> {
        self.0.warnings()
    }

    /// `config.models`, `config.tui.altScreen !== false`, `config.tui.bell !== false`.
    fn config(&self) -> TuiConfig {
        let s = &self.0.settings;
        let tui = s.tui.clone().unwrap_or_default();
        TuiConfig {
            models: s.models.clone().unwrap_or_default(),
            alt_screen: tui.alt_screen != Some(false),
            bell: tui.bell != Some(false),
        }
    }

    fn resolve(&self, spec: &str) -> anyhow::Result<ModelInfo> {
        self.0.resolve(spec)
    }

    fn sessions_dir(&self) -> PathBuf {
        self.0.sessions_dir.clone()
    }

    fn history_path(&self) -> Option<PathBuf> {
        Some(self.0.history_path.clone())
    }

    async fn list_models(&self, refresh: bool) -> anyhow::Result<Vec<ListedModel>> {
        Ok(self
            .0
            .list_models(refresh)
            .await
            .into_iter()
            .map(listed_model)
            .collect())
    }

    async fn load_plugins(&self, ui: Arc<dyn UiBridge>) -> anyhow::Result<()> {
        self.0.load_plugins(Some(ui)).await;
        Ok(())
    }

    async fn load_session(&self, path: &Path) -> anyhow::Result<()> {
        self.0.load_session(path).await
    }

    async fn new_session(&self) -> anyhow::Result<()> {
        self.0.new_session().await
    }

    async fn fork(&self, index: usize) -> anyhow::Result<()> {
        self.0.fork(index).await
    }

    async fn shutdown(&self) {
        self.0.shutdown().await;
    }

    fn save_lang(&self, lang: Lang) -> anyhow::Result<()> {
        self.0.save_lang(lang)
    }

    fn transcript(&self) -> Option<Vec<Message>> {
        Some(self.0.transcript())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listing_keeps_its_sizes_and_server() {
        let m = listed_model(ModelChoice {
            spec: "ollama/qwen".into(),
            context_window: None,
            max_context: Some(262_144),
            server: Some("ollama".into()),
        });
        assert_eq!(
            m,
            ListedModel {
                spec: "ollama/qwen".into(),
                context_window: None,
                max_context: Some(262_144),
                server: Some("ollama".into()),
            }
        );
    }
}
