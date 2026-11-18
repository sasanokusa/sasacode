# Glossary: TypeScript names → Rust names

The TS terms the docs, issues and session logs use, and where each lives in Rust. Behavior is unchanged
unless an ADR in [DECISIONS.md](DECISIONS.md) says otherwise.

## Packages and crates

| TS | Rust |
| --- | --- |
| `packages/ai` | `sasacode-ai` (core) |
| `packages/agent` | `sasacode-agent` (core) |
| `packages/plugin-api` | `sasacode-plugin-api` (core; internal since ADR-002, no longer published) |
| `packages/tools` | `sasacode-tools` (core) |
| `packages/host` | `sasacode-host` (i18n, models table, notes, session listing) |
| `packages/mcp`, `packages/skills` | `sasacode-mcp`, `sasacode-skills` |
| `packages/bundled`, `plugins/*` | `sasacode-bundled` (ADR-011: the formerly separate ones are bundled but off) |
| `packages/cli` | `sasacode-cli` (lib + the `sasacode` binary) |
| `packages/tui`, pi-tui | `sasacode-tui` (ratatui + crossterm) |
| `packages/e2e` test helpers | `sasacode-testing` |
| `scripts/*.ts` | `xtask` (`cargo xtask build|checks|improve`, ADR-009) |

## Runtime and language

| TS | Rust |
| --- | --- |
| `AbortSignal`, `AbortController` | `tokio_util::sync::CancellationToken` |
| `Promise`, `async` function | `BoxFuture<'static, _>` / `async fn` on tokio (ADR-010) |
| `JSON.stringify(v)` | `sasacode_ai::json::stringify` (JS number format, key order kept) |
| `JSON.stringify(v, null, 2)` | `sasacode_ai::json::stringify_pretty` |
| `string.length`, `slice` on UTF-16 | `encode_utf16()` where the TS counted units (helpers: `sasacode_bundled::repair::js::utf16_*`, `sasacode_tui::text::utf16_len`); never byte lengths |
| `util.parseArgs` | `sasacode_cli::args::parse_args` (Bun's error texts) |
| `Number(v)` | `js_number` in `crates/sasacode-cli/src/main.rs` |
| `Bun.Glob` | `globset` behind `permission::glob` with Bun's syntax (`bun_glob_syntax`), checked against `bun_glob_cases.json` |
| `spawn(…, { detached: true })` | `sasacode_plugin_api::detach_session` (setsid, ADR-012) |
| `childEnv()` | `sasacode_plugin_api::{child_env, apply_child_env}`; `.env` keys hidden by `hide_from_children` |
| `fs` error text (`ENOENT: no such file or directory, open 'x'`) | `sasacode_tools::util::fs_error` |
| `t("…", { x })` | `sasacode_host::t!("…", x = …)`; tables in `messages_ja.rs` / `messages_en.rs` |

## Agent and plugins

| TS | Rust |
| --- | --- |
| `Plugin` (a function of `api`) | `PluginFn` / `PluginInit` (`FnOnce(PluginApi) -> BoxFuture`) |
| `api.on("turn_end", f)` | `agent.hooks().on::<hook::TurnEnd, _, _>(owner, f)`; `emit::<hook::X>(event)` |
| `agent.on("event", f)` | `agent.events().on(f)` → `Subscription` (`.off()`) |
| `PluginHost.load(name, plugin)` | `PluginHost::load(name, plugin)` (catches sync and async panics) |
| `bundledPlugins` | `sasacode_bundled::PLUGINS` (`Bundled { name, plugin, on_by_default }`), `wanted()` |
| `ui` (TUI bridge) | `UiBridge` trait |
| `setup()` and its return | `sasacode_cli::setup::{setup, Harness}` |
| `runHeadless` | `sasacode_cli::headless::run_headless` |
| `startControl` | `sasacode_cli::control::start_control` |
| `assessProject`, `isTrusted`, `askTrust` | `sasacode_cli::trust::*` (same fingerprint for projects without plugins) |
| `ModelCatalog` | `sasacode_cli::catalog::ModelCatalog` |
