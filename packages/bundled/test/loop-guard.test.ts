import { expect, test } from "bun:test";
import { Agent, PermissionPolicy, PluginHost } from "@sasacode/agent";
import { type AssistantMessage, emptyUsage, registerApi, replayProvider } from "@sasacode/ai";
import type { Plugin } from "@sasacode/plugin-api";
import { bundledPlugins } from "../src/index.ts";

const reply = (content: AssistantMessage["content"]): AssistantMessage => ({
  role: "assistant", content, api: "replay", provider: "t", model: "m", usage: emptyUsage(),
  stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop", timestamp: 0,
});
const call = (id: string, name: string, input: Record<string, unknown>) => reply([{ type: "tool_call", id, name, input }]);

async function run(script: AssistantMessage[]) {
  const provider = replayProvider(script);
  registerApi("replay", provider);
  let n = 0;
  const tools: Plugin = (api) => {
    api.registerTool({ name: "check", description: "", parameters: { type: "object", properties: { x: { type: "number" } } }, kind: "read", execute: async () => ({ content: [{ type: "text", text: "same output" }] }) });
    api.registerTool({ name: "fix", description: "", parameters: { type: "object", properties: {} }, kind: "edit", paths: () => [], execute: async () => ({ content: [{ type: "text", text: `changed ${++n}` }] }) });
  };
  const agent = new Agent({ model: { id: "m", provider: "t", api: "replay", contextWindow: 1e5, maxOutput: 1e3 }, cwd: import.meta.dir, systemPrompt: "", permissions: new PermissionPolicy("auto") });
  const host = new PluginHost({ agent, cwd: agent.cwd });
  await host.load("tools", tools);
  await host.load("loop-guard", bundledPlugins["loop-guard"]!);
  await agent.prompt("go");
  return agent.messages.filter((m) => m.role === "tool").map((m) => JSON.stringify(m.content));
}

test("the same call with the same result: noted on the 3rd, blocked on the 5th", async () => {
  const results = await run([...Array.from({ length: 5 }, (_, i) => call(String(i), "check", { x: 1 })), reply([{ type: "text", text: "done" }])]);
  expect(results[1]).not.toContain("[harness]");
  expect(results[2]).toContain("call 3 of check with the same arguments");
  expect(results[4]).toContain("loop-guard: the same check call returned the same result 4 times");
});

test("a file change in between resets the count; different arguments are not a loop", async () => {
  const results = await run([
    call("1", "check", { x: 1 }),
    call("2", "check", { x: 1 }),
    call("3", "fix", {}),
    call("4", "check", { x: 1 }),
    call("5", "check", { x: 2 }),
    reply([{ type: "text", text: "done" }]),
  ]);
  expect(results.join()).not.toContain("[harness]");
  expect(results.join()).not.toContain("loop-guard");
});
