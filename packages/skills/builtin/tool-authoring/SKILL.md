---
name: tool-authoring
description: Reference for creating sasacode tools, slash commands and plugins (PluginAPI, ToolDefinition, hooks, permissions, packaging, testing). Use when asked to build a new tool or plugin for sasacode.
---

# Creating sasacode tools and plugins

A plugin is a module whose default export receives a `PluginAPI`. Everything in sasacode is added this way, including the built-in read/write/edit/bash tools, so a plugin can do anything they do.

## 1. Pick the right form

| Need | Use |
| --- | --- |
| A new capability the model calls (API client, generator, checker) | **Tool** in a plugin |
| A command the user types (`/deploy`) | **Command** in a plugin |
| React to the loop (lint after edits, block commands, add context) | **Hook** in a plugin |
| Instructions or know-how only, no code | **Skill** (`SKILL.md`) instead of a plugin |
| A tool that already exists as an MCP server | Add it to `mcpServers` in config; no plugin needed |

## 2. Where files go

- One file: `~/.sasacode/plugins/<name>.ts` (all projects) or `<project>/.sasacode/plugins/<name>.ts` (this project; the user is asked to trust it on first load).
- A package: a directory with `plugin.json`:
  ```json
  { "name": "my-plugin", "version": "0.1.0", "apiVersion": "^1.1.0", "extensions": ["src/index.ts"], "skills": "skills" }
  ```
  or the same fields under `"sasacode"` in `package.json`.
- TypeScript runs as-is (Bun); no build step. `import ... from "@sasacode/plugin-api"` works without installing it.
- New plugins load on the next start of sasacode. Tell the user to restart after you create one.

## 3. Tool template

