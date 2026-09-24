// The tool-authoring skill is what the model reads before writing a plugin, so its code examples
// are run here against the real host: if the API drifts, this fails.
import { expect, test } from "bun:test";
import { Agent, PermissionPolicy, PluginHost } from "@sasacode/agent";
import { type AssistantMessage, emptyUsage, registerApi, replayProvider } from "@sasacode/ai";
import * as pluginApi from "@sasacode/plugin-api";
import builtinTools, { bashTool } from "@sasacode/tools";
import { BUILTIN_SKILLS } from "../src/index.ts";

const doc = BUILTIN_SKILLS["tool-authoring"]!;
const transpiler = new Bun.Transpiler({ loader: "ts" });
const AsyncFunction = Object.getPrototypeOf(async () => {}).constructor;

/** The first ```ts block after `marker`. */
function block(marker: string): string {
  const at = doc.indexOf(marker);
  expect(at).toBeGreaterThan(-1);
  const m = /```ts\n([\s\S]*?)```/.exec(doc.slice(at));
  return m![1]!.replace(/^\s{3}/gm, "");
}

/** Run a hook example as the body of a plugin. */
function examplePlugin(marker: string): pluginApi.Plugin {
  const js = transpiler.transformSync(block(marker));
  return (api) => new Function("api", "text", js)(api, pluginApi.text);
}

const reply = (content: AssistantMessage["content"], ts: number): AssistantMessage => ({
  role: "assistant",
  content,
  api: "replay",
  provider: "t",
  model: "m",
  usage: emptyUsage(),
  stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop",
  timestamp: ts,
});

async function agentWith(script: AssistantMessage[], plugins: Record<string, pluginApi.Plugin>, mode: "auto" | "edits" = "auto") {
  const provider = replayProvider(script);
  registerApi("replay", provider);
  const agent = new Agent({
    model: { id: "m", provider: "t", api: "replay", contextWindow: 1e5, maxOutput: 1e3 },
    cwd: import.meta.dir,
    systemPrompt: "",
    permissions: new PermissionPolicy(mode),
  });
  const host = new PluginHost({ agent, cwd: agent.cwd });
  await host.load("builtin-tools", builtinTools);
  for (const [n, p] of Object.entries(plugins)) await host.load(n, p);
  if (script.length) await agent.prompt("go");
  return { agent, provider, host };
}

test("the stub API in 'Test it' runs a plugin", async () => {
  const code = transpiler.transformSync(block("Unit-test with a stub API").replace(/^import plugin from .*$/m, ""));
  const logged: unknown[] = [];
  const plugin: pluginApi.Plugin = (api) =>
    api.registerTool({
      name: "weather",
      description: "d",
      parameters: { type: "object", properties: {} },
      kind: "read",
      execute: async (a) => ({ content: [pluginApi.text(`sunny in ${a.city}`)] }),
    });
  const origLog = console.log;
  console.log = (...a: unknown[]) => logged.push(...a);
  try {
    await new AsyncFunction("plugin", code)(plugin);
  } finally {
    console.log = origLog;
  }
  expect(logged).toEqual([{ content: [{ type: "text", text: "sunny in Tokyo" }] }]);
});

test("the tool_call_raw example repairs an argument alias and the model is told", async () => {
  const { provider } = await agentWith(
    [reply([{ type: "tool_call", id: "1", name: "read", input: { filename: "authoring-doc.test.ts", limit: 1 } }], 1), reply([{ type: "text", text: "ok" }], 2)],
    { alias: examplePlugin("Example — accept an argument alias") },
  );
  const result = JSON.stringify(provider.requests[1]!.messages.at(-1));
  expect(result).toContain('harness repaired this call: argument \\"filename\\" → \\"path\\"');
  expect(result).toContain("The tool-authoring skill");
});

test("the stream_delta + assistant_message example stops a long answer and nudges", async () => {
  const { provider } = await agentWith([reply([{ type: "text", text: "x".repeat(25_000) }], 1), reply([{ type: "text", text: "short" }], 2)], {
    long: examplePlugin("Example — stop a response that goes on too long"),
  });
  expect(provider.requests[1]!.messages.at(-2)).toMatchObject({ role: "assistant", stopReason: "stopped" });
  expect(JSON.stringify(provider.requests[1]!.messages.at(-1))).toContain("cut off for length");
});

test("a plugin's allow replaces the mode default but not the user's deny rules", async () => {
  const allowAll: pluginApi.Plugin = (api) => api.on("tool_call", () => ({ decision: "allow", reason: "plugin says ok" }));
  const { agent } = await agentWith([], { allowAll }, "edits");
  agent.permissions.addRules({ deny: ["bash(sudo *)"] });
  const denied = await agent.permissions.check({ tool: bashTool, args: { command: "sudo ls" }, cwd: agent.cwd });
  expect(denied).toMatchObject({ decision: "deny", source: "rule" });
  const { provider } = await agentWith(
    [
      reply([{ type: "tool_call", id: "1", name: "bash", input: { command: "echo hi" } }, { type: "tool_call", id: "2", name: "bash", input: { command: "sudo ls" } }], 1),
      reply([{ type: "text", text: "done" }], 2),
    ],
    { allowAll, deny: (api) => api.permissions.addRules({ deny: ["bash(sudo *)"] }) },
    "edits",
  );
  const [ok, blocked] = provider.requests[1]!.messages.slice(-2);
  expect(JSON.stringify(ok)).toContain("hi"); // edits mode would have asked; the plugin allowed it
  expect(JSON.stringify(blocked)).toContain("Permission denied (rule deny: bash(sudo *))");
});
