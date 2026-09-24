import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { type AssistantContent, type AssistantMessage, emptyUsage, type ModelInfo, registerApi, replayProvider } from "@sasacode/ai";
import type { Plugin, ToolDefinition } from "@sasacode/plugin-api";
import builtinTools from "@sasacode/tools";
import { Agent, type AgentEvent, PermissionPolicy, PluginHost, restore, SessionFile, searchTools, TOOL_SEARCH_NAME } from "../src/index.ts";

const dir = mkdtempSync(join(tmpdir(), "sasacode-plugins-"));
afterAll(() => rmSync(dir, { recursive: true, force: true }));
const model: ModelInfo = { id: "m", provider: "test", api: "replay", contextWindow: 100_000, maxOutput: 4000 };

const reply = (content: AssistantContent[], extra: Partial<AssistantMessage> = {}): AssistantMessage => ({
  role: "assistant",
  content,
  api: "replay",
  provider: "test",
  model: "m",
  usage: { ...emptyUsage(), input: 100 },
  stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop",
  timestamp: 0,
  ...extra,
});
const call = (id: string, name: string, input: Record<string, unknown>) => ({ type: "tool_call" as const, id, name, input });
const say = (text: string) => ({ type: "text" as const, text });

async function setup(script: AssistantMessage[], plugins: Record<string, Plugin>, opts: Partial<ConstructorParameters<typeof Agent>[0]> = {}) {
  const provider = replayProvider(script);
  registerApi("replay", provider);
  const agent = new Agent({ model, cwd: dir, systemPrompt: "base", permissions: new PermissionPolicy("auto"), ...opts });
  const host = new PluginHost({ agent, cwd: dir });
  await host.load("builtin-tools", builtinTools);
  for (const [n, p] of Object.entries(plugins)) await host.load(n, p);
  const events: AgentEvent[] = [];
  agent.events.on((e) => events.push(e));
  return { agent, host, provider, events };
}

test("hooks transform prompts, system prompt, requests, tool calls and results", async () => {
  const seen: string[] = [];
  const { agent, provider } = await setup([reply([call("1", "bash", { command: "echo original" })]), reply([say("ok")])], {
    p: (api) => {
      api.on("user_prompt", ({ content }) => ({ content: [...content, { type: "text", text: "(added by plugin)" }] }));
      api.on("system_prompt", ({ prompt }) => ({ prompt: `${prompt}\nextra instructions` }));
      api.on("before_request", ({ messages }) => {
        seen.push(`req:${messages.length}`);
      });
      api.on("tool_call", ({ args }) => ({ args: { ...args, command: "echo rewritten" } }));
      api.on("tool_result", ({ result }) => ({ result: { ...result, content: [...result.content, { type: "text", text: "lint: ok" }] } }));
    },
  });
  await agent.prompt("go");
  expect(provider.requests[0]!.system).toBe("base\nextra instructions");
  expect(JSON.stringify(provider.requests[0]!.messages[0])).toContain("(added by plugin)");
  const result = JSON.stringify(provider.requests[1]!.messages.at(-1));
  expect(result).toContain("rewritten");
  expect(result).toContain("lint: ok");
  expect(seen).toEqual(["req:1", "req:3"]);
});

test("user_prompt can consume input; tool_call can deny; failing handlers are reported, not fatal", async () => {
  const { agent, provider, events } = await setup([reply([call("1", "bash", { command: "rm -rf x" })]), reply([say("fine")])], {
    consume: (api) => api.on("user_prompt", ({ content }) => (JSON.stringify(content).includes("/local") ? { handled: true } : undefined)),
    guard: (api) => api.on("tool_call", ({ args }) => (String(args.command).startsWith("rm") ? { decision: "deny", reason: "no rm" } : undefined)),
    broken: (api) =>
      api.on("turn_end", () => {
        throw new Error("boom");
      }),
  });
  expect(await agent.prompt("/local thing")).toBe("done");
  expect(provider.requests).toHaveLength(0);
  expect(await agent.prompt("delete")).toBe("done");
  expect(JSON.stringify(provider.requests[1]!.messages.at(-1))).toContain("Permission denied (no rm)");
  expect(events).toContainEqual({ type: "plugin_error", plugin: "broken", hook: "turn_end", error: "boom" });
});

test("turn_end inject continues the loop; context_limit retry after replaceMessages", async () => {
  let injected = false;
  const big = reply([say("long")], { usage: { ...emptyUsage(), input: 99_000 } });
  const { agent, provider, events } = await setup([reply([say("first")]), big, reply([say("after compaction")])], {
    nudge: (api) =>
      api.on("turn_end", () => {
        if (injected) return;
        injected = true;
        return { inject: "please also do X" };
      }),
    compactor: (api) =>
      api.on("context_limit", () => {
        api.session.replaceMessages([{ role: "user", content: [{ type: "text", text: "summary" }], timestamp: 0 }]);
        return { retry: true };
      }),
  });
  expect(await agent.prompt("go")).toBe("done");
  expect(provider.requests).toHaveLength(3);
  expect(JSON.stringify(provider.requests[1]!.messages.at(-1))).toContain("please also do X");
  expect(provider.requests[2]!.messages).toHaveLength(1);
  expect(events.some((e) => e.type === "messages_replaced")).toBe(true);
});

