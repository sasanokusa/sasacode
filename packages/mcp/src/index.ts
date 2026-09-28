import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { getDefaultEnvironment, StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { ToolListChangedNotificationSchema } from "@modelcontextprotocol/sdk/types.js";
import { errorResult, type Plugin, saveOutput, type PluginAPI, type ToolResult, type UserContent } from "@sasacode/plugin-api";

export type McpServerConfig = (
  | { command: string; args?: string[]; env?: Record<string, string>; cwd?: string }
  | { url: string; headers?: Record<string, string> }
) & {
  disabled?: boolean;
  /** Send this server's tool definitions even when tool search defers the rest. */
  alwaysLoad?: boolean;
  /** Per-call timeout in ms (default 10 minutes). */
  timeout?: number;
  /** "plain" registers tools under their own names instead of mcp__<server>__<tool>. */
  toolNames?: "prefixed" | "plain";
};

const MAX_OUTPUT = 100_000;
/** A server that fell over is reconnected this many times (with growing gaps), then left to `/mcp reconnect`. */
const MAX_RECONNECTS = 5;
const RECONNECT_DELAY_MS = 2_000;
const MAX_RECONNECT_DELAY_MS = 30_000;

export interface ServerState {
  name: string;
  status: "connecting" | "connected" | "failed" | "closed";
  error?: string;
  tools: string[];
  client?: Client;
  stderr: string;
  /** Tool names on the model's list, so a tool the server drops can be taken off again. */
  registered: string[];
  /** Reconnects since the last successful connection: capped, reset by connecting or `/mcp reconnect`. */
  attempts: number;
  /** The pending reconnect, if any. */
  timer?: ReturnType<typeof setTimeout>;
}

/** How to connect a server again (`/mcp reconnect`), kept out of the state others read. */
const reconnects = new WeakMap<ServerState, () => void>();

/** `${VAR}` in env values and headers comes from the environment, so tokens need not sit in config files. */
function expand(v: string): string {
  return v.replace(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g, (_, k) => process.env[k] ?? "");
}
const expandAll = (o: Record<string, string> = {}) => Object.fromEntries(Object.entries(o).map(([k, v]) => [k, expand(v)]));

/** MCP tool names become mcp__<server>__<tool>, limited to what every provider accepts. */
export function toolName(server: string, tool: string): string {
  return `mcp__${server}__${tool}`.replace(/[^a-zA-Z0-9_-]/g, "_").slice(0, 64);
}

/**
 * Connect servers in the background and register their tools as they come up. Closes them when
 * sasacode exits (session_end other than a session switch). Other plugins (e.g. browsr) reuse this
 * to wrap a specific MCP server. A server that drops on its own is reconnected, a few times.
 */
export function connectServers(
  api: PluginAPI,
  servers: Record<string, McpServerConfig>,
  onChange?: (states: Map<string, ServerState>) => void,
): Map<string, ServerState> {
  const states = new Map<string, ServerState>();
  const changed = () => onChange?.(states);
  let exiting = false;
  // `plugins.settings.mcp.reconnectDelayMs` shortens the gaps (the count and the ceiling stay).
  const gap = (api.settings.reconnectDelayMs as number | undefined) ?? RECONNECT_DELAY_MS;
  const open = (state: ServerState, cfg: McpServerConfig) =>
    connect(api, state.name, cfg, state, changed, () => {
      // The gap grows with every attempt; after the last one the server stays down until the user
      // asks (`/mcp reconnect`), so a command that is gone cannot spin retries forever.
      if (exiting) return;
      if (state.attempts >= MAX_RECONNECTS) {
        api.ui.notify(
          api.ui.lang === "en"
            ? `MCP server ${state.name}: gave up reconnecting; \`/mcp reconnect ${state.name}\` tries again`
            : `MCP サーバー ${state.name}: 再接続をあきらめました。\`/mcp reconnect ${state.name}\` でもう一度試せます`,
          "warning",
        );
        return;
      }
      // Never hold the process open: a pending reconnect is background work.
      state.timer = setTimeout(() => void open(state, cfg), Math.min(gap * 2 ** state.attempts++, MAX_RECONNECT_DELAY_MS));
      state.timer.unref?.();
    });
  for (const [name, cfg] of Object.entries(servers)) {
    if (cfg.disabled) continue;
    const state: ServerState = { name, status: "connecting", tools: [], registered: [], stderr: "", attempts: 0 };
    states.set(name, state);
    reconnects.set(state, () => {
      clearTimeout(state.timer);
      state.attempts = 0;
      state.error = undefined;
      state.status = "closed"; // replacing the client is expected: no crash, no retry
      void state.client?.close().catch(() => {});
      void open(state, cfg);
    });
    api.ready(open(state, cfg));
  }
  changed();
  // Servers outlive /clear, /resume and /fork: nothing would connect them again for the next session.
  api.on("session_end", async ({ reason }) => {
    if (reason === "switch") return;
    exiting = true;
    await Promise.all(
      [...states.values()].map((s) => {
        clearTimeout(s.timer);
        s.status = "closed"; // expected shutdown, not a crash
        return s.client?.close().catch(() => {});
      }),
    );
  });
  return states;
}

/** The MCP adapter is an ordinary plugin: servers connect in the background and register tools as they come up. */
export function createMcpPlugin(servers: Record<string, McpServerConfig>): Plugin {
  return (api) => {
    const states = connectServers(api, servers, (all) => {
      if (!all.size) return;
      const ok = [...all.values()].filter((s) => s.status === "connected").length;
      api.ui.setStatus("servers", `MCP ${ok}/${all.size}`);
    });

    api.registerCommand({
      name: "mcp",
      description: api.ui.lang === "en" ? "MCP servers: status and reconnecting" : "MCP サーバーの接続状態と再接続",
      argumentHint: "[reconnect [name]]",
      complete: (prefix: string) => {
        const words = ["reconnect", ...states.keys()].filter((w) => w.startsWith(prefix ?? ""));
        return words.map((value) => ({ value, label: value }));
      },
      run({ args }: { args?: string }) {
        if (!states.size) {
          api.ui.notify(api.ui.lang === "en" ? "No MCP servers are configured (mcpServers in config.json)" : "MCP サーバーは設定されていません（config.json の mcpServers）");
          return;
        }
        const [sub, target] = (args ?? "").trim().split(/\s+/);
        if (sub === "reconnect") {
          const picked = target ? states.get(target) : undefined;
          if (target && !picked) {
            const known = [...states.keys()].join(", ");
            api.ui.notify(api.ui.lang === "en" ? `No MCP server named ${target} (${known})` : `その名前の MCP サーバーはありません: ${target}（${known}）`, "warning");
            return;
          }
          const now = picked ? [picked] : [...states.values()];
          for (const s of now) reconnects.get(s)?.();
          const names = now.map((s) => s.name).join(", ");
          api.ui.notify(api.ui.lang === "en" ? `Reconnecting: ${names}` : `再接続します: ${names}`);
          return;
        }
        if (sub) {
          api.ui.notify(api.ui.lang === "en" ? "usage: /mcp [reconnect [name]]" : "使い方: /mcp [reconnect [name]]", "warning");
          return;
        }
        const lines = [...states.values()].map(
          (s) =>
            `${s.status === "connected" ? "●" : "○"} ${s.name}: ${s.status}${s.error ? ` (${s.error})` : ""}${s.tools.length ? ` · ${s.tools.length} tools` : ""}`,
        );
        api.ui.notify(lines.join("\n"));
      },
    });
  };
}

/** `dropped`: the connection was lost or never came up, so the caller may schedule a reconnect. */
async function connect(api: PluginAPI, name: string, cfg: McpServerConfig, state: ServerState, onChange: () => void, dropped: () => void) {
  const client = new Client({ name: "sasacode", version: api.version });
  state.status = "connecting";
  try {
    const transport =
      "command" in cfg
        ? new StdioClientTransport({
            command: cfg.command,
            args: cfg.args,
            env: { ...getDefaultEnvironment(), ...expandAll(cfg.env) },
            cwd: cfg.cwd ?? api.cwd,
            stderr: "pipe",
          })
        : new StreamableHTTPClientTransport(new URL(expand(cfg.url)), { requestInit: { headers: expandAll(cfg.headers) } });
    if (transport instanceof StdioClientTransport)
      // Shown in notices: the server's escape sequences must not reach the terminal.
      transport.stderr?.on("data", (d: Buffer) => (state.stderr = (state.stderr + d.toString().replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, "")).slice(-2000)));
    client.onclose = () => {
      // A close we asked for (exit, /mcp reconnect) has already moved the state on.
      if (state.status !== "connected") return;
      state.status = "closed";
      state.error = "server exited";
      api.ui.notify(api.ui.lang === "en" ? `MCP server ${name} disconnected` : `MCP サーバー ${name} の接続が切れました`, "warning");
      onChange();
      dropped();
    };
    await client.connect(transport);
    state.client = client;
    state.status = "connected";
    state.attempts = 0;
    await registerTools(api, name, cfg, client, state);
    if (client.getServerCapabilities()?.tools?.listChanged)
      client.setNotificationHandler(ToolListChangedNotificationSchema, () => {
        // The list changed in some way: sync it, so a tool the server dropped leaves the model's list.
        void registerTools(api, name, cfg, client, state).catch((e) =>
          api.ui.notify(
            api.ui.lang === "en"
              ? `MCP server ${name}: tool list update failed: ${e instanceof Error ? e.message : e}`
              : `MCP サーバー ${name}: ツール一覧を更新できませんでした: ${e instanceof Error ? e.message : e}`,
            "warning",
          ),
        );
      });
    if (client.getServerCapabilities()?.prompts) await registerPrompts(api, name, client);
  } catch (e) {
    state.status = "failed";
    state.error = (e instanceof Error ? e.message : String(e)) + (state.stderr ? ` — ${state.stderr.trim().split("\n").slice(-1)[0]}` : "");
    // Repeats are the reconnect loop's own business; one notice is enough to explain a dead server.
    if (!state.attempts)
      api.ui.notify(api.ui.lang === "en" ? `MCP server ${name} failed to start: ${state.error}` : `MCP サーバー ${name} を起動できませんでした: ${state.error}`, "warning");
    dropped();
  }
  onChange();
}

