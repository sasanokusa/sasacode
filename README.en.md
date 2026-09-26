# sasacode

English · [日本語](README.md) · [Documentation (Japanese)](https://sasanokusa.com/sasacode/docs/)

A small coding agent for the terminal that you reshape with plugins (TypeScript / Bun).

The core holds only the agent loop, the providers, four built-in tools (read / write / edit / bash), the TUI, sessions and permission checks. Everything else is added through the public plugin API, and the built-in tools are registered through that same API. MCP servers and Agent Skills (`SKILL.md`) work as they are. To make small models usable, it also ships repair of broken tool calls and a stop for repeated sentences.

The requirements are in [docs/requirements.md](docs/requirements.md) and the milestone log in [docs/milestones.md](docs/milestones.md) (both in Japanese). The detailed guide with diagrams and full setting tables is at [sasanokusa.com/sasacode/docs](https://sasanokusa.com/sasacode/docs/) (Japanese).

## Quick start

```bash
curl -fsSL https://sasanokusa.com/sasacode/install.sh | sh   # installs ~/.local/bin/sasacode
sasacode --version                                          # the first run creates ~/.sasacode/.env
$EDITOR ~/.sasacode/.env                                    # fill in the key of the provider you use
sasacode
```

`~/.sasacode/.env` lists the keys of every supported provider, blank. Fill in only the ones you use; blank entries count as unset. Without `-m`, sasacode uses the default model of the first provider that has a key.

## Install

```bash
curl -fsSL https://sasanokusa.com/sasacode/install.sh | sh
```

- Single binaries for macOS (arm64 / x64) and Linux (x64 / arm64, glibc / musl, and a baseline build for CPUs without AVX2). Bun is not needed.
- The installer and the binary come from the latest [GitHub release](https://github.com/sasanokusa/sasacode/releases) and are checked against SHA256. The sasanokusa.com URL redirects to the latest release's `install.sh`.
- `SASACODE_VERSION=v0.9.4` pins a version and `SASACODE_INSTALL_DIR` changes where it goes. On Windows, use WSL.

From source ([Bun](https://bun.sh) 1.4 or later):

```bash
git clone https://github.com/sasanokusa/sasacode && cd sasacode
bun install
ln -s "$PWD/packages/cli/src/main.ts" ~/.local/bin/sasacode
```

## Usage

```bash
sasacode                            # interactive (TUI)
sasacode -p "fix the tests"          # headless: the answer on stdout, progress on stderr
sasacode -p "…" --output jsonl       # every event as JSONL
echo "$LOG" | sasacode -p "why?"     # piped input is appended to the -p prompt
sasacode -c                         # resume the latest session in this directory
sasacode -r <id> -p "go on"          # continue a session by id (a conversation without tmux)
```

| Option | Meaning |
| --- | --- |
| `-m <provider/model>` | Model (e.g. `anthropic/claude-opus-5`, `openai/gpt-5.5`, `ollama/gemma4:e4b`) |
| `--permission <edits\|ask\|agent\|auto>` | Permission mode (default `edits`) |
| `--thinking <off\|low\|medium\|high\|xhigh\|max>` | Reasoning effort |
| `--max-turns <n>` | Limit on requests per run (default 200, 0 = no limit) |
| `-c` / `-r <id>` / `--no-session` | Resume / resume by id / do not save |
| `--trust-project` | Trust the project without asking (for headless runs; see [Project trust](#project-trust)) |
| `sasacode plugin search\|install\|update\|remove\|list\|publish` | Find, install (npm or git URL), update and publish plugins ([Sharing](#sharing-plugins)) |
| `sasacode endpoint add\|remove\|list` | Add your own server (the format is detected) |
| `sasacode login [status\|logout]` | Sign in with a ChatGPT plan (for `openai-codex/…` models) |

### TUI

It runs full screen: the conversation scrolls at the top, the input and the footer stay at the bottom. On exit the terminal returns to how it was, and only a summary is left (tokens, requests, cost, models, and the command to resume). To keep the conversation in the terminal as before, set `"tui": { "altScreen": false }`.

| Key | Action |
| --- | --- |
| enter / shift+enter, alt+enter | Send / new line |
| ↑↓ | Input history |
| tab | Completion (slash commands, model names in `/model`, file paths) |
| esc | Interrupt generation or a tool (the model is told on the next input) |
| enter while running | Queue a message for the next turn |
| ctrl+o | Expand or collapse tool output and thinking |
| shift+tab | Cycle the permission mode |
| pageup / pagedown, wheel | Scroll (while scrolled up, a scrollbar and "jump to latest" appear) |
| ctrl+↑ / ctrl+↓ | Previous / next of your own messages |
| ctrl+home / ctrl+end | Top / latest |
| ctrl+shift+f | Search the conversation (enter: next, shift+enter: previous, esc: close) |
| drag | Select and copy to the clipboard (hold option / alt for the terminal's own selection) |
| `/exit`, ctrl+c twice, ctrl+d | Quit (shows usage and `sasacode -r <id>`) |

At start it asks the terminal how many cells it draws East Asian Ambiguous characters (such as ①) in, and lays out text to match. Without an answer it assumes one. `SASACODE_AMBIGUOUS_WIDTH=1` or `2` sets it explicitly.

| Command | Meaning |
| --- | --- |
| `/model` | Pick from the models your providers list; type to filter. `/model <provider/model>` sets one directly, `/model --refresh` fetches the lists again |
| `/resume` `/clear` `/fork` `/session` | Earlier sessions / a new session / branch from an earlier message / id and file |
| `/effort` | Reasoning effort (`off` / `low` / `medium` / `high` / `xhigh` / `max`); without an argument, pick from a list. Shown in the footer. OpenAI-style servers receive it as `reasoning_effort`, and `off` is sent as `none` (servers such as vLLM think unless told so). A value a server refuses is replaced with the closest one it takes and remembered (Command Code refuses `none`, so `off` becomes `low` there) |
| `/copy` | Copy the last answer (pbcopy / wl-copy / xclip, else through the terminal) |
| `/permission` `/help` `/exit` (`/quit`) | Permission mode / help / quit |
| `/usage` `/goal` `/bg` `/reconnect` `/compact` `/mcp` `/skills` `/skill:<name>` `/presets` `/browsr` | Commands of bundled plugins |

The footer shows the model, the reasoning effort, the permission mode, context use (`ctx 77.6k/256k (30%)`), the session id and plugin statuses (MCP connections, TODO progress, …). When a run that took 30 seconds or more finishes, the terminal bell rings (`"tui": { "bell": false }` turns it off).

## Providers and models

Models are named `<provider>/<model>`.

| provider | API | Key |
| --- | --- | --- |
| `anthropic` | Anthropic Messages | `ANTHROPIC_API_KEY` (empty: the SDK uses the `ant auth login` profile) |
| `openai` / `openai-chat` | OpenAI Responses / Chat Completions | `OPENAI_API_KEY` |
| `openrouter` | Chat Completions | `OPENROUTER_API_KEY` |
| `commandcode` / `commandcode-responses` / `commandcode-anthropic` | Command Code's Chat Completions / Responses / Messages | `CMD_API_KEY` |
| `ollama` | Chat Completions (localhost:11434) | none |
| `openai-codex` | The Codex backend of a ChatGPT plan (Responses) | none (`sasacode login` with a ChatGPT account) |

**With a ChatGPT plan**: `sasacode login` opens a browser; after signing in with your ChatGPT account, the `openai-codex/…` models (listed in `/model`) work without an API key and use your plan's Codex allowance. The credentials are stored in `~/.sasacode/codex-auth.json` (readable only by you) and refreshed before they expire. When the browser cannot reach this machine (over SSH, for example), paste the URL the browser shows after signing in. `sasacode login status` shows the state, `sasacode login logout` removes it, and `/usage plan` shows what the plan has left (the 5-hour and weekly windows). With no API key but a login, `openai-codex/gpt-5.5` is the default model. This uses the same mechanism as OpenAI's Codex CLI (a public client with PKCE) and is unofficial: OpenAI may stop it working.

### Your own server (llama.cpp, vLLM, LM Studio, proxies, …)

```bash
sasacode endpoint add llama http://192.168.1.20:8080            # detects the format, saves to ~/.sasacode/config.json
sasacode endpoint add mygw https://llm.example.com/v1 --key-env MYGW_KEY
sasacode endpoint list
sasacode -m llama/<model>
```

From `GET /v1/models` it tells OpenAI-style servers (called with Chat Completions) from Anthropic-style ones (called with Messages) and adjusts the URL to what each SDK expects. For llama.cpp and Ollama it also reads the real context length (from `/props` and `/api/show`). Models of an added server appear in `/model`. A config entry with only a URL, `"providers": { "llama": { "baseUrl": "http://…" } }`, is detected at start too (the result is cached in `~/.sasacode/endpoints.json`).

Endpoints in a project's `.sasacode/config.json` are not used until you trust the project ([Project trust](#project-trust)): even a server without a key receives the conversation and the code.

**Model lists**: when the TUI starts, it calls the Models API (`GET /v1/models`) of every provider with a key (and Ollama) in the background, and shows the models and their context lengths in `/model`. The context length is also used for the footer and the compaction threshold (`modelOverrides` in the config wins). Ollama is read through `/api/show`: the Modelfile's `num_ctx` is used as the real length when set, otherwise the model's maximum is shown for reference. OpenAI's Models API gives no context length, so the built-in model table fills it in. Services that offer several API formats on one key, such as Command Code, have each model routed to the provider of the matching format (Claude models go to `commandcode-anthropic/…`, for example).

**API keys** are looked up in this order: shell environment → `~/.sasacode/.env` → OS keychain (macOS: `security add-generic-password -s sasacode -a <provider> -w`, Linux: `secret-tool store --label sasacode service sasacode account <provider>`). Blank values are ignored everywhere. The working directory's `.env` and `bunfig.toml` are never read (so a repository cannot redirect keys with `ANTHROPIC_BASE_URL` or run code at start). Keys in use are replaced with `[REDACTED]` before a session log is written.

## Configuration

`~/.sasacode/config.json` (global) and `<project>/.sasacode/config.json` (project) are merged, the project winning, except that the parts of a project config that need trust are ignored until the project is trusted (below). Permission rules and the `disabled` lists (tools, plugins) are combined from both, so a project can never remove a global deny / ask rule or re-enable something switched off globally. Values of the wrong type and unknown keys are dropped with a warning.

```json
{
  "model": "commandcode/deepseek/deepseek-v4-flash",
  "models": ["commandcode/deepseek/deepseek-v4-pro", "ollama/gemma4:e4b"],
  "thinking": "high",
  "providers": { "myproxy": { "api": "openai-chat", "baseUrl": "https://…/v1", "apiKeyEnv": "MY_KEY" } },
  "modelOverrides": { "myproxy/some-model": { "contextWindow": 200000 } },
  "permissions": {
    "mode": "edits",
    "allow": ["bash(git status*)", "bash(bun test*)"],
    "ask": ["bash(git push*)"],
    "deny": ["bash(rm -rf *)", "edit(.env)"]
  },
  "instructions": "Text appended to the system prompt",
  "mcpServers": { "github": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"] } },
  "plugins": { "disabled": ["web-fetch"], "settings": { "permission-presets": { "presets": ["guard", "tests"] } } },
  "tools": { "disabled": [] },
  "toolSearch": { "mode": "auto", "percent": 10, "count": 30 },
  "tui": { "altScreen": true, "bell": true },
  "trustedProjects": ["/path/to/project"]
}
```

`models` go to the top of the `/model` list. `providers` adds OpenAI- or Anthropic-compatible endpoints.

`maxTurns` (default 200, 0 = no limit) caps the requests sent to the model for one request of yours. At the cap, the TUI asks whether to go on and a headless run stops. Separately, the bundled `loop-guard` stops a run after five turns in a row in which no tool call ran (refusals or bad arguments over and over; `plugins.settings["loop-guard"].noProgressTurns` changes it, 0 turns it off).

### Permissions

| Mode | Behavior |
| --- | --- |
| `edits` (default) | read / write / edit inside the working directory run without asking; bash and anything outside ask |
| `ask` | Every tool call asks, reads included |
| `agent` | Reads inside the working directory run; everything else is judged by the current model: safe runs, anything else asks |
| `auto` | Everything runs (deny and ask rules still apply) |

- A rule is `tool` or `tool(pattern)`. bash matches the command string with `*` wildcards; read / write / edit match paths with globs.
- The order is deny → softDeny → ask → allow → mode → the `permission` hook (plugins). `softDeny` refuses like deny, but a plugin such as `jev-guard` may lower it to "ask the user" (never to running without asking); it is meant for broad patterns that also catch legitimate calls. An allow rule only lets a command through when **every** part joined with `&&`, `;` or `|` is allowed. Commands with `$(…)`, backticks or `>` never pass by an allow rule.
- The bundled `permission-presets` enables `guard` by default: sudo, `rm -rf /`, disk operations, `| sh` and the like are always denied; `rm -rf ~…` under home and force pushes are softDeny; `ssh`, `scp`, `sftp` and `rsync` to another machine always ask, in `auto` mode too.
- Headless runs have nobody to ask, so a call that needs approval is not run, and the model is told why.
- Paths are judged by where symbolic links lead (the real path). A link inside the working directory that points outside counts as outside, even before its target exists, and links inside a project cannot swap what a rule applies to. A path whose target cannot be determined (a link loop, for example) always asks.
- Permissions are a check before a tool runs, not isolation (a sandbox). Swapping a link between the check and the run, or what an allowed bash command does, is not prevented. For code you do not trust, isolate at the OS level (a container, for example).
- After an interrupt (esc), tools that were approved but not yet started do not run.

### Judging calls with Jev (`jev-guard`)

Adds Jev's judgment after the rules and the permission mode. Jev answers typed questions with probabilities instead of text and takes a few hundred milliseconds a call.

```json
{ "plugins": { "settings": { "jev-guard": { "enabled": true } } } }
```

- `backend` picks where Jev is reached. Without it, whichever of `CMD_API_KEY` and `TYPESAFE_API_KEY` is set is used (the keychain entries `commandcode` / `typesafe` work too).

  | `backend` | Endpoint | Key | Default `model` |
  | --- | --- | --- | --- |
  | `commandcode` | Command Code's Provider API (GOAT plan or above) | `CMD_API_KEY` | `typesafe/jev` |
  | `typesafe` | TypeSafe's API | `TYPESAFE_API_KEY` | `jev-latest` |
  | `openrouter` | OpenRouter's Decisions API (alpha) | `OPENROUTER_API_KEY` | `typesafe/jev-latest` |
  | `vercel` | Vercel AI Gateway | `AI_GATEWAY_API_KEY` | `typesafe-ai/jev` |
  | `chat` | Any sasacode model (`model`: `"<provider>/<model>"`, the current model if omitted) | that provider's key | — |

  OpenRouter and Vercel are used only when named, so people who hold those keys for chat do not start sending judgments. `endpoint` and `apiKeyEnv` change the URL and the key variable (for proxies). Only `commandcode` and `chat` have been checked against the real APIs; `typesafe` / `openrouter` / `vercel` send what their published specs describe (tested against mock servers).
- `chat` asks a chat model the same questions and has it answer in JSON. It is an uncalibrated and slow stand-in: on real commands, DeepSeek V4.1 Flash let through more calls that should have been stopped than Jev ([docs/benchmarks.md](docs/benchmarks.md)), and local models take tens of seconds a call. It is for when Jev cannot be reached but you want the mechanism.
- When enabled, the call (command, paths it may change and their git state, your latest request) is sent to the endpoint. Strings that look like API keys or tokens are masked first (best effort).
- It changes a decision only in these cases:
  - A call that would run without asking (allow rules, `auto` mode): dangerous → denied, needs a look → asks.
  - A call that `agent` mode would ask about: safe → runs without asking (instead of the model's judgment). When Jev holds back, the model judges as before.
  - A softDeny call: safe → asks you instead of refusing.
- Jev never turns a call you would be asked about into a refusal (its verdict is added to the approval dialog). Calls denied by a rule are not sent to Jev.
- Jev gets only facts the harness collected and your own request, never tool output or file contents (Jev could be steered by instructions hidden in them).
- Reads and edits inside the working directory are not judged. When Jev cannot be reached (no key, network error, 5-second timeout), the decision stands without it.
- Thresholds are set with `thresholds` (`allow` 0.8, headless `headlessAllow` 0.9, `deny` 0.85, `confidence` 0.6, `risk` 0.6, `flag` 0.7, `clear` 0.3). Other settings: `timeoutMs` (default 5 seconds, 60 for `chat`) and `skip` (tools never judged; default `todo_write`, `task`).
- `/jev` shows the state and the latest judgments. Judgments are also recorded in the session.
- Jev judges by the command line, so a read on another machine such as `ssh host 'ps aux'` looks safe, and a call whose script it cannot see, such as `bun run clean`, is judged without knowing what the script does. The `guard` preset always asks before operations on other machines. To always be asked about scripts, write a rule (e.g. `"permissions": { "ask": ["bash(bun run *)", "bash(make *)"] }`).
- On 5,135 calls from real session logs: 92% of the safe calls are not stopped in `auto`, 71% run without asking in `agent`, and 1% of the calls that need a look ran without asking in `agent` (locally). Details in [docs/benchmarks.md](docs/benchmarks.md).

### Project trust

A cloned repository can, by itself, only make things stricter. The following can run code or change where data goes, so they are ignored until you trust the project:

- plugins in `.sasacode/plugins`
- in `.sasacode/config.json`: `providers` (endpoints), `modelOverrides`, `plugins` (disabling and settings), `mcpServers`, `allow` rules, a permission mode looser than the global one, a `maxTurns` (0 = unlimited) or `maxRetries` above the global one, and a `model` that points at a provider only the project defines

In such a project, sasacode lists them at start and asks whether to trust it (headless: `--trust-project`). The answer is saved in `~/.sasacode/trust.json` and asked again when those settings or the plugin code (files under `.sasacode/plugins`, not installed dependencies) change. Projects in the global `trustedProjects` are trusted without asking. The model choice, `thinking`, `instructions`, added deny / ask rules and disabled tools, a stricter permission mode and lower limits apply without trust.

## Plugins, MCP and Skills

### Bundled plugins (switch off with `plugins.disabled`)

A plugin of the same name in `~/.sasacode/plugins` or the project is loaded instead of the bundled one.

| Name | What it does |
| --- | --- |
| `tool-repair` | Fixes broken tool calls before validation: broken JSON (trailing commas, missing brackets, unquoted keys, `True`/`None`, code fences, double encoding), key names (`file` → `path`), types (`"20"` → `20`) and typos in tool names, only when there is exactly one candidate. The model is told briefly, the history keeps the fixed call, and the original is recorded in the session |
| `repetition-guard` | Stops generation that starts repeating the same sentence, keeps one copy and asks for the rest (short repeats such as "I see." too, once they go on) |
| `loop-guard` | When the same tool call (or a cycle of up to three, A→B→A→B) keeps giving the same result, adds a note on the 3rd time and blocks every call of the cycle from the 5th (until a file changes or you send the next request). Duplicate calls in one response with nothing in between that could change state run once. Also points out the same error repeating (including calls rejected before running). Stops the run after five turns in which no tool ran |
| `agents-md` | Reads `~/.sasacode/AGENTS.md` and every `AGENTS.md` from the repository root down to the working directory (not CLAUDE.md) |
| `compaction` | Summarizes older history when context reaches 80% or the limit. `/compact` runs it by hand (the footer shows it while it works; an empty summary leaves the history alone) |
| `subagent` | The `task` tool: hands work to a subagent with its own history and gets back only the report (several in one turn run in parallel) |
| `todo` | The `todo_write` tool: keeps a work plan in the session and shows progress in the footer |
| `web-fetch` | The `web_fetch` tool: fetches a URL as text |
| `browsr` | Web search (`search`) and reading pages (`open`) through [browsr-4-agent](https://github.com/sasanokusa/browsr-4-agent); started automatically when `browsr-agent` is on PATH |
| `permission-presets` | Rule presets (`guard`, `read-only-shell`, `tests`) |
| `jev-guard` | Judges calls before they run with [Jev](https://commandcode.ai/models/jev) (TypeSafe), a model made for decisions. **Off by default** (above) |
| `openai-codex` | ChatGPT-plan models (`openai-codex/…`) with the credentials from `sasacode login` |
| `usage` | `/usage`: this run's usage, totals for today, the last 7 and 30 days and all time, and what the ChatGPT plan has left (5-hour and weekly windows, time to reset). `/usage graph` draws daily use like GitHub's contributions graph, `/usage history [days]` lists days, `/usage models` totals by model, `/usage plan` shows only the plan. Read from the session logs in `~/.sasacode/sessions` (compaction and subagents are not included) |
| `goal` | The session's goal. `/goal <goal>` sets it; `done` / `pause` / `resume` / `clear` change it. Ask "make this the goal" and the model sets one with the `set_goal` tool, summarized from the conversation. An active goal is shown in the system prompt and the footer |
| `background-sessions` | The `bg_start` tool: runs long commands such as builds and tests in the background and brings the exit code and the end of the output back into the conversation when they finish (starting the next turn when idle). Judged by the same permission rules as bash (`permissionsAs`). Jobs outlive sasacode and are reported at the next start. `/bg` lists, shows and stops them. POSIX only |
| `auto-reconnect` | When a run ends in an error and the endpoint in use cannot be reached, waits for it to come back (up to 10 minutes by default) and resumes the work. `/reconnect` does the same by hand |
| `skills` / `mcp` | Adapters for Agent Skills and MCP |

### MCP

```json
{
  "mcpServers": {
    "github": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" } },
    "remote": { "url": "https://example.com/mcp", "headers": { "Authorization": "Bearer ${REMOTE_TOKEN}" } }
  }
}
```

stdio and Streamable HTTP are supported. Tools are named `mcp__<server>__<tool>` and go through the same permission checks and hooks as any tool. Prompts become slash commands. Servers connect in the background, so startup does not wait. `${VAR}` is expanded from the environment.

### Skills

Standard Agent Skills (frontmatter `name` and `description`) are read from `~/.sasacode/skills/<name>/SKILL.md`, `.sasacode/skills/<name>/SKILL.md` and plugins' `skills` directories. Only the name, description and path go into the system prompt; the model reads the body with the read tool when it needs it. `/skill:<name> <request>` makes it use one explicitly.

The built-in skill `tool-authoring` (how to write sasacode tools and plugins) is included: `/skill:tool-authoring make a tool that returns the weather`.

### Deferred tool loading

When the definitions of MCP and plugin tools add up to 10% of the context or 30 tools, not all are sent. The model gets a `tool_search` tool with the names and descriptions, and loads what it needs. The four built-in tools and tools with `alwaysLoad` are always sent.

### Writing a plugin

Put it at `~/.sasacode/plugins/hello.ts` and it is loaded (no build step). For type completion in an editor, `npm install --save-dev @sasacode/plugin-api` ([npm](https://www.npmjs.com/package/@sasacode/plugin-api)).

```ts
import { text, type Plugin } from "@sasacode/plugin-api";

export default ((api) => {
  api.registerTool({
    name: "hello",
    description: "Greet someone by name.",
    parameters: { type: "object", properties: { name: { type: "string" } }, required: ["name"] },
    kind: "read",
    execute: async ({ name }) => ({ content: [text(`Hello, ${name}!`)] }),
  });
}) satisfies Plugin;
```

What the public API (now 1.7.0; frozen at 1.4.0, so 1.x only adds; [compatibility promise](docs/plugins.md#互換性の約束140-で確定)) offers:

- **Registration**: tools (with `permissionsAs` to apply another tool's permission rules), slash commands (with argument completion), providers (model lists, custom fetch), permission rules (including `softDeny`)
- **Hooks**: `session_start/end`, `user_prompt`, `system_prompt`, `before_request`, `stream_delta`, `assistant_message`, `tool_call_raw`, `tool_call`, `permission`, `tool_result`, `turn_end`, `agent_end`, `context_limit`
- **UI**: notices, confirmations, choices, status line, custom rendering of tool results
- **Session**: saving state, replacing the history, injecting messages
- **Agent**: subagents, single model calls

More in [docs/plugins.md](docs/plugins.md) (Japanese).

### Sharing plugins

```bash
sasacode plugin search weather                      # the curated list (sasanokusa.com) and npm; no words lists all
sasacode plugin install sasacode-plugin-weather     # shows what it is and asks first
sasacode plugin publish weather.ts --license MIT    # publishes a single-file plugin to npm
```

A published plugin shows up in everyone's `search` through the npm keyword `sasacode-plugin`. The list on sasanokusa.com (`site/plugins.json`) is a few KB of JSON for recommendations only; plugins themselves come straight from npm or GitHub. To get one listed, send a pull request that adds it to `site/plugins.json`. Installing and publishing use the Bun built into sasacode, so neither Bun nor npm has to be installed (except npm for publishing). Without a terminal (scripts), `plugin install` needs `--yes`.

## Sessions

Saved to `~/.sasacode/sessions/<working directory>-<hash>/<date>_<id>.jsonl`, one message or tool result per line as it happens, so a session can be resumed after a crash. A call that stopped before its result was saved is reported to the model as "unknown whether it ran", so it checks before retrying. A half-written last line is moved to `<file>.torn`. The session id is shown in the TUI footer, `/session`, the exit summary, and headless output (the last stderr line in text mode, the first `{"type":"session"}` line in JSONL). `SASACODE_HOME` moves `~/.sasacode`.

## Development

```bash
bun test            # all tests, including LLM-free end-to-end tests (replayed responses, a test MCP server)
bun run typecheck
bun run build       # dist/sasacode for this machine; --all for every target
```

**Releasing**: pushing a `v*` tag makes GitHub Actions build the binaries and create the GitHub release (macOS binaries are built on macOS runners and started once to check them). Installs from sasanokusa.com pick up the new version by themselves. Run `scripts/publish-site.sh` only when the site changes.

| Package | Role |
| --- | --- |
| `@sasacode/ai` | Normalized message types, Anthropic / OpenAI Chat / OpenAI Responses adapters, Models API, recording and replaying responses |
| `@sasacode/agent` | Loop, hooks, tool execution (repair, validation, permissions), deferred tool loading, sessions, plugin host |
| `@sasacode/plugin-api` | The only public API plugins depend on (published on npm) |
| `@sasacode/tools` | read / write / edit / bash (registered through the plugin API) |
| `@sasacode/tui` | The interactive UI, built on pi-tui |
| `@sasacode/cli` | Arguments, configuration, keys, model lists, finding, loading and trusting plugins, headless runs |
| `@sasacode/mcp` / `@sasacode/skills` / `@sasacode/bundled` | MCP and Skills adapters, bundled plugins |

The core (ai, agent, plugin-api, tools) is about 3,600 lines, with a ceiling of 4,000. Runtime dependencies and why they are there are in [docs/dependencies.md](docs/dependencies.md).