test("plugin session entries and replaced messages survive a resume", async () => {
  const session = SessionFile.create(join(dir, "s"), dir);
  const { agent, host } = await setup([reply([say("hi")])], {}, { session });
  const api = host.api("mem");
  await agent.prompt("hello");
  api.session.append("note", { n: 1 });
  api.session.replaceMessages([{ role: "user", content: [{ type: "text", text: "compacted" }], timestamp: 0 }]);
  const { entries } = SessionFile.open(session.path);
  expect(restore(entries).messages).toEqual([{ role: "user", content: [{ type: "text", text: "compacted" }], timestamp: 0 }]);
  const fresh = new PluginHost({ agent, cwd: dir });
  fresh.setSessionEntries(entries);
  expect(fresh.api("mem").session.entries("note")).toEqual([{ n: 1 }]);
  expect(fresh.api("other").session.entries("note")).toEqual([]);
});

test("subagents run with their own history and share tools and tool hooks", async () => {
  let hookCalls = 0;
  const { agent, provider } = await setup(
    [
      reply([call("1", "spawn", {})]),
      reply([call("s1", "read", { path: "nothing.txt" })]), // subagent turn 1
      reply([say("sub report")]), // subagent turn 2
      reply([say("parent done")]),
    ],
    {
      counter: (api) => api.on("tool_call", () => void hookCalls++),
      spawner: (api) =>
        api.registerTool({
          name: "spawn",
          description: "spawn",
          parameters: { type: "object", properties: {} },
          kind: "other",
          execute: async () => {
            const r = await api.agent.run({ prompt: "investigate", excludeTools: ["spawn"] });
            return { content: [{ type: "text", text: r.text }] };
          },
        }),
    },
  );
  await agent.prompt("go");
  expect(provider.requests[1]!.messages).toHaveLength(1); // fresh history
  expect(provider.requests[1]!.tools.map((t) => t.name)).not.toContain("spawn");
  expect(JSON.stringify(provider.requests[3]!.messages.at(-1))).toContain("sub report");
  expect(hookCalls).toBe(2);
});

test("tool search defers plugin tools past the count threshold and loads them on demand", async () => {
  const many: Plugin = (api) => {
    for (let i = 0; i < 31; i++)
      api.registerTool({
        name: `mcp__srv__tool${i}`,
        description: i === 7 ? "Create a GitHub issue" : `Tool number ${i}`,
        parameters: { type: "object", properties: {} },
        kind: "other",
        execute: async () => ({ content: [{ type: "text", text: `ran ${i}` }] }),
      });
  };
  const { agent, provider } = await setup(
    [reply([call("1", TOOL_SEARCH_NAME, { query: "github issue" })]), reply([call("2", "mcp__srv__tool7", {})]), reply([say("done")])],
    { many },
  );
  await agent.prompt("file an issue");
  const names0 = provider.requests[0]!.tools.map((t) => t.name);
  expect(names0).toEqual(["read", "write", "edit", "bash", TOOL_SEARCH_NAME]);
  expect(provider.requests[0]!.tools.at(-1)!.description).toContain("mcp__srv__tool7: Create a GitHub issue");
  expect(provider.requests[1]!.tools.map((t) => t.name)).toContain("mcp__srv__tool7");
  expect(JSON.stringify(provider.requests[2]!.messages.at(-1))).toContain("ran 7");

  agent.toolSearch = { mode: "never" };
  expect(agent.requestTools()).toHaveLength(35);
  agent.toolSearch = { count: 100, percent: 50 };
  expect(agent.requestTools()).toHaveLength(35);
});

test("searchTools ranks names over descriptions and supports select:", () => {
  const t = (name: string, description: string) => ({ name, description }) as ToolDefinition;
  const tools = [t("slack_send", "send a message"), t("gh_issue", "create issue"), t("x", "mentions slack")];
  expect(searchTools("slack", tools).map((x) => x.name)).toEqual(["slack_send", "x"]);
  expect(searchTools("select:gh_issue,x", tools).map((x) => x.name)).toEqual(["gh_issue", "x"]);
});

test("disabled tools are not registered and registerCommand replaces by name", async () => {
  const agent = new Agent({ model, cwd: dir, systemPrompt: "s" });
  const host = new PluginHost({ agent, cwd: dir, disabledTools: ["bash"] });
  await host.load("builtin-tools", builtinTools);
  expect(agent.getTools().map((t) => t.name)).not.toContain("bash");
  host.api("a").registerCommand({ name: "x", description: "a", run() {} });
  host.api("b").registerCommand({ name: "x", description: "b", run() {} });
  expect(host.commands.map((c) => `${c.name}:${c.owner}`)).toEqual(["x:b"]);
  expect(await host.load("bad", () => Promise.reject(new Error("nope")))).toBe(false);
  expect(host.plugins.at(-1)).toEqual({ name: "bad", error: "nope" });
});