/** Brings the model's tool list in line with what the server offers right now (also on list changes). */
async function registerTools(api: PluginAPI, server: string, cfg: McpServerConfig, client: Client, state: ServerState) {
  const tools = [];
  let cursor: string | undefined;
  do {
    const page = await client.listTools(cursor ? { cursor } : undefined);
    tools.push(...page.tools);
    cursor = page.nextCursor;
  } while (cursor);
  state.tools = tools.map((t) => t.name);
  // A tool the server no longer offers must come off the model's list: it would only keep failing.
  const names = tools.map((t) => (cfg.toolNames === "plain" ? t.name : toolName(server, t.name)));
  for (const gone of state.registered) if (!names.includes(gone)) api.unregisterTool(gone);
  state.registered = names;
  for (const t of tools) {
    api.registerTool({
      name: cfg.toolNames === "plain" ? t.name : toolName(server, t.name),
      description: t.description ?? t.title ?? t.name,
      parameters: t.inputSchema as Record<string, unknown>,
      // Server annotations are hints from outside our trust boundary, so MCP tools always go through approval.
      kind: "other",
      concurrent: t.annotations?.readOnlyHint === true,
      alwaysLoad: cfg.alwaysLoad,
      async execute(args, ctx) {
        if (state.status !== "connected") return errorResult(`MCP server ${server} is not connected (${state.error ?? state.status}).`);
        try {
          const r = await client.callTool({ name: t.name, arguments: args }, undefined, {
            signal: ctx.signal,
            timeout: cfg.timeout ?? 600_000,
            resetTimeoutOnProgress: true,
          });
          return convertResult(r as CallResult, server, t.name);
        } catch (e) {
          return errorResult(`MCP ${server}/${t.name} failed: ${e instanceof Error ? e.message : String(e)}`);
        }
      },
    });
  }
}

