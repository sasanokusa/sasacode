# Rust rewrite map

Goal: sasacode as a pure Rust app (v0.20.0), every format compatible, TS removed at the end of the
`rust/rewrite` branch. Read [HANDOFF.md](HANDOFF.md) first when resuming.

- Decisions: [DECISIONS.md](DECISIONS.md) — ADR-001 whole app, ADR-002 no external plugins,
  ADR-003 full compatibility, ADR-004 oracles, ADR-005 core budget, ADR-006 GUI untouched,
  ADR-007 v0.20.0, ADR-008 real models, ADR-009 xtask, ADR-010 runtime and libraries, ADR-011 bundled-but-off plugins.
- Rules for every unit: [PORTING.md](PORTING.md). Names: [GLOSSARY.md](GLOSSARY.md).
- Research: [research/](research/) (llm-sdks, mcp, tui).

## Units

State: `open`, `claimed by <who> <date>`, `done (<evidence>)`.

| Wave | Unit (TS → Rust) | State |
| --- | --- | --- |
| 0 | contract: ai/types, plugin-api → `sasacode-ai` types/provider/json/http, `sasacode-plugin-api` | done (builds, clippy clean) |
| 1 | ai adapters: sse, anthropic, openai-chat, openai-responses, effort, format, replay | done (2bb1cc6) |
| 1 | plugin-api validate + settings; tools: read, write, edit, bash, shell, util | done (c3da12a) |
| 1 | mcp; skills | done (13+4 TS tests ported; 8722613) |
| 1 | agent/session → `sasacode-agent::session`; host: i18n, messages, models, notes, sessions | done (8,932 real log lines round-trip; cbe65f5) |
| 1 | testing: replay provider, fake model server; e2e harness `SASACODE_E2E_BIN` | done (11 unit + 17 e2e vs TS CLI; 5271490) |
| 1 | agent: agent, events, tool-exec, permission, plugins host, tool-search, system-prompt | done; permission globs = Bun.Glob on 2,303 recorded pairs (79b82b6); all TS tests ported (ecdb8eb) |
| 2 | headless vertical slice: cli main/config/headless/control + builtin tools, e2e green | done: e2e 17/17 Rust, 17/18 TS (the 18th is ADR-002), app cases 34/34 (ae4d9bc) |
| 3 | bundled table and ADR-011 switch (`sasacode-bundled/src/lib.rs`) | done by coordinator |
| 3 | bundled: web-fetch, web-search, rustxng, browsr, openai-codex, usage, background-sessions, auto-reconnect, goal, gui, image-attach, image-slim, free-search | claimed by subagent N 2026-11-18 |
| 3 | bundled: tool-repair, repair, text-tool-calls, guards, agents-md, compaction, diagnostics, subagent, todo, progress-notes, permission-presets, agent-judge, checkpoints, official plugins, jev-guard | claimed by subagent K 2026-11-18 |
| 3 | tui | claimed by subagent T 2026-11-18 |
| 3 | cli: setup, catalog, endpoints, detect, env, keys, list-models, trust, update, stdin (loader: ADR-002) | done (1ed76bb, ae4d9bc); TUI entry waits for T |
| 4 | xtask (build, checks, improve), CI and release workflows, install scripts | claimed by subagent X 2026-11-18 |
| 4 | docs (READMEs, docs/*.md) | claimed by subagent D 2026-11-18 |
| 4 | app record/replay (`rust/app-cases`, rust-anything) | done: 34 cases from TS 0.15.2 (61cd088) |
| 4 | TS removal; GUI and protocol tests read TS sources and move to the Rust ones | open (coordinator, last) |

## Tests

TS: 619 `test(` calls under packages/ and plugins/. Each unit lists ported and dropped tests in
its report; dropped ones are collected here with the reason.

Dropped:
- e2e `cli.e2e`: "a plugin installed from a local folder loads…" and all of `plugin-install.e2e` (4): ADR-002.
- skills `authoring-doc` (3): they run the SKILL.md's TS plugin code samples with Bun; ADR-002. The SKILL.md
  is rewritten for MCP and Skills in wave 4.
- plugin-api v19/v110: the asserts that check runtime argument types (Rust checks them at compile time).

Still to port (owner: coordinator):
- ~~agent tests~~: done (ecdb8eb). Three symlink tests are `#[cfg(unix)]`; the TS ran one on Windows with a junction.
- cli tests (control, context-refresh, commands, cli headless parts): claimed by subagent C 2026-11-18.
- npm-launcher on Windows: the TS used a `bun build --compile` stand-in; needs a Windows CI job.

## Open checks (block the TS removal)

- ~~Glob equivalence~~: done. `bun_glob_cases.json` holds Bun 1.4.4's answers for 2,303 pairs; all agree
  except Bun's `**/*` on paths ending in `/`, which a rule never sees (79b82b6).
- Windows paths: `paths.rs` tests are unix-only. `resolve(&["/work/x"])` on Windows must get a drive
  (Node does); `path_pattern` assumes it. Needs a Windows CI job before release.

## Out of scope

- External plugin loading, `sasacode plugin *`, `@sasacode/plugin-api` on npm (ADR-002).
- `bench/` (gitignored, local).
