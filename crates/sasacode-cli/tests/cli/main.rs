//! The CLI's tests from packages/cli/test, one module per TS file, that run the sasacode binary or
//! need a home of their own (`support::isolated`). The rest are lib tests next to their modules.
//!
//! Dropped with the plugin installer and external plugins (rust/DECISIONS.md ADR-002): all of
//! plugin-share.test.ts; from plugins.test.ts every test of discovering, loading, installing,
//! removing and trusting plugins from disk; from commands.test.ts the three `sasacode plugin` and
//! plugin-folder tests.

mod append_system_prompt;
mod commands;
mod context_refresh;
mod control;
mod fork_redact;
mod gui;
mod headless;
mod plugins;
mod setup_models;
mod support;
mod tui;

/// The lib's test server (the TS tests' `Bun.serve`), shared with the integration tests.
#[path = "../../src/test_server.rs"]
#[allow(dead_code)]
mod test_server;
