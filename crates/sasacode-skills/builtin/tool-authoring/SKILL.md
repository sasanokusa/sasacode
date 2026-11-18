---
name: tool-authoring
description: Reference for giving sasacode a new tool or command (an MCP server, a Skill, or, for sasacode's own code, a bundled Rust plugin). Use when asked to build a new tool, command or plugin for sasacode.
---

# Giving sasacode a new tool

sasacode 0.20 has no plugin API for outside code: `~/.sasacode/plugins` and `.sasacode/plugins` are not loaded, and `@sasacode/plugin-api` is no longer published. A tool comes from an MCP server, know-how from a Skill. Plugins exist only inside sasacode's own source.

## 1. Pick the form

| Need | Use |
| --- | --- |
| A capability the model calls (API client, generator, checker) | **MCP server**, registered in `mcpServers` |
| A command the user types | an MCP server's **prompt** (it becomes the slash command `mcp__<server>__<prompt>`) |
| Instructions or know-how only, no code | **Skill** (`SKILL.md`) |
| A change to sasacode itself (a hook into the loop, a built-in tool) | a **bundled plugin** in sasacode's repository (section 5) |

## 2. An MCP server

Any language with an MCP SDK works. The smallest is a stdio server, for example in Python with the official SDK (`pip install mcp` or run it with `uv`):

```python
# weather.py
from mcp.server.fastmcp import FastMCP

mcp = FastMCP("weather")

@mcp.tool()
def current(city: str) -> str:
    """Current weather for a city: temperature (°C) and conditions."""
    return f"{city}: 21°C, clear"  # call the real API here

if __name__ == "__main__":
    mcp.run()
```

Rules that make a tool work well for the model:

- The description says what it returns and when to use it; the parameter names and types are the schema the model sees.
- Return text the model can read (short, no secrets). Raise an error for failures: it reaches the model as a failed tool call.
- Mark a tool that only reads with `readOnlyHint`: sasacode then runs it alongside other calls.

## 3. Register it

In `~/.sasacode/config.json` (every project) or the project's `.sasacode/config.json`:

```json
{
  "mcpServers": {
    "weather": { "command": "uv", "args": ["run", "--with", "mcp", "python", "/abs/path/weather.py"], "env": { "WEATHER_KEY": "${WEATHER_KEY}" } }
  }
}
```

- `command` / `args` start a stdio server; `url` (+ `headers`) connects to a Streamable HTTP one. `${VAR}` is expanded from the environment.
- The server gets only `HOME`, `PATH`, `SHELL`, `TERM`, `USER`, `LOGNAME` and its `env`; put the keys it needs in `env`.
- The tool is named `mcp__weather__current` (`"toolNames": "plain"` keeps the server's own name). `timeout` is per call in ms (default 600000).
- A server in a project's config starts only after the user trusts the project.
- Servers connect in the background. After changing the config, restart sasacode or run `/mcp reconnect weather`; `/mcp` shows the state.

## 4. Permissions and testing

- MCP tools are kind `other`: in the `edits` mode the user is asked before each call. To allow one without asking, add a rule: `{ "permissions": { "allow": ["mcp__weather__current"] } }`.
- Try the server by hand first (the SDK's inspector, or run it and send it JSON-RPC), then through sasacode: `sasacode -p "what is the weather in Kyoto?" --output jsonl` shows each tool call and its result.

## 5. Bundled plugins (sasacode's own source only)

A change to sasacode itself is a Rust module in `crates/sasacode-bundled/src`:

1. Write `pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>>` and register tools, commands and hooks through `api` (`crates/sasacode-plugin-api`). `git_context.rs` is a small example of a tool.
2. Add it to `PLUGINS` in `crates/sasacode-bundled/src/lib.rs`: `on(name, module::plugin)` loads it by default, `off(…)` only when `plugins.enabled` names it. The order is the load order and the hook order.
3. Add a row to the bundled-plugin table in README.md and README.en.md, and test it with `cargo test -p sasacode-bundled`.

## 6. Skills

A folder with `SKILL.md` in `~/.sasacode/skills/<name>/` or the project's `.sasacode/skills/<name>/`:

```markdown
---
name: release-notes
description: Write release notes from the commits since the last tag.
---

Steps the model should follow, with the commands to run.
```

`name` and `description` are read; the description decides when the model loads the rest. Skills load at start (restart after adding one).
