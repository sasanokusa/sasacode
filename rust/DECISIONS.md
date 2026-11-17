# Decisions for the Rust rewrite

Source of truth for the port. Numbered in order, never renumbered; a changed decision gets a new
ADR that supersedes the old one.

## ADR-001: Rewrite the whole app; TS stays only until the end of the branch
Status: accepted (2026-11-18)
Context: The user asked for all of sasacode in Rust, following the rust-anything skill. The GUI
(`crates/sasacode-gui`) was the first Rust part.
Decision: Everything under `packages/` and the dev scripts become Rust crates in one branch,
`rust/rewrite` (worktree `.claude/worktrees/rust-rewrite`). The TS code stays as the reference and
oracle until the Rust app passes the ported tests, the TS e2e suite run against the Rust binary, the
recorded app cases and real-model checks; then it is deleted in the last commit of the branch. A PR
with green CI ends the work; merge and release wait for the user.
Consequences: No napi bridges or front-door fallback to bun: units are ported straight to Rust and
proved against the TS original by black-box tests.

## ADR-002: No external plugin API; bundled plugins use internal hooks
Status: accepted (2026-11-18)
Context: Nobody uses the TS plugin API or published plugins.
Decision: The TS plugin loader, `@sasacode/plugin-api` (npm), `sasacode plugin search/install/
update/publish` and loading `~/.sasacode/plugins` are removed. The plugin interface lives on as a
Rust-internal API (`sasacode-plugin-api`): bundled plugins register tools, commands, providers and
hooks through it exactly as before, and `plugins.disabled`, `tools.disabled`, `plugins.settings` and
same-name replacement keep working. Same-name replacement means a later registration of a tool or
command replaces an earlier one (a bundled plugin's, or an MCP server's tool with `toolNames: "plain"`);
a plugin on disk taking the place of the bundled plugin of the same name is gone with the loader.
Outside extension is MCP and Skills only. The official plugins
(ask-user, git-context, project-checks, workflows) and jev-guard become bundled, disabled by
default. Of the user's personal plugins only free-search is ported (bundled, disabled by default).
Consequences: Requirements P2/P3 are read as "no privileged path inside the binary". The plugin-api
version number is kept for the record only.

## ADR-003: Keep every format and interface compatible
Status: accepted (2026-11-18)
Decision: `~/.sasacode/config.json` and project `.sasacode/config.json`, `.env`, keychain access,
trust file, session JSONL (old sessions resume), headless `--output jsonl` events and
`--control stdio`, CLI flags and subcommands (except the removed `plugin` ones), install.sh /
install.ps1 and the npm launcher, release asset names. The TUI keeps its layout, keys and
behaviour; output is equivalent, not byte-identical.
Consequences: JSON written by Rust must match `JSON.stringify` where it is read back or compared:
key order (serde_json `preserve_order`), integers without `.0` (timestamps are integers; costs and
other floats use the JS number format, `sasacode_ai::json`), absent optional fields omitted.

## ADR-004: rust-anything for app record/replay; TS tests as the second oracle
Status: accepted (2026-11-18)
Decision: The rust-anything CLI (v1.2.0) records CLI cases from the TS original into
`rust/app-cases/` and replays them against the Rust binary. The TS e2e suite (`packages/e2e`) is
pointed at the Rust binary through `SASACODE_E2E_BIN`. Every TS unit test is ported to Rust;
dropped tests are listed with their reason in `rust/MAP.md`. With the TS removed (the last commit of
the rewrite), the oracles left are the recorded cases, the Rust `cli_e2e` and the protocol fixtures.

## ADR-005: Core budget re-measured after the port
Status: accepted (2026-11-18)
Decision: Core (ai + plugin-api + agent + tools) and bundled stay separate crates; lines stay at
most 120 columns (rustfmt `max_width = 120`). The line ceiling is set again from the Rust core's
measured size, and the checks follow it.

## ADR-006: The GUI stays a child-process client
Status: accepted (2026-11-18)
Decision: `crates/sasacode-gui` is not touched; it keeps running `sasacode` as a child process.
`crates/sasacode-protocol` stays the GUI's lenient reader; the core has its own full-fidelity types
in `sasacode-ai`. Changes to the protocol crate are announced to the GUI session first.

