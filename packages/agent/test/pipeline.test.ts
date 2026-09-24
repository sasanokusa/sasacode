import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { type AssistantContent, type AssistantMessage, emptyUsage, type ModelInfo, registerApi, replayProvider } from "@sasacode/ai";
import type { Plugin } from "@sasacode/plugin-api";
import builtinTools from "@sasacode/tools";
import { Agent, type AgentEvent, PermissionPolicy, PluginHost, SessionFile } from "../src/index.ts";

const dir = mkdtempSync(join(tmpdir(), "sasacode-pipeline-"));
afterAll(() => rmSync(dir, { recursive: true, force: true }));
writeFileSync(join(dir, "a.txt"), "hello\n");
const model: ModelInfo = { id: "m", provider: "t", api: "replay", contextWindow: 100_000, maxOutput: 4000 };
let clock = 1;
const reply = (content: AssistantContent[], extra: Partial<AssistantMessage> = {}): AssistantMessage => ({
  role: "assistant",
  content,
  api: "replay",
  provider: "t",
  model: "m",
  usage: emptyUsage(),
  stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop",
  timestamp: clock++,
  ...extra,
});
const say = (text: string) => ({ type: "text" as const, text });

async function setup(script: AssistantMessage[], plugins: Record<string, Plugin>, session?: SessionFile) {
  const provider = replayProvider(script);
  registerApi("replay", provider);
  const agent = new Agent({ model, cwd: dir, systemPrompt: "s", permissions: new PermissionPolicy("auto"), session });
  const host = new PluginHost({ agent, cwd: dir });
  await host.load("builtin-tools", builtinTools);
  for (const [n, p] of Object.entries(plugins)) await host.load(n, p);
  const events: AgentEvent[] = [];
  agent.events.on((e) => events.push(e));
  return { agent, provider, events };
}

/** A repair plugin that fixes one known mistake, standing in for the bundled tool-repair. */
const fixer: Plugin = (api) =>
  api.on("tool_call_raw", ({ name, input, rawInput }) => {
    if (name === "raed") return { name: "read", note: 'tool "raed" → "read"' };
    if (rawInput === "{path: 'a.txt'}") return { input: { path: "a.txt" }, note: "JSON: quoted keys" };
    if ("file" in input) return { input: { path: input.file }, note: 'argument "file" → "path"' };
  });

test("tool_call_raw repairs run before validation; history shows the repaired call; the model is told", async () => {
  const session = SessionFile.create(join(dir, "s1"), dir);
  const { agent, provider, events } = await setup(
    [
      reply([
        { type: "tool_call", id: "1", name: "raed", input: { path: "a.txt" } },
        { type: "tool_call", id: "2", name: "read", input: {}, rawInput: "{path: 'a.txt'}" },
        { type: "tool_call", id: "3", name: "read", input: { file: "a.txt" } },
      ]),
      reply([say("ok")]),
    ],
    { fixer },
    session,
  );
  await agent.prompt("go");
  const sent = provider.requests[1]!.messages;
  const calls = (sent[1] as AssistantMessage).content;
  expect(calls).toEqual([
    { type: "tool_call", id: "1", name: "read", input: { path: "a.txt" } },
    { type: "tool_call", id: "2", name: "read", input: { path: "a.txt" } },
    { type: "tool_call", id: "3", name: "read", input: { path: "a.txt" } },
  ]);
  const results = sent.slice(2);
  expect(results.every((r) => r.role === "tool" && !r.isError)).toBe(true);
  expect(JSON.stringify(results[0])).toContain('[harness repaired this call: tool \\"raed\\" → \\"read\\"]');
  expect(JSON.stringify(results[2])).toContain("hello");
  const log = readFileSync(session.path, "utf8");
  expect(log).toContain('"type":"tool_repair"');
  expect(log).toContain('"rawInput":"{path: \'a.txt\'}"');
  expect(events.filter((e) => e.type === "tool_repaired")).toHaveLength(3);
});

test("calls with signed reasoning are repaired for execution but not rewritten; truncated calls are never repaired", async () => {
  const { agent, provider } = await setup(
    [
      reply([{ type: "thinking", thinking: "t", signature: "sig" }, { type: "tool_call", id: "1", name: "raed", input: { path: "a.txt" } }]),
      reply([{ type: "tool_call", id: "2", name: "raed", input: { path: "a.txt" } }], { stopReason: "max_tokens" }),
      reply([say("done")]),
    ],
    { fixer },
  );
  await agent.prompt("go");
  const first = provider.requests[1]!.messages;
  expect((first[1] as AssistantMessage).content[1]).toMatchObject({ name: "raed" });
  expect(first[2]).toMatchObject({ role: "tool", toolName: "read", isError: false });
  const second = provider.requests[2]!.messages.at(-1)!;
  expect(JSON.stringify(second)).toContain("hit max_tokens");
});

test("stream_delta can stop a response; assistant_message sees who stopped it and can continue the loop", async () => {
  const seen: string[] = [];
  const { agent, provider } = await setup([reply([say("loop loop loop"), say("never streamed")]), reply([say("fine now")])], {
    guard: (api) => {
      api.on("stream_delta", ({ kind, text }) => {
        seen.push(`${kind}:${text}`);
        if (text.includes("loop loop")) return { stop: "repeating" };
      });
      api.on("assistant_message", ({ message, stopped }) => {
        if (!stopped) return;
        expect(stopped).toEqual({ plugin: "guard", reason: "repeating" });
        expect(message.stopReason).toBe("stopped");
        return { message: { ...message, content: [say("loop")] }, inject: "do not repeat" };
      });
    },
  });
  expect(await agent.prompt("go")).toBe("done");
  // The second block of the stopped response is never streamed; the next response is watched again.
  expect(seen).toEqual(["text:loop loop loop", "text:fine now"]);
  const sent = provider.requests[1]!.messages;
  expect(sent[1]).toMatchObject({ role: "assistant", content: [{ text: "loop" }], stopReason: "stopped" });
  expect(sent[2]).toMatchObject({ role: "user", content: [{ text: "do not repeat" }] }); // not "user interrupted"
});

test("assistant_message retry discards a response and asks again (bounded)", async () => {
  let n = 0;
  const { agent, provider, events } = await setup([reply([say("bad")]), reply([say("bad")]), reply([say("bad")]), reply([say("good")])], {
    picky: (api) => api.on("assistant_message", () => (n++ < 5 ? { retry: true } : undefined)),
  });
  await agent.prompt("go");
  expect(provider.requests).toHaveLength(3); // first try + 2 retries, then the response is kept
  expect(events.filter((e) => e.type === "message_discarded")).toHaveLength(2);
  expect(agent.messages.filter((m) => m.role === "assistant")).toHaveLength(1);
});

test("stream_delta handlers must be synchronous; before_request can set sampling", async () => {
  const { agent, provider, events } = await setup([reply([say("hi")])], {
    slow: (api) => api.on("stream_delta", (async () => ({ stop: "x" })) as never),
    sampler: (api) => api.on("before_request", () => ({ sampling: { temperature: 0.2, extraBody: { repetition_penalty: 1.1 } } })),
  });
  await agent.prompt("go");
  expect(agent.messages.at(-1)).toMatchObject({ stopReason: "stop" });
  expect(events.some((e) => e.type === "plugin_error" && e.error.includes("must be synchronous"))).toBe(true);
  expect(provider.requests[0]!.sampling).toEqual({ temperature: 0.2, extraBody: { repetition_penalty: 1.1 } });
});
