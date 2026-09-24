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
  { "name": "my-plugin", "version": "0.1.0", "apiVersion": "^1.4.0", "extensions": ["src/index.ts"], "skills": "skills" }
  ```
  or the same fields under `"sasacode"` in `package.json`.
- TypeScript runs as-is (Bun); no build step. `import ... from "@sasacode/plugin-api"` works without installing it. For editor types, `npm install --save-dev @sasacode/plugin-api` (published on npm; its version is the API version).
- New plugins load on the next start of sasacode. Tell the user to restart after you create one.
- `apiVersion` `^1.4.0` needs sasacode 0.8 or later. Use the lowest version whose features you use (1.0 base, 1.1 `ready`, 1.2 `stream_delta` / `assistant_message` / `tool_call_raw` / sampling, 1.3 command `complete`, 1.4 `tool_result` for calls that did not run (`ran`), `turn_end` `stop`, 1.5 `Provider.listModels` and `Request.fetch`). The API was frozen at 1.4: 1.x only adds, so ignore fields and values you do not know. Only `@sasacode/plugin-api` exports are public.

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
    // alwaysLoad: true,                   // see "Deferred loading" below
    // paths: (a, cwd) => [...],           // files touched (permissions); for kind "edit"
    // matchTarget: (a) => a.city,         // string matched by rules like weather(Tok*)
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
- **Deferred loading**: when MCP and plugin tools together reach 30 tools (or 10% of the context window), their full definitions are no longer sent; the model sees a `tool_search` tool listing names and first-line descriptions and loads what it needs. Make the first line of the description say what the tool is for. Set `alwaysLoad: true` only for a tool that must always be visible (it costs context on every request).

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
  // Optional: tab completion for the argument.
  complete: (prefix) => ["alice", "bob"].filter((a) => a.startsWith(prefix)).map((a) => ({ value: a, label: a })),
});
```

## 6. Hooks

`api.on(name, handler)`. Handlers run in registration order; each one sees the event as changed by the ones before it. A handler that throws is reported as a plugin error and skipped; the loop goes on. Handlers may be `async` except `stream_delta`.

Order within one model response:

```
before_request → stream_delta (while streaming) → assistant_message
  → for each tool call: tool_call_raw → lookup + schema validation → tool_call → permission → execute → tool_result (also for calls stopped on the way)
  → turn_end
```

