import { Agent, PermissionPolicy, PluginHost } from "../packages/agent/src/index.ts";
import { executeTools } from "../packages/agent/src/tool-exec.ts";
import type { ToolResult, Plugin } from "@sasacode/plugin-api";

export async function loadPlugin(name: string, plugin: Plugin, cwd: string, settings: Record<string, unknown> = {}) {
  const agent = new Agent({ cwd, systemPrompt: "test", model: { id: "test", provider: "test", api: "replay", contextWindow: 10000, maxOutput: 1000 }, permissions: new PermissionPolicy("edits") });
  const host = new PluginHost({ agent, cwd, lang: "en", settings: () => settings });
  const notices: string[] = [];
  host.setUI({ interactive: false, notify: (s) => notices.push(s), confirm: async () => false, select: async () => undefined });
  if (!await host.load(name, plugin)) throw new Error(host.plugins.at(-1)?.error);
  const context = { cwd, signal: new AbortController().signal };
  return { agent, host, notices, context, async invoke(name: string, input: Record<string, unknown>, signal = context.signal) {
    let result!: ToolResult;
    await executeTools({ cwd, hooks: agent.hooks, events: agent.events, permissions: agent.permissions, approve: agent.approve,
      judge: async () => ({ safe: false, reason: "test" }), findTool: (n) => agent.getTools().find((t) => t.name === n), allTools: () => agent.getTools() },
      [{ type: "tool_call", id: "test-parent", name, input }], { truncated: false, signal, repairs: new Map(), onResult: () => {}, onToolResult: (r) => { result = r; } });
    return result;
  }, tool: (name: string) => {
    const tool = agent.getTools().find((t) => t.name === name);
    if (!tool) throw new Error(`Missing tool ${name}`);
    return tool;
  } };
}