```ts
import { errorResult, text, type Plugin } from "@sasacode/plugin-api";

const plugin: Plugin = (api) => {
  api.registerTool({
    name: "weather",                       // [a-zA-Z0-9_-], unique; same name replaces an existing tool
    description: "Current weather for a city. Returns temperature (°C) and conditions.",
    parameters: {                          // JSON Schema; arguments are validated before execute()
      type: "object",
      properties: { city: { type: "string", description: "City name, e.g. Tokyo" } },
      required: ["city"],
      additionalProperties: false,
    },
    kind: "other",                         // read | edit | exec | other (see permissions)
    concurrent: true,                      // may run in parallel with other concurrent tools
    summary: (a) => a.city,                // one-line label in the UI
    async execute(args, ctx) {             // ctx: { cwd, signal, onUpdate(text) }
      const res = await fetch(`https://example.com/weather?q=${encodeURIComponent(args.city)}`, { signal: ctx.signal });
      if (!res.ok) return errorResult(`weather service returned ${res.status}; try the English city name`);
      const w = await res.json();
      return { content: [text(`${args.city}: ${w.temp}°C, ${w.conditions}`)], details: { raw: w } };
    },
  });
};
export default plugin;
```

Guidelines:
- **Description**: what it does, what it returns, when to use it. State facts; do not tell the model how to think.
- **Errors**: return `errorResult("what failed; what to try")` rather than throwing (a thrown error is also reported, but without your hint). Never swallow failures.
- **Output size**: cut only at an explicit limit and say so, e.g. `[Showing 1-200 of 950 lines. Use offset=201 to continue.]`, or write the full output to a temp file and return its path.
- **Images**: `{ type: "image", mediaType: "image/png", data: base64 }` in `content`.
- **`details`** is for renderers and logs only; the model never sees it.
- **Long work**: honour `ctx.signal` (abort) and stream progress with `ctx.onUpdate(text)`.

## 4. Permissions

The user's mode decides what runs without asking. In the default mode (`edits`), only `read`/`edit` tools whose `paths()` are inside the working directory run automatically; everything else asks.

- `kind`: `read` (no side effects), `edit` (changes files — implement `paths`), `exec` (arbitrary effects), `other`.
- `paths(args, cwd)`: absolute paths the call touches; enables rules like `mytool(src/**)` and the in-cwd check.
- `matchTarget(args)`: string for pattern rules like `mytool(deploy staging*)`.
- A harmless tool may pre-approve itself: `api.permissions.addRules({ allow: ["mytool"] })`. Do this only when a call cannot hurt anything.

## 5. Commands

```ts
api.registerCommand({
  name: "standup",                   // typed as /standup
  description: "Summarize today's git activity",
  argumentHint: "[author]",
  async run({ args }) {
    api.session.inject(`Summarize git log --since=midnight${args ? ` --author=${args}` : ""}`, "now");
  },
});
```

## 6. Hooks

`api.on(name, handler)`; handlers run in registration order; a throwing handler is reported and skipped.

| Hook | Receives | May return |
| --- | --- | --- |
| `session_start` / `session_end` | `{ sessionId, resumed }` | — (restore / clean up state) |
| `user_prompt` | `{ content }` | `{ content }` to rewrite, `{ handled: true }` to consume |
| `system_prompt` | `{ prompt }` | `{ prompt }` (append context) |
| `before_request` | `{ messages, model }` | `{ messages }` for this request only |
| `tool_call` | `{ call, tool, args }` | `{ args }`, `{ decision: "allow" \| "ask" \| "deny", reason }` |
| `tool_result` | `{ call, result }` | `{ result }` (e.g. append lint output after `edit`) |
| `turn_end` / `agent_end` | `{ turn, message }` / `{ cause }` | `{ inject: "text" }` to continue the loop |
| `context_limit` | `{ tokens, contextWindow }` | `{ retry: true }` after freeing context |

Example — run a linter after every edit:

```ts
api.on("tool_result", async ({ call, result }) => {
  if (call.name !== "edit" && call.name !== "write") return;
  const p = Bun.spawn(["bunx", "biome", "check", String(call.input.path)], { cwd: api.cwd, stdout: "pipe", stderr: "pipe" });
  const out = (await new Response(p.stdout).text()).trim();
  if ((await p.exited) !== 0) return { result: { ...result, content: [...result.content, text(`lint:\n${out}`)] } };
});
```

## 7. Other API

- `api.settings` — this plugin's settings from config: `"plugins": { "settings": { "<name>": { ... } } }`.
- `api.ui` — `notify(msg, level)`, `confirm(title, msg)`, `select(title, options)`, `setStatus(key, text)`, `registerToolRenderer(tool, (call, result, {expanded, width}) => lines)`. Check `api.ui.interactive`: headless runs return false/undefined from confirm/select.
- `api.session` — `append(kind, data)` / `entries(kind)` to persist state across resume; `messages()`; `replaceMessages(msgs)`; `inject(text, "next" | "now")`.
- `api.agent` — `run({ prompt, excludeTools, model })` for a subagent with its own history; `complete({ system, messages })` for one tool-less model call; `model()`, `tools()`.
- `api.registerProvider(name, { api, baseUrl, apiKeyEnv })` — add an LLM endpoint.
- `api.ready(promise)` — report background startup work (headless runs wait for it).
- `api.cwd`, `api.name`, `api.version` (plugin API version).

## 8. Test it

1. Unit-test `execute()` directly with a stub API:
   ```ts
   import plugin from "./weather.ts";
   const tools: any[] = [];
   await plugin({ registerTool: (t) => tools.push(t), registerCommand() {}, on() {}, ready() {}, settings: {}, cwd: process.cwd(),
     permissions: { addRules() {} }, ui: { interactive: false, notify() {}, setStatus() {}, registerToolRenderer() {} } } as any);
   console.log(await tools[0].execute({ city: "Tokyo" }, { cwd: process.cwd(), signal: new AbortController().signal }));
   ```
2. End to end: `sasacode --no-session -p "Use the weather tool for Tokyo"` (add `--permission auto` only in a throwaway directory). `sasacode plugin list` shows what was found; load errors are printed as warnings.

## 9. Checklist

- [ ] Name is unique and descriptive; description says what it returns.
- [ ] Schema has `required` and `additionalProperties: false`.
- [ ] Right `kind`; `paths` for file tools; no self-approval of risky tools.
- [ ] Failures return `errorResult` with a hint; large output is cut with a notice.
- [ ] `ctx.signal` honoured for anything slow.
- [ ] Secrets come from `process.env` or settings, never hard-coded.
- [ ] Tried once headless; told the user to restart sasacode to load it.