| Hook | When | Receives | May return |
| --- | --- | --- | --- |
| `session_start` | session started or resumed | `{ sessionId?, resumed }` | — (restore state from `api.session.entries`) |
| `session_end` | before `/clear`, `/resume`, exit | `{ sessionId? }` | — (clean up) |
| `user_prompt` | before user input goes to the model | `{ content }` | `{ content }` to rewrite; `{ handled: true }` to consume it |
| `system_prompt` | every request | `{ prompt }` | `{ prompt }` (append context) |
| `before_request` | right before the model call | `{ messages, model, sampling }` | `{ messages }` for this request only (history is unchanged); `{ sampling: { temperature?, topP?, frequencyPenalty?, presencePenalty?, extraBody? } }` (`extraBody` = provider-specific fields; Anthropic's newer models reject sampling fields) |
| `stream_delta` | each streamed delta of text, thinking or tool-call arguments | `{ kind: "text" \| "thinking" \| "toolcall", index, delta, text, message }` — `text` is the block's accumulated text | `{ stop: "reason" }` ends generation. **Must be synchronous**: a returned Promise is ignored and reported. Keep it cheap (runs on every delta) |
| `assistant_message` | response finished, before it is stored | `{ message, stopped? }` — `stopped: { plugin, reason }` when a `stream_delta` handler stopped it | `{ message }` to rewrite; `{ retry: true }` to discard and ask again (at most 2 retries per response); `{ inject: "text" }` to add a user message and continue |
| `tool_call_raw` | before the tool is looked up and arguments validated | `{ call, name, input, rawInput?, tools, stopReason }` — `rawInput` is the raw string when the arguments were not valid JSON; `tools` includes deferred tools | `{ name?, input?, note }` to repair. The model sees `[harness repaired this call: <note>]` in the result; history keeps the repaired call (the original goes to the session log). Not called for calls cut off by `max_tokens` |
| `tool_call` | after validation, before permission | `{ call, tool, args }` | `{ args }` to rewrite; `{ decision: "allow" \| "ask" \| "deny", reason }` (strongest wins: deny > ask > allow). A plugin's decision replaces the permission mode's default, but the user's own deny/ask rules still apply |
| `tool_result` | a call's result is final | `{ call, result, ran }` — `ran: false` for calls that never ran (unknown tool, invalid arguments, denied, interrupted) | `{ result }` to change or append |
| `turn_end` | after a response and its tools | `{ turn, message }` | `{ inject: "text" }` to keep the loop going, or `{ stop: "reason" }` to end the run (1.4) |
| `agent_end` | the loop stopped | `{ cause }` — done, aborted, error, refusal, context_limit, max_turns, stopped (with `stopped: { plugin, reason }`) | `{ inject: "text" }` to start another run |
| `context_limit` | the context window is full | `{ tokens, contextWindow }` | free space with `api.session.replaceMessages`, then `{ retry: true }` (at most twice in a row) |

Subagents (`api.agent.run`) share `system_prompt`, `before_request`, `stream_delta`, `assistant_message`, `tool_call_raw`, `tool_call` and `tool_result` handlers; session hooks (`session_*`, `user_prompt`, `turn_end`, `agent_end`, `context_limit`) are not called for them.

Example — run a linter after every edit:

```ts
api.on("tool_result", async ({ call, result }) => {
  if (call.name !== "edit" && call.name !== "write") return;
  const p = Bun.spawn(["bunx", "biome", "check", String(call.input.path)], { cwd: api.cwd, stdout: "pipe", stderr: "pipe" });
  const out = (await new Response(p.stdout).text()).trim();
  if ((await p.exited) !== 0) return { result: { ...result, content: [...result.content, text(`lint:\n${out}`)] } };
});
```

Example — accept an argument alias a small model keeps using (runs before validation):

```ts
api.on("tool_call_raw", ({ name, input }) => {
  if (name === "read" && "filename" in input && !("path" in input)) {
    const { filename, ...rest } = input;
    return { input: { ...rest, path: filename }, note: 'argument "filename" → "path"' };
  }
});
```

Example — stop a response that goes on too long, then continue with a nudge:

```ts
api.on("stream_delta", ({ kind, text }) => (kind === "text" && text.length > 20_000 ? { stop: "too long" } : undefined));
api.on("assistant_message", ({ stopped }) => {
  if (stopped?.plugin === api.name) return { inject: "Your answer was cut off for length. Summarize the rest briefly." };
});
```

## 7. Other API

- `api.settings` — this plugin's settings from config: `"plugins": { "settings": { "<name>": { ... } } }`.
- `api.ui` — `notify(msg, level?)`, `confirm(title, msg?)`, `select(title, options)`, `setStatus(key, text | undefined)` (footer), `registerToolRenderer(tool, (call, result, { expanded, width }) => lines | undefined)`. Check `api.ui.interactive`: headless runs return `false` from confirm and `undefined` from select.
- `api.session` — `id`; `append(kind, data)` / `entries(kind)` to persist state across resume (restore it in `session_start`); `messages()`; `replaceMessages(msgs)` (persisted); `inject(text, "next" | "now")` ("next" = with the next turn or prompt, "now" = also start a run if idle).
- `api.agent` — `run({ prompt, systemPrompt?, tools?, excludeTools?, model?, signal?, onProgress? })` → `{ text, messages, cause }`: a subagent with its own history, sharing tools, permissions and tool hooks; `complete({ system, messages, model?, maxTokens?, signal? })` → text: one model call without tools; `model()`, `tools()`.
- `api.permissions.addRules({ allow?, ask?, deny? })` — rules like `mytool`, `mytool(pattern*)`, `bash(git status*)`.
- `api.registerProvider(name, { api, baseUrl?, apiKeyEnv?, headers? }, implementation?)` — add an LLM endpoint; pass an implementation to add a new API format.
- `api.ready(promise)` — report background startup work, e.g. connecting to a server; headless runs wait for it before the first request, the TUI does not.
- `api.log(...)` — debug output, shown only with `SASACODE_DEBUG=1`.
- `api.cwd`, `api.name`, `api.version` (the host's plugin API version).

## 8. Test it

1. Unit-test with a stub API (`bun test-weather.ts`). The stub records tools, commands and hooks so you can call them directly:
   ```ts
   import plugin from "./weather.ts";
   const tools: any[] = [], commands: any[] = [], hooks: Record<string, Function[]> = {};
   const api: any = {
     version: "1.5.0", name: "weather", cwd: process.cwd(), settings: {},
     registerTool: (t: any) => tools.push(t), registerCommand: (c: any) => commands.push(c), registerProvider() {},
     on: (h: string, fn: Function) => (hooks[h] ??= []).push(fn), ready() {}, log() {},
     permissions: { addRules() {} },
     ui: { interactive: false, notify: console.log, confirm: async () => false, select: async () => undefined, setStatus() {}, registerToolRenderer() {} },
     session: { id: undefined, append() {}, entries: () => [], messages: () => [], replaceMessages() {}, inject() {} },
     agent: { model: () => ({ id: "test" }), tools: () => tools, run: async () => ({ text: "", messages: [], cause: "done" }), complete: async () => "" },
   };
   await plugin(api);
   console.log(await tools[0].execute({ city: "Tokyo" }, { cwd: process.cwd(), signal: new AbortController().signal }));
   ```
   Put the test file outside the plugins directory (every `.ts` directly in `plugins/` is loaded as a plugin).
2. End to end, headless: `sasacode --no-session -p "Use the weather tool for Tokyo"`. A plugin in the project's `.sasacode/plugins/` is only loaded headless with `--trust-project` (interactively, the user is asked once). Tools that are not auto-approved need `--permission auto` — use it only in a throwaway directory. Load errors are printed as warnings; `sasacode plugin list` shows what was found.

## 9. Checklist

- [ ] Name is unique and descriptive; description says what it returns.
- [ ] Schema has `required` and `additionalProperties: false`.
- [ ] Right `kind`; `paths` for file tools; no self-approval of risky tools.
- [ ] Description's first line says what the tool is for (it is all the model sees when tools are deferred).
- [ ] `stream_delta` handlers are synchronous and cheap; other hooks return only the fields they change.
- [ ] Failures return `errorResult` with a hint; large output is cut with a notice.
- [ ] `ctx.signal` honoured for anything slow.
- [ ] Secrets come from `process.env` or settings, never hard-coded.
- [ ] Tried once headless; told the user to restart sasacode to load it.
