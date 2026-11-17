//! The interactive session: hands the harness to `sasacode-tui`.

use std::sync::Arc;

use crate::setup::Harness;

/// Runs the TUI until the user leaves; the exit code.
pub async fn run(harness: Arc<Harness>, initial_prompt: String) -> anyhow::Result<i32> {
    // Wired to sasacode_tui::run_tui once its host trait lands.
    let _ = (harness, initial_prompt);
    anyhow::bail!("the interactive TUI is not wired yet")
}
