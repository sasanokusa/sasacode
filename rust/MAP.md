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
| 1 | ai adapters: sse, anthropic, openai-chat, openai-responses, effort, format, replay | claimed by subagent A 2026-11-18 |
| 1 | plugin-api validate + settings; tools: read, write, edit, bash, shell, util | claimed by subagent B 2026-11-18 |
| 1 | mcp; skills | claimed by subagent C 2026-11-18 |
| 1 | agent/session → `sasacode-agent::session`; host: i18n, messages, models, notes, sessions | claimed by subagent D 2026-11-18 |
| 1 | testing: replay provider, fake model server; e2e harness `SASACODE_E2E_BIN` | claimed by subagent E 2026-11-18 |
| 1 | agent: agent, events, tool-exec, permission, plugins host, tool-search, system-prompt | written by coordinator; permission tests 20/20 (16 TS + 4 new); agent/pipeline/plugin tests wait for tools |
| 2 | headless vertical slice: cli main/config/headless/control + builtin tools, e2e green | open |
| 3 | bundled plugins (incl. official + jev-guard + free-search) | open |
| 3 | tui | claimed by subagent T 2026-11-18 |
| 3 | cli: setup, loader, catalog, endpoints, detect, env, keys, list-models, trust, update, stdin | open |
| 4 | xtask (build, checks, improve, pack), CI and release workflows, docs, TS removal | open |

## Tests

TS: 619 `test(` calls under packages/ and plugins/. Each unit lists ported and dropped tests in
its report; dropped ones are collected here with the reason.

Dropped:
- (none yet)

## Open checks (block the TS removal)

- Glob equivalence: a pattern × path table from `Bun.Glob.match` against `permission::glob()` (classes,
  `[!…]`, braces, `**` positions, `secrets/**` vs `secrets`, dotfiles, `!(`/`?(`, an unclosed `[`).
  The permission tests pass, but they do not prove globset matches Bun.Glob, and this is a security boundary.
- Windows paths: `paths.rs` tests are unix-only. `resolve(&["/work/x"])` on Windows must get a drive
  (Node does); `path_pattern` assumes it. Needs a Windows CI job before release.

## Out of scope

- External plugin loading, `sasacode plugin *`, `@sasacode/plugin-api` on npm (ADR-002).
- `bench/` (gitignored, local).
