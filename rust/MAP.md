# Rust rewrite map

Goal: sasacode as a pure Rust app (v0.20.0), every format compatible, TS removed at the end of the
`rust/rewrite` branch. Read [HANDOFF.md](HANDOFF.md) first when resuming.

- Decisions: [DECISIONS.md](DECISIONS.md) — ADR-001 whole app, ADR-002 no external plugins,
  ADR-003 full compatibility, ADR-004 oracles, ADR-005 core budget, ADR-006 GUI untouched,
  ADR-007 v0.20.0, ADR-008 real models, ADR-009 xtask, ADR-010 runtime and libraries.
- Rules for every unit: [PORTING.md](PORTING.md). Names: [GLOSSARY.md](GLOSSARY.md).
- Research: [research/](research/) (llm-sdks, mcp, tui).

## Units

State: `open`, `claimed by <who> <date>`, `done (<evidence>)`.

| Wave | Unit (TS → Rust) | State |
| --- | --- | --- |
| 0 | contract: ai/types, plugin-api → `sasacode-ai` types/provider/json/http, `sasacode-plugin-api` | done (builds, clippy clean) |
| 1 | ai adapters: sse, anthropic, openai-chat, openai-responses, effort, format, replay | open |
| 1 | plugin-api validate + settings; tools: read, write, edit, bash, shell, util | open |
| 1 | mcp; skills | open |
| 1 | agent/session → `sasacode-agent::session`; host: i18n, messages, models, notes, sessions | open |
| 1 | testing: replay provider, fake model server; e2e harness `SASACODE_E2E_BIN` | open |
| 1 | agent: agent, events, tool-exec, permission, plugins host, tool-search, system-prompt | claimed by coordinator 2026-11-18 |
| 2 | headless vertical slice: cli main/config/headless/control + builtin tools, e2e green | open |
| 3 | bundled plugins (incl. official + jev-guard + free-search) | open |
| 3 | tui | open |
| 3 | cli: setup, loader, catalog, endpoints, detect, env, keys, list-models, trust, update, stdin | open |
| 4 | xtask (build, checks, improve, pack), CI and release workflows, docs, TS removal | open |

## Tests

TS: 619 `test(` calls under packages/ and plugins/. Each unit lists ported and dropped tests in
its report; dropped ones are collected here with the reason.

Dropped:
- (none yet)

## Out of scope

- External plugin loading, `sasacode plugin *`, `@sasacode/plugin-api` on npm (ADR-002).
- `bench/` (gitignored, local).
