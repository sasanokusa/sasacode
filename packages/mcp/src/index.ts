import { writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { getDefaultEnvironment, StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { ToolListChangedNotificationSchema } from "@modelcontextprotocol/sdk/types.js";
import { errorResult, type Plugin, type PluginAPI, type ToolResult, type UserContent } from "@sasacode/plugin-api";

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

export interface ServerState {
  name: string;
  status: "connecting" | "connected" | "failed" | "closed";
  error?: string;
  tools: string[];
  client?: Client;
  stderr: string;
}

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
 * Connect servers in the background and register their tools as they come up. Closes them on
 * session_end. Other plugins (e.g. browsr) reuse this to wrap a specific MCP server.
 */
export function connectServers(
  api: PluginAPI,
  servers: Record<string, McpServerConfig>,
  onChange?: (states: Map<string, ServerState>) => void,
): Map<string, ServerState> {
  const states = new Map<string, ServerState>();
  const changed = () => onChange?.(states);
  for (const [name, cfg] of Object.entries(servers)) {
    if (cfg.disabled) continue;
    const state: ServerState = { name, status: "connecting", tools: [], stderr: "" };
    states.set(name, state);
    api.ready(connect(api, name, cfg, state, changed));
  }
  changed();
  api.on("session_end", async () => {
    await Promise.all(
      [...states.values()].map((s) => {
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
      description: "MCP サーバーの接続状態",
      run() {
        if (!states.size) {
          api.ui.notify("MCP サーバーは設定されていません（config.json の mcpServers）");
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

async function connect(api: PluginAPI, name: string, cfg: McpServerConfig, state: ServerState, onChange: () => void) {
  const client = new Client({ name: "sasacode", version: api.version });
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
      transport.stderr?.on("data", (d: Buffer) => (state.stderr = (state.stderr + d.toString()).slice(-2000)));
    client.onclose = () => {
      if (state.status === "connected") {
        state.status = "closed";
        state.error = "server exited";
        api.ui.notify(`MCP server ${name} disconnected`, "warning");
        onChange();
      }
    };
    await client.connect(transport);
    state.client = client;
    state.status = "connected";
    await registerTools(api, name, cfg, client, state);
    if (client.getServerCapabilities()?.tools?.listChanged)
      client.setNotificationHandler(ToolListChangedNotificationSchema, () => registerTools(api, name, cfg, client, state));
    if (client.getServerCapabilities()?.prompts) await registerPrompts(api, name, client);
  } catch (e) {
    state.status = "failed";
    state.error = (e instanceof Error ? e.message : String(e)) + (state.stderr ? ` — ${state.stderr.trim().split("\n").slice(-1)[0]}` : "");
    api.ui.notify(`MCP server ${name} failed to start: ${state.error}`, "warning");
  }
  onChange();
}

async function registerTools(api: PluginAPI, server: string, cfg: McpServerConfig, client: Client, state: ServerState) {
  const tools = [];
  let cursor: string | undefined;
  do {
    const page = await client.listTools(cursor ? { cursor } : undefined);
    tools.push(...page.tools);
    cursor = page.nextCursor;
  } while (cursor);
  state.tools = tools.map((t) => t.name);
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
    const file = join(tmpdir(), `sasacode-mcp-${server}-${tool}-${Date.now()}.txt`.replace(/[^\w./-]/g, "_"));
    writeFileSync(file, full);
    const images = content.filter((c) => c.type === "image");
    return {
      content: [{ type: "text", text: `${full.slice(0, MAX_OUTPUT)}\n[Output truncated at ${MAX_OUTPUT} of ${total} characters. Full output saved to ${file}]` }, ...images],
      isError: r.isError,
    };
  }
  return { content: content.length ? content : [{ type: "text", text: "(no content)" }], isError: r.isError };
}