## ADR-007: Version v0.20.0
Status: accepted (2026-11-18)
Decision: The Rust version ships as v0.20.0, through the usual rc flow: commit only, tag and
release after the user approves.

## ADR-008: Real-model checks
Status: accepted (2026-11-18)
Decision: Command Code DeepSeek V4.1 Flash for the M0 task and TUI operation; Gemma4 locally, short
runs only, after checking the power state.

## ADR-009: Dev scripts move to cargo xtask
Status: accepted (2026-11-18)
Decision: build, checks, improve and the pack scripts become `cargo xtask` commands; bun is no
longer needed. The npm launcher (`npm/sasacode`, plain JS run by node) stays. `bench/` is
gitignored local material and is left alone.

## ADR-010: Runtime and libraries
Status: accepted (2026-11-18)
Decision: tokio (multi-thread) as the async runtime; reqwest (rustls) with a hand-written SSE
parser for the model APIs (no SDKs: see `research/llm-sdks.md` for what the SDKs did implicitly);
`async-trait` for object-safe async traits; `tokio-util` `CancellationToken` for AbortSignal;
ratatui + crossterm for the TUI; arguments by a port of Node's `parseArgs` (`sasacode_cli::args`), not
clap, so the flags and their error texts stay Bun's (ADR-003); serde_json with `preserve_order`; anyhow for
errors that only become messages. MCP: see `research/mcp.md`.
Consequences: Hook handlers and tools are `Send + Sync` and run on the tokio runtime; the agent's
state sits behind a mutex that is never held across an await.

## ADR-011: Formerly separate plugins are bundled but off; `plugins.enabled` turns them on
Status: accepted (2026-11-18)
Context: ADR-002 bundles the official plugins (ask-user, git-context, project-checks, workflows),
jev-guard and free-search, disabled by default. Before the rewrite a user turned them on by
installing them; with no installer, config needs a switch.
Decision: `sasacode_bundled::PLUGINS` lists every shipped plugin in load order with an
`on_by_default` flag. A plugin with the flag off loads only when `plugins.enabled` (a string list,
accumulated across config files like `plugins.disabled`) names it; `plugins.disabled` always wins.
In a project config, `plugins.enabled` is an elevated key (needs trust), like `plugins.disabled`.
free-search was a provider registration that started `uvx free-search-mcp`; it is ported as what it
was for: an MCP stdio server `uvx free-search-mcp` registered through `sasacode-mcp`.
Consequences: Old configs stay valid (the TS check ignores unknown keys under `plugins`). Old
installs in `~/.sasacode/plugins` are no longer loaded; the plugin names stay the same, so
`plugins.settings` for them keep working once enabled.
jev-guard's own switch, `plugins.settings["jev-guard"].enabled: true`, also turns it on, so
configs written for the installed plugin keep working.

## ADR-012: Two `unsafe` sites: setsid before exec for bash, and the environment at startup
Status: accepted (2026-11-18)
Context: PORTING forbids `unsafe`. Node's `detached: true` calls `setsid()` in the child, so a bash
command has no controlling terminal: ssh/git credential prompts and gpg fail at once. Rust's
`process_group(0)` keeps the terminal, and those commands would hang until the timeout.
Decision: `sasacode_plugin_api::detach_session` calls `setsid` in `pre_exec`, the only `unsafe` block
in the code base, with a SAFETY note. The bash tool uses it instead of `process_group(0)`.
`std::env::set_var` and `remove_var` are `unsafe` since edition 2024. main.rs needs them to load
`~/.sasacode/.env` and to drop blank key variables, as main.ts did. It calls them in `fn main`
before the tokio runtime or any other thread starts, so nothing reads the environment
concurrently, with a SAFETY note at each call. That is the second and last place.
Consequences: std spawns with fork+exec instead of posix_spawn for bash; the error for a missing
bash is the same `io::Error`. Any other `unsafe` needs its own ADR.