async function registerPrompts(api: PluginAPI, server: string, client: Client) {
  const { prompts } = await client.listPrompts();
  for (const p of prompts) {
    const argNames = (p.arguments ?? []).map((a) => a.name);
    api.registerCommand({
      name: toolName(server, p.name),
      description: p.description ?? `MCP prompt from ${server}`,
      argumentHint: argNames.join(" "),
      async run({ args }) {
        // "key=value" pairs, or plain words filling the arguments in order.
        const values: Record<string, string> = {};
        const words = args.match(/(?:[^\s"]+|"[^"]*")+/g) ?? [];
        words.forEach((w, i) => {
          const m = /^(\w+)=(.*)$/s.exec(w);
          if (m) values[m[1]!] = m[2]!.replace(/^"|"$/g, "");
          else if (argNames[i]) values[argNames[i]!] = w.replace(/^"|"$/g, "");
        });
        const r = await client.getPrompt({ name: p.name, arguments: values });
        const text = r.messages
          .map((m) => (m.content.type === "text" ? m.content.text : m.content.type === "resource" && "text" in m.content.resource ? m.content.resource.text : ""))
          .filter(Boolean)
          .join("\n\n");
        api.session.inject(text, "now");
      },
    });
  }
}

type CallResult = {
  content?: Array<
    | { type: "text"; text: string }
    | { type: "image"; data: string; mimeType: string }
    | { type: "audio"; data: string; mimeType: string }
    | { type: "resource"; resource: { uri: string; text?: string; blob?: string; mimeType?: string } }
    | { type: "resource_link"; uri: string; name?: string }
  >;
  structuredContent?: unknown;
  isError?: boolean;
  toolResult?: unknown;
};

export function convertResult(r: CallResult, server: string, tool: string): ToolResult {
  const content: UserContent[] = [];
  for (const c of r.content ?? []) {
    if (c.type === "text") content.push({ type: "text", text: c.text });
    else if (c.type === "image") content.push({ type: "image", mediaType: c.mimeType, data: c.data });
    else if (c.type === "resource")
      content.push({ type: "text", text: c.resource.text ?? `[resource ${c.resource.uri} (${c.resource.mimeType ?? "binary"})]` });
    else if (c.type === "resource_link") content.push({ type: "text", text: `[resource link ${c.name ?? ""} ${c.uri}]` });
    else content.push({ type: "text", text: `[${c.type} content omitted]` });
  }
  if (!content.length && r.structuredContent !== undefined) content.push({ type: "text", text: JSON.stringify(r.structuredContent) });
  if (!content.length && r.toolResult !== undefined) content.push({ type: "text", text: JSON.stringify(r.toolResult) });
  // A4: cut only at an explicit limit, and say where the rest is.
  const total = content.reduce((n, c) => n + (c.type === "text" ? c.text.length : 0), 0);
  if (total > MAX_OUTPUT) {
    const full = content.map((c) => (c.type === "text" ? c.text : "")).join("\n");
    const file = saveOutput(`mcp-${server}-${tool}`.replace(/[^\w.-]/g, "_"), full);
    const images = content.filter((c) => c.type === "image");
    return {
      content: [{ type: "text", text: `${full.slice(0, MAX_OUTPUT)}\n[Output truncated at ${MAX_OUTPUT} of ${total} characters. Full output saved to ${file}]` }, ...images],
      isError: r.isError,
    };
  }
  return { content: content.length ? content : [{ type: "text", text: "(no content)" }], isError: r.isError };
}
