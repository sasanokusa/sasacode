# sasacode

English · [日本語](README.md) · [Documentation (Japanese)](https://sasanokusa.com/sasacode/docs/)

A small coding agent for the terminal whose bundled plugins you reshape in the config (Rust).

The core holds only the agent loop, the providers, four built-in tools (read / write / edit / bash), the TUI, sessions and permission checks. Everything else is added by the bundled plugins through the plugin API, and the built-in tools are registered through that same API. Every plugin can be switched off in the config. To add features from outside, use MCP servers and Agent Skills (`SKILL.md`). To make small models usable, it also ships repair of broken tool calls and a stop for repeated sentences.

0.20 is a rewrite from TypeScript / Bun to Rust. Config files, keys, sessions and the CLI flags work as before. Loading external plugins is gone ([extending sasacode](docs/plugins.md)).

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

On Windows, from PowerShell (installs `%LOCALAPPDATA%\Programs\sasacode\sasacode.exe` and puts it on your PATH):

```powershell
irm https://sasanokusa.com/sasacode/install.ps1 | iex
```

- Windows needs [Git for Windows](https://git-scm.com/download/win): the `bash` tool runs commands with its Git Bash (`winget install --id Git.Git -e`). If it is somewhere unusual, set `SASACODE_GIT_BASH_PATH` to its `bash.exe`. Keys go in `%USERPROFILE%\.sasacode\.env` (the OS keychain lookup is macOS / Linux only).
- Also on npm: `npm install -g sasacode`. The npm package is a small launcher: its first run downloads the binary of the same version from GitHub Releases, checks its SHA256 and keeps it in `~/.sasacode/npm-bin`. Update with `npm install -g sasacode@latest`.
- Single binaries for macOS (arm64 / x64), Linux (x64 / arm64, glibc / musl) and Windows (x64 / arm64). Every x64 build runs on CPUs without AVX2 (the same file is also published under the baseline names used up to 0.15).
- The installer and the binary come from the latest [GitHub release](https://github.com/sasanokusa/sasacode/releases) and are checked against SHA256. The sasanokusa.com URLs redirect to the latest release's `install.sh` / `install.ps1`.
- SHA256 only catches a broken download. That a file was built by this repository's GitHub Actions is proved by the signed provenance attached to releases from v0.9.8: `gh attestation verify sasacode-darwin-arm64.tar.gz --repo sasanokusa/sasacode` (on a file downloaded from the release; `install.sh` too).
- `SASACODE_VERSION=v0.15.2` pins a version and `SASACODE_INSTALL_DIR` changes where it goes (both installers).
- `sasacode update` updates in place (the same checks as the installer; `--check` only reports). A newer version is mentioned quietly once at startup (`"updateCheck": false` turns it off). Installed from source, run `git pull` and the `cargo install` below again.
- RustXNG, which web search (`web_search`) uses, installs with `sasacode web-search install` (`/web-search install` in the TUI): the official release, checksum-checked, into `~/.sasacode/bin`, usable without a restart. Running it again updates to the newest release.
- The desktop GUI (in progress) is not in the release: `sasacode gui install` builds it on this machine (it needs [Rust](https://rustup.rs); without it, it says how to get it and stops). It goes in `~/.sasacode/gui/bin`, and `sasacode update` does not update it: after updating sasacode, run `sasacode gui install` again ([docs/gui.md](docs/gui.md)).

From source ([Rust](https://www.rust-lang.org/) 1.88 or later):

```bash
git clone https://github.com/sasanokusa/sasacode && cd sasacode
cargo install --locked --path crates/sasacode-cli   # installs ~/.cargo/bin/sasacode
```

## Usage

```bash
sasacode                            # interactive (TUI)
sasacode -p "fix the tests"          # headless: the answer on stdout, progress on stderr
sasacode -p "…" --output jsonl       # every event as JSONL
sasacode -p "…" --output jsonl --control stdio   # a parent process answers approvals, sends messages on stdin and can abort ([Headless control](#headless-control))
echo "$LOG" | sasacode -p "why?"     # piped input is appended to the -p prompt (skipped if nothing comes for 5 s; --stdin waits to the end)
sasacode -c                         # resume the latest session in this directory
sasacode -r <id> -p "go on"          # continue a session by id (a conversation without tmux)
```

| Option | Meaning |
| --- | --- |
| `-p, --print <prompt>` | Run headless (piped input is appended to the prompt) |
| `--stdin` | With `-p`: wait for piped input to end, however long it takes |
| `--output <text\|jsonl>` | Headless output format (default `text`) |
| `--control stdio` | With `-p --output jsonl`: a parent process answers approvals, sends messages on stdin and can abort the run ([Headless control](#headless-control)) |
| `-m, --model <provider/model>` | Model (e.g. `anthropic/claude-opus-5`, `openai/gpt-5.5`, `ollama/gemma4:e4b`). A name the provider's model list does not have is an error that names close ones (not checked when the list cannot be had) |
| `--permission <edits\|ask\|agent\|auto>` | Permission mode (default `edits`) |
| `--thinking <off\|low\|medium\|high\|xhigh\|max>` | Reasoning effort |
| `--max-turns <n>` | Limit on requests per run (default 200, 0 = no limit) |
| `-c, --continue` / `-r, --resume <id>` / `--no-session` | Resume / resume by id / do not save |
| `--append-system-prompt-file <path>` | Add the file's text (UTF-8) to the system prompt, after the config's `instructions`. A file that cannot be read is an error; an empty file adds nothing. Works in the TUI too |
| `--trust-project` | Trust the project without asking (for headless runs; see [Project trust](#project-trust)) |
| `--allow <rule>` | Allow calls matching a permission rule for this run, as answering "always" does. Repeatable. The form is that of `rules` in `approval_request` (e.g. `bash(npm test)`): a GUI that starts a process per message carries "always allow" over to the next message with it |
| `--slash` | With `-p`: a prompt that starts with `/<command> args` runs that command (a bundled plugin's or a skill's) instead of going to the model (`/goal …` in a conversation continued with `-r <id>`, say). An unknown command is an error. What the command shows comes as `plugin_notify` and similar lines with `--control stdio`, on stderr otherwise |
| `sasacode endpoint add <name> <url> [--key-env VAR] [--project]` | Add your own server (the format is detected; `--project` writes it to the project's config) |
| `sasacode endpoint remove <name> [--project]` / `sasacode endpoint list` | Remove an added server / list them |
| `sasacode list models\|commands\|skills [--json]` | What a run in this folder can use. `models` is the TUI's `/model` list (`[{"id":"provider/model","contextWindow"?,"maxContext"?,"server"?,"current"?}]`), `skills` is `[{"name","description","path"}]`, `commands` the TUI's commands and the skills' (`[{"name","description","argumentHint"?,"owner","headless"}]`). The bundled plugins' commands are not listed here, since loading a plugin runs its startup work: a `--control stdio` run reports them in its `commands` line |
| `sasacode plugin list --json` | The bundled plugins, as `[{"name","source":"bundled","enabled"}]`. `enabled` follows the config (`plugins.enabled` and `plugins.disabled`). It is all that is left of `sasacode plugin` in 0.20, kept for the GUI |
| `sasacode trust [--yes] [--json]` | Whether this folder is trusted, and what trust would enable (`{"cwd","trusted","needed","complete","items","plugins","mcpServers","endpoints","allow","mode"}`). `--yes` saves the trust, as answering "y" at the start-up question does. 0.20 does not load project plugins, so `plugins` is empty and `complete` is always `true` |
| `sasacode login [status\|logout]` | Sign in with a ChatGPT plan (for `openai-codex/…` models) |
| `sasacode update [--check] [tag]` | Download and install the newest release (checksum-verified; `--check` only reports) |
| `sasacode web-search install [tag]` | Install RustXNG for the `web_search` tool (checksum-verified) |
| `sasacode gui [folder]` | Open the desktop GUI on a folder (`sasacode gui install` puts it in; [docs/gui.md](docs/gui.md)) |
| `sasacode gui install [--path <dir>]` | Build the desktop GUI on this machine with Rust (from this version's tag; from a checkout with `--path`) |
| `-h, --help` / `-v, --version` | Print the usage / the version |

`sasacode plugin` is gone in 0.20. Run, it says that the plugins now ship with sasacode (the optional ones are turned on with `plugins.enabled`) and that tools are added with MCP servers or Skills ([extending sasacode](docs/plugins.md)), and exits with an error.

### Headless control

When another program runs `sasacode -p "…" --output jsonl` as a child process, `--control stdio` lets it answer the calls that need approval over stdin, send the model messages while it runs, and abort the run. Both `-p` and `--output jsonl` are required; without them sasacode prints an error and exits non-zero. stdin is the control channel, so piped input is not appended to the prompt.

On stdout, these lines appear among the usual JSONL events, one per line.

```jsonc
{"type":"approval_request","id":"approval-1","callId":"call_1","tool":"bash","args":{"command":"touch x"},"reason":"…","cwd":"/path/to/project","rules":["bash(touch x)"]}
{"type":"approval_cancelled","id":"approval-1"}
{"type":"control_error","error":"not valid JSON"}
{"type":"ui_request","id":"ui-1","kind":"confirm","title":"…","message":"…"}   // kind: "confirm" | "select" | "input" | "select_many"
{"type":"ui_request","id":"ui-2","kind":"select","title":"…","options":[{"value":"a","label":"A","description":"…"}]}
{"type":"ui_request","id":"ui-3","kind":"input","title":"…","message":"…","placeholder":"…","initial":"…"}
{"type":"ui_request","id":"ui-4","kind":"select_many","title":"…","options":[…],"selected":["a"]}
{"type":"ui_cancelled","id":"ui-1"}
{"type":"plugin_notify","level":"info","text":"…"}                 // "info" | "warning" | "error"
{"type":"plugin_text","title":"…","text":"…","format":"text"}      // "text" | "markdown"
{"type":"plugin_status","key":"goal:goal","text":"…"}             // text: null clears it
{"type":"commands","commands":[{"name":"goal","description":"…","argumentHint":"…","owner":"goal"}]}
{"type":"judged","callId":"call_1","tool":"bash","verdict":"allow","reason":"agent judged safe: …"}   // "allow" | "ask"
```

stdin is read as JSON Lines.

```jsonc
{"type":"approval","id":"approval-1","decision":"allow"}           // "allow" | "always" | "deny"
{"type":"approval","id":"approval-1","decision":"deny","feedback":"why"}
{"type":"approval","id":"approval-1","decision":"allow","args":{"command":"touch y"}}   // allow, with the arguments edited
{"type":"message","text":"check the tests too"}
{"type":"abort"}
{"type":"abort","callId":"call_1"}                                // stop that call alone
{"type":"ui_response","id":"ui-1","value":true}                   // confirm: true/false, select: the value picked, input: a string, select_many: an array of values, null: dismissed
```

- `approval_request` is written once for each call that needs approval, and the call does not run until an answer arrives. `id` goes back in the answer. `callId` is the id of the tool call in the conversation, `tool` and `args` are the call itself, and `reason` is why it needs approval.
- In `approval`, `decision` is `allow` (this once), `always` (allow, and add a rule that allows the same call, as "always allow" does in the TUI, until the process exits) or `deny`. `feedback` reaches the model with a `deny`.
- `rules` in `approval_request` are the allow rules an `always` adds. Given to the next process one by one with `--allow`, "always allow" lasts across runs.
- `allow` and `always` may carry `args`: the call runs with those arguments (same tool). They are checked as the model's are; when they do not fit, or a deny rule matches, the call is denied and the model told why. The model is told, with the result, that the arguments were edited and what ran. An `always` rule is then the edited call's.
- `abort` with a `callId` stops that running tool call alone (a subagent, a long command): its result is an error, and the run goes on. An id that is not running draws a `control_error`. It stops tools that heed the abort signal (bash, subagents, MCP and others).
- The bundled plugins' questions and notices (`api.ui`) reach the parent. A confirm, select, input or multiple-choice question writes a `ui_request` and waits for the `ui_response` with its `id`; `null` dismisses it, answered as a headless run answers (no for a confirm, no answer otherwise). When the run is aborted (`abort` or Ctrl+C), each question still open gets a `ui_cancelled` and is dismissed; once stdin closes, every question is dismissed. Notices come as `plugin_notify`, shown text as `plugin_text`, status items as `plugin_status` (keyed `plugin:key`). To plugins the run is interactive until stdin closes.
- `commands` comes once the plugins have loaded, and again whenever it changes: the slash commands the run can use, each of which `--slash` can run.
- `judged` comes when the `agent` mode's judge (the bundled `agent-judge` plugin) decides a call: `allow` runs it, `ask` (risky, or the judge failed) is followed by an `approval_request`.
- `message` works like input typed while the TUI is busy: the model sees it at its next turn. Use it to add to or redirect the work in progress. `text` must be a non-empty string.
- `abort` is the same as Ctrl+C: the run is interrupted (`cause` in `agent_end` is `aborted`, the exit code 1).
- When the run is interrupted, each request still waiting gets an `approval_cancelled`; an answer to that id then draws a `control_error`.
- A line that cannot be used (not JSON, an unknown `type`, an unknown `id`, a bad value) draws a `control_error`, and reading goes on.
- When stdin is closed, every request waiting and every later one is denied, with the feedback `the controlling process closed stdin`. The run is not aborted.

### TUI

It runs full screen: the conversation scrolls at the top, the input and the footer stay at the bottom. On exit the terminal returns to how it was, and only a summary is left (tokens, requests, cost, models, and the command to resume). To keep the conversation in the terminal as before, set `"tui": { "altScreen": false }`.

| Key | Action |
| --- | --- |
| enter / shift+enter, alt+enter | Send / new line |
| ↑↓ | Input history |
| tab | Completion (slash commands, model names in `/model`, file paths) |
| esc | Interrupt generation or a tool (the model is told on the next input). Queued messages are not sent; they go back to the input after the draft (with their images, as `[Image #1]`, which go with them when sent again; the same holds for a message `/fork` puts back) |
| enter while running | Queue a message for the next turn |
| paste | Up to 40 lines and 4,000 characters shows as it is; a longer paste shows as `[paste #1 +60 lines]` and is sent in full. Backspace or delete removes such a marker, or an `[Image #1]`, as a whole |
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
| `/effort` | Reasoning effort (`off` / `low` / `medium` / `high` / `xhigh` / `max`); without an argument, pick from a list. Shown in the footer. OpenAI-style servers receive it as `reasoning_effort`, and `off` is sent as `none` (servers such as vLLM think unless told so). On llama.cpp, `off` also sends `chat_template_kwargs` with `enable_thinking: false`, since llama-server builds before b10121 ignore `reasoning_effort`. A value a server refuses is replaced with the closest one it takes and remembered (Command Code refuses `none`, so `off` becomes `low` there) |
| `/language` | Switch the interface language (`ja` / `en`; without an argument, pick from a list). It switches at once and is saved as `lang` in `~/.sasacode/config.json`. Text already on screen stays as it was |
| `/copy` | Copy the last answer (pbcopy / wl-copy / xclip, else through the terminal) |
| `/permission` `/help` `/exit` (`/quit`) | Permission mode / help / quit |
| `/usage` `/goal` `/bg` `/reconnect` `/compact` `/mcp` `/skills` `/skill:<name>` `/presets` `/browsr` `/web-search` `/undo` `/gui` | Commands of bundled plugins (with the optional ones turned on, also `/jev` `/checks` `/workflow`) |

The footer shows the model, the reasoning effort, the permission mode, context use (`ctx 77.6k/256k (30%)`), the session id and plugin statuses (MCP connections, TODO progress, …). When a run that took 30 seconds or more finishes, the terminal bell rings (`"tui": { "bell": false }` turns it off).

## Providers and models

Models are named `<provider>/<model>`.

| provider | API | Key |
| --- | --- | --- |
| `anthropic` | Anthropic Messages | `ANTHROPIC_API_KEY` (empty: `ANTHROPIC_AUTH_TOKEN` from the environment as a bearer token, else the `ant auth login` profile) |
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

From `GET /v1/models` it tells OpenAI-style servers (called with Chat Completions) from Anthropic-style ones (called with Messages) and adjusts the URL to what each format expects. For llama.cpp, Ollama and LM Studio (0.3.6 or later) it also reads the real context length (from `/props`, `/api/show` and `/api/v0/models`; a server added with an earlier version, which did not tell LM Studio apart, is recognised when its models are listed). Models of an added server appear in `/model`. A config entry with only a URL, `"providers": { "llama": { "baseUrl": "http://…" } }`, is detected at start too (the result is cached in `~/.sasacode/endpoints.json`).

Endpoints in a project's `.sasacode/config.json` are not used until you trust the project ([Project trust](#project-trust)): even a server without a key receives the conversation and the code.

**Model lists**: when the TUI starts, it calls the Models API (`GET /v1/models`) of every provider with a key (and Ollama) in the background, and shows the models and their context lengths in `/model`. The context length is also used for the footer and the compaction threshold (`modelOverrides` in the config wins). Ollama is read through `/api/show`: the Modelfile's `num_ctx` is used as the real length when set, otherwise the model's maximum is shown for reference (a model already in memory uses the real length from `/api/ps`). LM Studio works the same way: a loaded model uses the length it was loaded with (`loaded_context_length` in `/api/v0/models`), and for one not loaded yet the maximum is shown for reference (LM Studio loads a model with its own default length, not the maximum). An Ollama or LM Studio model whose length is still unknown is asked about again after its reply (and after the next ones while it stays unknown, three times in all), and switches to the length it was actually loaded with. Until then it plans with the default 128K (or with its maximum, when that is smaller). Choosing an Ollama model without `num_ctx` brings one warning that Ollama silently cuts input longer than its default. Headless runs (`-p`) on a local server (Ollama, LM Studio, llama.cpp) or another added OpenAI-style server also ask for the current model's length before they start, and print the same warning on stderr. OpenAI's Models API gives no context length, so the built-in model table fills it in. Services that offer several API formats on one key, such as Command Code, have each model routed to the provider of the matching format (Claude models go to `commandcode-anthropic/…`, for example).

**API keys** are looked up in this order: shell environment → `~/.sasacode/.env` → OS keychain (macOS: `security add-generic-password -s sasacode -a <provider> -w`, Linux: `secret-tool store --label sasacode service sasacode account <provider>`). Blank values are ignored everywhere. The working directory's `.env` is never read (so a repository cannot redirect keys with `ANTHROPIC_BASE_URL`). Keys in use are replaced with `[REDACTED]` before a session log is written. Keys set only in `~/.sasacode/.env` are not passed to the commands the agent runs (bash, background jobs). `~/.sasacode` (session logs, input history, saved long outputs in `tmp/`) is kept private to the user (0700).

## Configuration

`~/.sasacode/config.json` (global) and `<project>/.sasacode/config.json` (project) are merged, the project winning, except that the parts of a project config that need trust are ignored until the project is trusted (below). Permission rules, the `disabled` lists (tools, plugins) and `plugins.enabled` are combined from both, so a project can never remove a global deny / ask rule or re-enable something switched off globally. Values of the wrong type and unknown keys are dropped with a warning.

```json
{
  "model": "commandcode/deepseek/deepseek-v4-flash",
  "models": ["commandcode/deepseek/deepseek-v4-pro", "ollama/gemma4:e4b"],
  "lang": "en",
  "thinking": "high",
  "providers": { "myproxy": { "api": "openai-chat", "baseUrl": "https://…/v1", "apiKeyEnv": "MY_KEY" } },
  "modelOverrides": { "myproxy/some-model": { "contextWindow": 200000, "images": false } },
  "permissions": {
    "mode": "edits",
    "allow": ["bash(git status*)", "bash(bun test*)"],
    "ask": ["bash(git push*)"],
    "deny": ["read(**/.env)", "edit(**/.env)", "write(**/.env)", "bash(rm -rf *)"]
  },
  "instructions": "Text appended to the system prompt",
  "mcpServers": { "github": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"] } },
  "plugins": { "enabled": ["git-context"], "disabled": ["web-fetch"], "settings": { "permission-presets": { "presets": ["guard", "tests"] } } },
  "tools": { "disabled": [] },
  "toolSearch": { "mode": "auto", "percent": 10, "count": 30 },
  "tui": { "altScreen": true, "bell": true },
  "updateCheck": true,
  "trustedProjects": ["/path/to/project"]
}
```

`models` go to the top of the `/model` list. `providers` adds OpenAI- or Anthropic-compatible endpoints. `lang` is the language of the interface (the TUI, the CLI and the bundled plugins alike; `"ja"` or `"en"`; `/language` switches it too). `SASACODE_LANG` wins over it, and without either the system locale decides. Text for the model (tool descriptions and results) is not translated. `updateCheck` controls the new-version notice at startup (default true).

**Cost**: the footer and the end-of-run summary show an estimate from the built-in model table (USD per 1M tokens), not a bill. OpenRouter reports the actual cost of a request, and that is used when it does. `modelOverrides` can set the price of any model.

```json
  "modelOverrides": { "myproxy/some-model": { "price": { "input": 1.25, "output": 10 } } }
```

`maxTurns` (default 200, 0 = no limit) caps the requests sent to the model for one request of yours. At the cap, the TUI asks whether to go on and a headless run stops. Separately, the bundled `loop-guard` stops a run after five turns in a row in which no tool call ran (refusals or bad arguments over and over; `plugins.settings["loop-guard"].noProgressTurns` changes it, 0 turns it off).

### Permissions

| Mode | Behavior |
| --- | --- |
| `edits` (default) | read / write / edit inside the working directory run without asking; bash and anything outside ask. Writing under `.git` or `.sasacode` asks too (git's hooks and config can run code at the next git command, and sasacode's config and rules take effect at the next start), and so does writing a file named `HEAD`, or a `config` in a folder that has `HEAD`, `objects/` and `refs/` (git may take that folder for a repository) |
| `ask` | Every tool call asks, reads included |
| `agent` | Reads inside the working directory run; everything else is judged by the current model: safe runs, anything else asks. `judgeModel` in the config (`"provider/model"`) picks another model to judge, e.g. a fast, cheap one to cut the wait and the cost. The judge reads the call's arguments, so text written to steer the model can steer the judgment too; this is a convenience, not protection against prompt injection (use deny / ask rules and the `guard` preset for that) |
| `auto` | Everything runs (deny and ask rules still apply). Shown in red in the TUI footer |

- A rule is `tool` or `tool(pattern)`. bash matches the command string with `*` wildcards; read / write / edit match paths with globs. Write `\*` for a literal `*`. A path rule only applies to the tool it names, so to protect `.env` list read, edit and write, and add `**/` to include subdirectories (`edit(.env)` only stops edits of the `.env` directly in the working directory). Command rules match the text of the command, so reordered options or a full path to the binary slip past them: they are a guard against mistakes, not a complete defense.
- "Always allow" in the approval prompt adds a rule no wider than what was shown: the command as written (its `*` are not wildcards), or, for a file tool, just that path. The rule lasts until sasacode exits (across `/clear`).
- The order is deny → softDeny → ask → allow → mode → the `permission` hook (bundled plugins). `softDeny` refuses like deny, but a plugin such as `jev-guard` may lower it to "ask the user" (never to running without asking); it is meant for broad patterns that also catch legitimate calls. An allow rule only lets a command through when **every** part joined with `&&`, `;` or `|` is allowed. Commands with `$(…)`, backticks or `>` never pass by an allow rule.
- The bundled `permission-presets` enables `guard` by default: sudo, `rm -rf /`, disk operations, `| sh` and the like are always denied; `rm -rf ~…` under home and force pushes are softDeny; `ssh`, `scp`, `sftp` and `rsync` to another machine always ask, in `auto` mode too.
- The `read-only-shell` preset (off by default) lets read-only commands such as `ls`, `cat`, `rg`, `git status`, `git diff`, `git log` and `git show` run without asking. A rule covers the command itself or the command followed by a space and arguments, so another subcommand such as `git difftool` still asks. `cat`, `head`, `tail`, `rg`, `grep` and the like take any path, so they can read files outside the working directory without asking (paths are not checked). If you do not want that, leave the preset off or add ask rules such as `bash(cat *)` (ask rules are checked before allow rules). The `tests` preset (off by default) is the same: it allows a command such as `cargo test` or `pytest` alone, or followed by a space and arguments, and a longer name such as `yarn test:unit` asks.
- Headless runs have nobody to ask, so a call that needs approval is not run, and the model is told why. With `--control stdio` a parent process answers over stdin and stdout ([Headless control](#headless-control)).
- Paths are judged by where symbolic links lead (the real path). A link inside the working directory that points outside counts as outside, even before its target exists, and links inside a project cannot swap what a rule applies to. A path whose target cannot be determined (a link loop, for example) always asks.
- On file systems that ignore case (the default on macOS and Windows), paths are compared in one case. Deny and ask rules also take letters that fold to the same one (`ſ` and `s`, for example) as the same, though not exactly as the file system itself does (`ß` and `ss`, for example). Allow rules are only compared in lower case.
- Permissions are a check before a tool runs, not isolation (a sandbox). Swapping a link between the check and the run, or what an allowed bash command does, is not prevented. For code you do not trust, isolate at the OS level (a container, for example).
- After an interrupt (esc), tools that were approved but not yet started do not run.
- Esc at an approval prompt refuses the call and interrupts the whole run. Esc while typing instructions for the model goes back to the choices. A command too long for the screen scrolls with shift+↑↓ (the choices stay on screen).

### Judging calls with Jev (`jev-guard`, bundled and off by default)

A plugin that adds the judgment of [Jev](https://commandcode.ai/models/jev) (TypeSafe), a model made for decisions, after the rules and the permission mode. It is off by default and needs a key, so from 0.9.6 to 0.15 it was a plugin installed separately. Since 0.20 it is bundled again and not loaded by default. To have it always, put this in the config (settings written before keep working):

```json
{ "plugins": { "settings": { "jev-guard": { "enabled": true } } } }
```

With `"plugins": { "enabled": ["jev-guard"] }` instead, it is only loaded and the judging stays off; `/jev on` in the TUI turns it on for the session. Backends, what it changes and the thresholds are in [docs/plugins.md](docs/plugins.md#jev-guard).

### Project trust

A cloned repository can, by itself, only make things stricter. The following can run code or change where data goes, so they are ignored until you trust the project:

- in `.sasacode/config.json`: `providers` (endpoints), `modelOverrides`, `plugins` (enabling, disabling and settings), `mcpServers`, `allow` rules, a permission mode looser than the global one, a `maxTurns` (0 = unlimited) or `maxRetries` above the global one, and a `model`, `models` or `judgeModel` that points at a provider only the project defines

In such a project, sasacode lists them at start and asks whether to trust it (headless: `--trust-project`). The answer is saved in `~/.sasacode/trust.json` and asked again when those settings change. Projects in the global `trustedProjects` are trusted without asking. The model choice, `thinking`, `instructions`, added deny / ask rules and disabled tools, a stricter permission mode and lower limits apply without trust. Up to 0.15 the plugins in `.sasacode/plugins` were covered too; 0.20 does not load that folder, so they are no longer part of the decision (a project without project plugins that was trusted under 0.15 stays trusted).

## Plugins, MCP and Skills

### Bundled plugins (switch off with `plugins.disabled`)

Each is left unloaded when its name is in `plugins.disabled`, and its settings go in `plugins.settings.<name>`. Plugins put in `~/.sasacode/plugins` or a project's `.sasacode/plugins` up to 0.15 are not loaded from 0.20 (nor is there a way to load one in place of the bundled plugin of the same name).

| Name | What it does |
| --- | --- |
| `tool-repair` | Fixes broken tool calls before validation: broken JSON (trailing commas, missing brackets, unquoted keys, `True`/`None`, code fences, double encoding), key names (`file` → `path`), types (`"20"` → `20`), typos in tool names, names without their MCP prefix (`get_file_contents` → `mcp__github__get_file_contents`) and other harnesses' names for the built-in tools (`read_file`, `str_replace_editor`, `run_command` and so on; `str_replace_editor` with `command` `view` / `create` goes to `read` / `write`), only when there is exactly one candidate. The model is told briefly, the history keeps the fixed call, and the original is recorded in the session |
| `text-tool-calls` | Picks up tool calls that a model writes into its reply instead of making them (llama.cpp and other servers whose chat template has no tool-call format) and runs them. Only for models served from this machine or the local network (a `baseUrl` with a private address, a name without a dot such as `localhost`, or a name ending in `.localhost` / `.local` / `.lan` / `.internal`): a model behind a cloud API makes native calls, so a call in its text is taken for an example (`always: true` in `plugins.settings.text-tool-calls` picks them up from any model). The forms are `<tool_call>{"name": …, "arguments": …}</tool_call>` (Qwen / Hermes), a `json` code fence holding only `{"name": …, "arguments": …}` (or `"parameters"`), and `<function=name>{…}</function>` (Llama 3.x). Only the calls that end the text of a reply count, in a reply that made no native call and was not cut off. An example followed by more text, or one inside a code block (also one left open), is not picked up, and a `json` code fence only after at most 300 characters of prose. Tool names must be known or resolve to one tool by tool-repair's rule (typos aside); otherwise the reply is left alone, as is a reply with signed thinking. tool-repair mends broken JSON. A call that is not a read may be an example, so outside auto mode it is asked about first (also a write or edit that edits mode would run unasked, and a call agent-judge would judge in agent mode). Headless runs (`sasacode -p`) cannot ask and do not run such calls; use auto mode or allow rules there. The model is told (with how many calls earlier in the text did not run, examples in code aside, if any), and the original text is recorded in the session. On by default (left as they are, such replies count as final answers without tools and end the run) |
| `repetition-guard` | Stops generation that starts repeating the same sentence, keeps one copy and asks for the rest (short repeats such as "I see." too, once they go on) |
| `thinking-budget` | Local models only (`server` set or a local/private `baseUrl`, excluding Ollama cloud models whose id ends with `-cloud` or contains `:cloud` / `-cloud:`; also runs on cloud models when `maxChars` / `cloudMaxChars` is set): stops runaway thinking blocks once they pass the limit (`localMaxChars`, 12,000 chars by default, which can be raised in settings) and asks the model to act with a tool or answer |
| `error-hints` | Appends actionable hints on tool errors: closest existing paths (`Did you mean:`) when `read` / `edit` hits ENOENT, the `SKILL.md` path (or similar tool names) on `Unknown tool`, and likely implementation files inferred from imports or stack traces on failing tests |
| `loop-guard` | When the same tool call (or a cycle of up to three, A→B→A→B) keeps giving the same result, adds a note on the 3rd time and blocks every call of the cycle from the 5th (until a file changes or you send the next request). Duplicate calls in one response with nothing in between that could change state run once. Also points out the same error repeating (including calls rejected before running). Stops the run after five turns in which no tool ran |
| `write-guard` | When `write`'s content is a copy of `read`'s output, removes the line numbers before writing and tells the model (only when every line has a space-padded number and they count up by one; if lines were dropped or added among them, nothing is written). A file written back with only the numbers and tabs taken off, so that read's padding (1 to 5 spaces) starts every line, gets those spaces removed the same way. Refuses to overwrite an existing file of 20 or more lines with fewer than half as many, and points the model to `edit`. The same `write` sent again goes through. `lineNumbers: false` / `shrink: false` turn each off; `minLines` sets the line count |
| `search-tools` | Adds bounded, `.gitignore`-aware `find_files` and `search_text` tools (skipping `.git`, `node_modules`, `target` and capping output at 50 matches by default, up to 200) |
| `syntax-check` | Runs a fast syntax check right after `edit` / `write` / `hash_edit` succeeds (JSON, Rust, JS/TS via `bun` / `node`, Python via `python3`) and appends `[syntax-check]` with context lines only when that edit broke the file; external checkers run in a private directory outside the project and execute none of its code |
| `action-nudge` | Local models only (same local-model rule as `thinking-budget`): when the final sentence/line declares an intent to act ("Let me check...", "確認します") and stops without calling any tool, nudges it to call the tool right away (up to 2 times per prompt; ignores questions, conditional offers such as "if you want" / "必要であれば", completion reports, and long prose) |
| `agents-md` | Reads `~/.sasacode/AGENTS.md` and every `AGENTS.md` from the repository root down to the working directory (not CLAUDE.md) |
| `compaction` | Summarizes older history when context reaches 80% or the limit. `/compact` runs it by hand (the footer shows it while it works; an empty summary leaves the history alone). The summary request is sized to fit the context length, so local models with a small one work too (the summary gets up to 15% of it, and the oldest part is left out when the history does not fit). Automatic compaction keeps the latest history (15% of the context length, at most 40k tokens) as it was, not summarized: the latest steps stay even when one prompt was followed by a long run of tool calls. `/compact` summarizes all of it. Either way the conversation stays on screen (a resumed session shows it from before the summary too) |
| `subagent` | The `task` tool: hands work to a subagent with its own history and gets back only the report (several in one turn run in parallel). With `background: true` the caller keeps working and the report arrives as a message when the run ends (interrupting the caller with Esc stops it too); `task_status`, `task_send` and `task_stop` watch, steer and stop runs. A run that fails reports why, with its last message and tool calls. The user can do the same with `/task` (list and details, `/task send <id> <message>`, `/task stop <id>`). Subagents cannot use the `task` tools, `todo_write` or `set_goal` (the caller's TODO list and goal stay its own) |
| `todo` | The `todo_write` tool: keeps a work plan in the session and shows progress in the footer. When unfinished items sit through 10 tool calls without an update, a reminder that quotes the plan follows the tool results (a message to the model, kept in the history and shown as one dim line; `remindAfter` changes the count, 0 turns it off) |
| `progress-notes` | Asks for short progress notes while the model works: the system prompt asks it to say in a sentence what it will do before its first tool call, then to note what it found, finished or changed course on. After 5 tool-call replies in a row with no text, a reminder follows the tool results (kept and shown like `todo`'s; `remindAfter` changes the count, 0 turns off only the reminder). Not added for subagents |
| `web-fetch` | The `web_fetch` tool: fetches a URL as text. Menus, tables of contents, language lists, ad furniture and inline CSS/JS are left out, and when the page marks where its content is (`main`, `article`, Wikipedia's content) that is what is read (collapsed parts within it included). The site's navigation links follow the content as one line (25 at most). The page's links come back as a numbered list of absolute URLs, so the agent follows them instead of putting URLs together by guesswork. On a 404 it points at the site's `llms.txt` / `sitemap.xml` when they exist, and at a search. Redirects are followed one hop at a time, and one that leads to an internal address (loopback, private, link-local and the like) is stopped (the first URL is the one the user approved, so a dev server on this machine is still read, and a redirect within its origin is followed); a redirected result names the URL it came from |
| `web-search` | The `web_search` tool: searches through [RustXNG](https://github.com/sahenjp/RustXNG), a keyless SearXNG-compatible metasearch. With `include_text`, the first three results that match the query (`textPages` changes the number) are read the way `web_fetch` reads a page, and their main text comes along (pages on public addresses only, connected to the very address the name resolved to, and each redirect gets the same check; `privatePages: true` lifts that, for an intranet search server). With `rustxng` (on PATH, or installed by `sasacode web-search install`), the first search starts it on `127.0.0.1:47888`, and it exits by itself after `RUSTXNG_IDLE_TIMEOUT` (600 s by default) unused: nothing stays resident, and sessions running meanwhile share it. `url` in `plugins.settings.web-search` uses a RustXNG or SearXNG you keep running instead. Also `command`, `port`, `idleTimeout`, `results`, `textPages`, `privatePages`. Left out while browsr's search is connected, so the model is not given two ways to search |
| `browsr` | Web search (`search`) and reading pages (`open`) through [browsr-4-agent](https://github.com/sasanokusa/browsr-4-agent); started automatically when `browsr-agent` is on PATH |
| `permission-presets` | Rule presets (`guard`, `read-only-shell`, `tests`) |
| `agent-judge` | The `agent` mode's judge: a model (`judgeModel`, else the current one) decides the calls the mode would ask about. Never loosens a rule. Disabled, `agent` mode asks |
| `openai-codex` | ChatGPT-plan models (`openai-codex/…`) with the credentials from `sasacode login` |
| `usage` | `/usage`: this run's usage, totals for today, the last 7 and 30 days and all time, and what the ChatGPT plan has left (5-hour and weekly windows, time to reset). `/usage graph` draws daily use like GitHub's contributions graph, `/usage history [days]` lists days, `/usage models` totals by model, `/usage plan` shows only the plan. Read from the session logs in `~/.sasacode/sessions` (including subagents, compaction summaries, the agent-mode judge and replies discarded for a retry; logs from earlier versions do not have them) |
| `goal` | The session's goal. `/goal <goal>` sets it; `done` / `pause` / `resume` / `clear` change it. Ask "make this the goal" and the model sets one with the `set_goal` tool, summarized from the conversation. An active goal is shown in the system prompt and the footer |
| `background-sessions` | The `bg_start` tool: runs long commands such as builds and tests in the background and brings the exit code and the end of the output back into the conversation when they finish (starting the next turn when idle). Judged by the same permission rules as bash (`permissionsAs`). Jobs outlive sasacode and are reported at the next start. `/bg` lists, shows and stops them. On Windows it runs with Git Bash |
| `auto-reconnect` | When a run ends in an error and the endpoint in use cannot be reached, waits for it to come back (up to 10 minutes by default) and resumes the work. When the server keeps returning a temporary error (429, 5xx), retries up to 4 times with growing waits. `/reconnect` does the same by hand |
| `checkpoints` | Keeps what write / edit changed just before it changed it, in the session. `/undo` brings one call's changes back at a time (`/undo list` shows what is left, `/undo <path>` picks one). bash changes are not covered |
| `gui` | The `/gui` command: opens the desktop GUI (`sasacode-gui`, in progress) on this folder, with the conversation under way loaded. `sasacode-gui` is looked for in `plugins.settings.gui.command`, then the copy `sasacode gui install` built, then on PATH. From then on, go on in one of the two: the TUI and the GUI do not read what the other adds |
| `image-attach` | Sends images with a message to a vision model. Paste a screenshot with ⌘V (ctrl+v where the terminal does not pass it on) or drop a file into the terminal: it is read right away and shows as `[Image #1]` in the input, and the message you type goes with it. Also `@path`, `/image <path…>`, or `/image` alone for the clipboard (macOS, Windows; Linux with `wl-paste` or `xclip`). The type is read from the bytes (PNG, JPEG, GIF, WebP); 5 MB each, 8 per message. `dropPaths: false` in `plugins.settings.image-attach` ignores paths without `@` |
| `image-slim` | Sends the history's images smaller in every request. An image over the size providers scale down to anyway (about 1.05 megapixels, `maxPixels`) is resized, aspect ratio kept, and an opaque PNG of 100 KB or more becomes a JPEG (quality 90). An image a tool returns is shrunk at once and kept that way in the session, and the model is told the factor that takes its positions back to the original. Old images are replaced by a short note: only the newest 4 to 11 go (`keepImages`, `dropStep`; they are left out 8 at a time, so in between the start of the prompt stays the same and the cache holds). The latest message's images always go, and more are left out past 8 MiB of images. `/image-slim` shows the counts for the last request; `models` limits it to some models |
| `context-slim` | Local models only (same local-model rule as `thinking-budget`): once estimated tokens cross a context-window threshold (55% by default), replaces old oversized outputs of `read` (except `SKILL.md`), `bash`, `find_files`, `search_text`, and `grep`/`glob`/`ls` in step batches (`maskStep`, 4 by default)—never masking subagent (`task`) reports or other tools—while keeping recent turns intact and preserving the prompt prefix between batches for KV cache reuse |
| `diagnostics` | API diagnostics (off by default). `/diagnostics on` records metadata only (whether requests succeeded, stop reasons, token counts), and `show` lists the last 100. No contents or secrets are recorded |
| `skills` / `mcp` | Adapters for Agent Skills and MCP |

These nine are bundled too but not loaded by default (up to 0.15 the first six were installed separately). A name in `plugins.enabled` loads it; a name also in `plugins.disabled` stays unloaded. A `plugins.enabled` in a project's config is not used until the project is trusted.

```json
{ "plugins": { "enabled": ["git-context", "project-checks"] } }
```

| Name | What it does |
| --- | --- |
| `hashline-edit` | Prefixes `read` lines with `<line>:<2-hex-hash>|`, adds the `hash_edit` tool so small models can edit by line anchors without exact indentation copies, and strips accidental `N:hh|` prefixes in `write` ([more](docs/plugins.md#hashline-edit)) |
| `ask-user` | The `ask_user` tool: instead of guessing, the model asks you to decide, with choices (1 to 4 questions at a time). Headless runs ask nothing: the model is told to decide and state what it assumed ([more](docs/plugins.md#ask-user)) |
| `git-context` | The `git_context` tool: the Git state, the change statistics and the patch a review needs, in one call. Read-only; needs Git ([more](docs/plugins.md#git-context)) |
| `project-checks` | The `project_checks` and `project_check` tools and `/checks`: run tests, lint, type checks and so on by name and get PASS / FAIL, the exit code, the time and the log. Judged by the same permission rules as bash. Checks named in `verifyOnFinish` also run, unasked, when the model is about to finish, and a failure's output goes back to it ([more](docs/plugins.md#project-checks)) |
| `workflows` | `/workflow`: call up requests you make again and again, such as a review, looking into a failure or a check before release. The config can add, override and remove them ([more](docs/plugins.md#workflows)) |
| `jev-guard` | Has Jev judge tool calls before they run. How to turn it on is in "Judging calls with Jev" above ([more](docs/plugins.md#jev-guard)) |
| `free-search` | Connects the MCP server `uvx free-search-mcp` under the name `free-search` (web search that runs locally with no key: several search engines, fetching pages, reading documents). Its tools are `mcp__free-search__<tool>`. Needs [uv](https://docs.astral.sh/uv/) ([more](docs/plugins.md#free-search)) |
| `clean-retry` | After repeated edit errors or failing test runs, restores modified files back to their pre-task snapshots, trims the failed history, and retries cleanly with a failure summary ([more](docs/plugins.md#clean-retry)) |
| `slim-profile` | For small local models: omits secondary plugins (`subagent`, `todo`, `goal`, web tools, Skills, etc.) unless explicitly enabled, adds concise rules, and caps huge `read` / `bash` outputs ([more](docs/plugins.md#slim-profile)) |

### MCP

```json
{
  "mcpServers": {
    "github": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" } },
    "remote": { "url": "https://example.com/mcp", "headers": { "Authorization": "Bearer ${REMOTE_TOKEN}" } }
  }
}
```

A stdio server in the global `~/.sasacode/config.json` with no `cwd` starts in a folder of its own outside the project (`~/.sasacode/tmp/mcp/<server name>`). `npx` and `uvx` look at the current folder's `node_modules` and `pyproject.toml` first, so what a project contains must not get to run. For a server that should run in the project folder, write `"cwd": "${SASACODE_PROJECT}"`. A server whose launch (`command` or `url`) the project's `.sasacode/config.json` writes itself (once trusted) starts in the project folder, as before; a project that only adds `disabled` or the like to a global server leaves it a global one. The bundled `free-search` also starts outside the project.

stdio and Streamable HTTP are supported. Tools are named `mcp__<server>__<tool>` and go through the same permission checks and hooks as any tool. Prompts become slash commands. Servers connect in the background, so startup does not wait. `${VAR}` is expanded from the environment. A server that drops is reconnected on its own (a few times, further apart; `/mcp reconnect [name]` by hand), and a tool that leaves the server's list leaves the model's. Servers in a project's `.sasacode/config.json` do not start until the project is trusted ([Project trust](#project-trust)). The per-server `cwd`, `disabled`, `alwaysLoad`, `timeout` and `toolNames` are in [docs/plugins.md](docs/plugins.md).

### Skills

Standard Agent Skills (frontmatter `name` and `description`) are read from `~/.sasacode/skills/<name>/SKILL.md` and `.sasacode/skills/<name>/SKILL.md`. For the same name, the project's wins. Only the name, description and path go into the system prompt; the model reads the body with the read tool when it needs it. `/skill:<name> <request>` makes it use one explicitly, and `/skills` lists them. Plugins' `skills` directories, read up to 0.15, are gone in 0.20.

The built-in skill `tool-authoring` is included too (written out to `~/.sasacode/builtin-skills`).

### Deferred tool loading

When the definitions of MCP and bundled plugin tools add up to 10% of the context or 30 tools, not all are sent. The model gets a `tool_search` tool with the names and descriptions, and loads what it needs. The four built-in tools and tools with `alwaysLoad` are always sent.

### Extending with MCP and Skills

TypeScript plugins put in `~/.sasacode/plugins` up to 0.15, and finding, installing and publishing them with `sasacode plugin`, are gone in 0.20. The npm package `@sasacode/plugin-api` is not published from 0.20 either. The 1.x plugin API (last version 1.15.0) ended with 0.20 and lives on as the internal Rust API for the bundled plugins (`crates/sasacode-plugin-api`). To add your own tools or commands, use one of these:

- **Something that runs code**: make it an MCP server and put it in `mcpServers`. A stdio server can be written in any language. Its tools go to the model, and its prompts become slash commands.
- **Steps or knowledge**: make it a Skill and put it in `~/.sasacode/skills/<name>/SKILL.md` or the project's `.sasacode/skills/<name>/SKILL.md`.

The config keys, the `SKILL.md` format, the bundled plugins' settings and how a bundled plugin is written (for contributors) are in [docs/plugins.md](docs/plugins.md) (Japanese).

## Sessions

Saved to `~/.sasacode/sessions/<working directory>-<hash>/<date>_<id>.jsonl`, one message or tool result per line as it happens, so a session can be resumed after a crash. A call that stopped before its result was saved is reported to the model as "unknown whether it ran", so it checks before retrying. A half-written last line is moved to `<file>.torn`. The session id is shown in the TUI footer, `/session`, the exit summary, and headless output (the last stderr line in text mode, the first `{"type":"session"}` line in JSONL, whose `contextWindow` is the model's context length, or `null` when unknown). `SASACODE_HOME` moves `~/.sasacode`.

## Development

```bash
cargo test --workspace --locked   # LLM-free tests (replayed responses, a fake model server, a test MCP server)
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo xtask checks                # repository checks (core lines, README matching the CLI, …)
cargo xtask improve               # the improvement loop's signals (checks, core lines, repository size)
cargo xtask build                 # the release binaries
```

Development needs only the Rust toolchain (cargo); Bun is not used from 0.20.

**Releasing**: pushing a `v*` tag makes GitHub Actions build the binaries and create the GitHub release. The version is the root `Cargo.toml`'s (`version` under `[workspace.package]`), and `sasacode --version` prints it. The workflow checks that the tag matches it, builds the macOS binaries on macOS runners, starts all the binaries where they run (x64 macOS under Rosetta, musl in an Alpine container), and attaches signed provenance. The release file names are the same as in 0.15. Installs from sasanokusa.com pick up the new version by themselves. Run `scripts/publish-site.sh <host>` (or set `SASACODE_SITE_HOST`) when the site changes and after a release: the version text on the pages is filled in at that time. Once the release is out, `cd npm/sasacode && npm publish` brings the npm `sasacode` to the same version (keep `npm/sasacode/package.json` at the root `Cargo.toml`'s version; the release workflow stops if they differ).

| Crate | Role |
| --- | --- |
| `sasacode-ai` | Provider-neutral message types, Anthropic / OpenAI Chat / OpenAI Responses adapters, cost computation |
| `sasacode-agent` | Loop, hooks, tool execution (repair, validation, permissions), deferred tool loading, sessions, plugin host |
| `sasacode-plugin-api` | The API the bundled plugins use (tools, commands, providers, hooks, UI, session, agent). Internal from 0.20, not published |
| `sasacode-tools` | read / write / edit / bash (registered through the plugin API) |
| `sasacode-tui` | The full-screen interactive UI, built on ratatui and crossterm |
| `sasacode-cli` | The `sasacode` command: arguments, configuration, keys, model lists, the trust check, loading the bundled plugins, headless runs |
| `sasacode-mcp` / `sasacode-skills` / `sasacode-bundled` | The MCP client (stdio and Streamable HTTP), Agent Skills, bundled plugins |
| `sasacode-host` | What the TUI, the CLI and the bundled plugins share: the interface language (ja / en translations), the built-in providers and known models (prices, context windows), and the session index |
| `sasacode-protocol` / `sasacode-gui` | Types of the JSONL events, the `--control stdio` messages and session logs (read by the GUI), the desktop GUI |
| `sasacode-testing` | For tests: a provider that replays responses, and a fake model server |
| `xtask` | Repository tasks: building, checks, the improvement loop's measurements, packing the release files |

The desktop GUI (Rust, in progress) lives in `crates/`. `cargo run -p sasacode-gui -- <folder>` opens it; each prompt runs `sasacode -p … --output jsonl --control stdio` in that folder (`SASACODE_BIN` or `--sasacode` names the executable). Once it is installed with `sasacode gui install --path .`, `sasacode gui [folder]` opens it too, and so does `/gui` in the TUI (with the conversation under way loaded). `cargo test` also checks that the CLI's output and session logs still read. Design and stages: [docs/gui.md](docs/gui.md); how it compares with other agents' windows: [docs/gui-parity.md](docs/gui-parity.md).

The core (`sasacode-ai`, `sasacode-agent`, `sasacode-plugin-api`, `sasacode-tools`) has a ceiling of 12,500 lines of at most 120 columns (rustfmt's `max_width = 120`; a line that is mostly one string literal may run longer): `cargo xtask checks` fails past either, and `cargo xtask improve` prints what it is now.

Repository docs: [requirements](docs/requirements.md), [milestone checks](docs/milestones.md), [runtime dependencies](docs/dependencies.md), [extending sasacode (MCP, Skills, bundled plugins)](docs/plugins.md), [API compatibility](docs/api-compatibility.md), [benchmarks](docs/benchmarks.md) and [how the improvement loop runs](docs/improvement.md). Improvements are kept small and stacked one round at a time: the procedure, the lenses and the record of what was done are in [docs/improvement.md](docs/improvement.md).
