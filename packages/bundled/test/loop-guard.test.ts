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

async function run(script: AssistantMessage[], onCause?: (cause: string) => void) {
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
  const cause = await agent.prompt("go");
  onCause?.(cause);
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

test("the same call twice in one response runs once", async () => {
  const same = (id: string) => ({ type: "tool_call" as const, id, name: "check", input: { x: 1 } });
  const results = await run([reply([same("1"), same("2"), same("3")]), reply([{ type: "text", text: "done" }])]);
  expect(results[0]).toContain("same output");
  expect(results[1]).toContain("appears earlier in this response");
  expect(results[2]).toContain("appears earlier in this response");
});

test("a short cycle (A, B, A, B, …) is noted and then blocked", async () => {
  const script = Array.from({ length: 10 }, (_, i) => call(String(i), "check", { x: i === 9 ? 0 : i % 2 }));
  const results = await run([...script, reply([{ type: "text", text: "done" }])]);
  expect(results[5]).toContain("The same 2 tool calls have now repeated 3 times");
  expect(results[8]).toContain("loop-guard: the same 2 calls have repeated 4 times");
  // A refusal does not reset the count: asking for the refused call again is refused again.
  expect(results[9]).toContain("loop-guard");
});

test("a call rejected before running (invalid arguments) counts as a repeated error", async () => {
  const results = await run([...Array.from({ length: 3 }, (_, i) => call(String(i), "check", { x: "nope" })), reply([{ type: "text", text: "done" }])]);
  expect(results[0]).toContain("Invalid arguments");
  expect(results[2]).toContain("[harness] This is call 3 of check");
});

test("repeating a check after a change in the same response is allowed", async () => {
  const results = await run([
    reply([
      { type: "tool_call", id: "1", name: "check", input: { x: 1 } },
      { type: "tool_call", id: "2", name: "fix", input: {} },
      { type: "tool_call", id: "3", name: "check", input: { x: 1 } },
    ]),
    reply([{ type: "text", text: "done" }]),
  ]);
  expect(results.join()).not.toContain("loop-guard");
});

test("once a cycle is stopped, all of its calls stay refused until a file changes", async () => {
  // A B A B A B A B, then the model keeps alternating: every later step is refused, not just every other one.
  const script = Array.from({ length: 12 }, (_, i) => call(String(i), "check", { x: i % 2 }));
  const results = await run([...script, call("f", "fix", {}), call("a", "check", { x: 0 }), reply([{ type: "text", text: "done" }])]);
  for (const r of results.slice(8, 12)) expect(r).toContain("loop-guard");
  expect(results[13]).toContain("same output"); // a file changed: the calls are allowed again
});

test("a run in which no tool call can run for 5 turns is stopped by the plugin", async () => {
  let cause = "";
  const bad = (i: number) => call(String(i), "no_such_tool", {});
  const results = await run(Array.from({ length: 20 }, (_, i) => bad(i)), (c) => (cause = c));
  expect(cause).toBe("stopped");
  expect(results).toHaveLength(5);
});
