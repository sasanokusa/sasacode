import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  type AssistantContent,
  type AssistantMessage,
  emptyUsage,
  type ModelInfo,
  type Provider,
  registerApi,
  replayProvider,
} from "@sasacode/ai";
import builtinTools, { bashTool, editTool, readTool, writeTool } from "@sasacode/tools";
import { Agent, type AgentEvent, PermissionPolicy, PluginHost, restore, SessionFile } from "../src/index.ts";

const dir = mkdtempSync(join(tmpdir(), "sasacode-agent-"));
afterAll(() => rmSync(dir, { recursive: true, force: true }));

const model: ModelInfo = { id: "m", provider: "test", api: "replay", contextWindow: 100_000, maxOutput: 4000 };
const tools = [readTool, writeTool, editTool, bashTool];

function reply(content: AssistantContent[], extra: Partial<AssistantMessage> = {}): AssistantMessage {
  return {
    role: "assistant",
    content,
    api: "replay",
    provider: "test",
    model: "m",
    usage: { ...emptyUsage(), input: 100, output: 10 },
    stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop",
    timestamp: 0,
    ...extra,
  };
}
const call = (id: string, name: string, input: Record<string, unknown>) => ({ type: "tool_call" as const, id, name, input });
const say = (text: string) => ({ type: "text" as const, text });

function setup(script: AssistantMessage[], opts: Partial<ConstructorParameters<typeof Agent>[0]> = {}) {
  const provider = replayProvider(script);
  registerApi("replay", provider);
  const agent = new Agent({ model, cwd: dir, systemPrompt: "sys", tools, ...opts });
  const events: AgentEvent[] = [];
  agent.events.on((e) => events.push(e));
  return { agent, provider, events };
}

test("fixes a bug end to end: read, edit, verify with bash, then stop", async () => {
  writeFileSync(join(dir, "add.js"), "export const add = (a, b) => a - b;\n");
  const { agent, provider } = setup([
    reply([say("Let me look."), call("1", "read", { path: "add.js" })]),
    reply([call("2", "edit", { path: "add.js", old_string: "a - b", new_string: "a + b" })]),
    reply([call("3", "bash", { command: "bun -e \"import {add} from './add.js'; console.log(add(2,3))\"" })]),
    reply([say("Fixed: add now returns 5 for (2,3).")]),
  ], { permissions: new PermissionPolicy("auto") });
  expect(await agent.prompt("add is broken")).toBe("done");
  expect(readFileSync(join(dir, "add.js"), "utf8")).toContain("a + b");
  expect(provider.requests).toHaveLength(4);
  const last = provider.requests[3]!.messages.at(-1)!;
  expect(last.role).toBe("tool");
  expect(JSON.stringify(last)).toContain("5\\n[exit code 0]");
});

test("tool errors, unknown tools and bad arguments go back to the model", async () => {
  const { agent, provider } = setup([
    reply([call("1", "nope", {}), call("2", "read", { path: 42 }), call("3", "read", { path: "missing.txt" })]),
    reply([say("ok")]),
  ]);
  await agent.prompt("go");
  const results = provider.requests[1]!.messages.filter((m) => m.role === "tool");
  expect(results.map((r) => r.isError)).toEqual([true, true, true]);
  expect(JSON.stringify(results)).toContain("Unknown tool");
  expect(JSON.stringify(results)).toContain("input.path must be string");
});

test("approval: deny with feedback, always-allow adds a rule, headless denies", async () => {
  const asked: string[] = [];
  const { agent, provider } = setup(
    [
      reply([call("1", "bash", { command: "echo a" })]),
      reply([call("2", "bash", { command: "echo b" })]),
      reply([call("3", "bash", { command: "echo b" })]),
      reply([say("done")]),
    ],
    {
      approve: async (r) => {
        asked.push(String(r.args.command));
        return r.args.command === "echo a" ? { decision: "deny", feedback: "use printf" } : { decision: "always" };
      },
    },
  );
  await agent.prompt("go");
  expect(asked).toEqual(["echo a", "echo b"]); // third call covered by the always rule
  expect(JSON.stringify(provider.requests[1]!.messages.at(-1))).toContain("User feedback: use printf");

  const headless = setup([reply([call("1", "bash", { command: "echo x" })]), reply([say("k")])]);
  await headless.agent.prompt("go");
  expect(JSON.stringify(headless.provider.requests[1]!.messages.at(-1))).toContain("no user is available");
});

test("interrupting a running tool reports it and tells the model on the next prompt", async () => {
  const { agent, provider } = setup(
    [reply([call("1", "bash", { command: "sleep 20" })]), reply([say("ok")])],
    { permissions: new PermissionPolicy("auto") },
  );
  agent.events.on((e) => {
    if (e.type === "tool_start") setTimeout(() => agent.abort(), 100);
  });
  expect(await agent.prompt("sleep")).toBe("aborted");
  expect(JSON.stringify(agent.messages.at(-1))).toContain("interrupted by the user");
  await agent.prompt("next");
  const lastUser = provider.requests[1]!.messages.at(-1)!;
  expect(JSON.stringify(lastUser)).toContain("interrupted the previous response");
});

test("messages sent while running are delivered next turn", async () => {
  const { agent, provider } = setup([reply([call("1", "read", { path: "add.js" })]), reply([say("saw it")])]);
  agent.events.on((e) => {
    if (e.type === "tool_start") void agent.prompt("also check tests");
  });
  await agent.prompt("look");
  await agent.waitForIdle();
  const msgs = provider.requests[1]!.messages;
  expect(msgs.at(-1)).toMatchObject({ role: "user", content: [{ text: "also check tests" }] });
});

test("read-only tools in one turn run in parallel", async () => {
  let active = 0;
  let peak = 0;
  const slow = {
    ...readTool,
    name: "slow",
    execute: async () => {
      peak = Math.max(peak, ++active);
      await Bun.sleep(50);
      active--;
      return { content: [say("x")] };
    },
  };
  const { agent } = setup([reply([call("1", "slow", { path: "a" }), call("2", "slow", { path: "b" }), call("3", "slow", { path: "c" })]), reply([say("k")])], {
    tools: [slow],
    permissions: new PermissionPolicy("auto"),
  });
  await agent.prompt("go");
  expect(peak).toBe(3);
});

test("context limit stops the loop with an event", async () => {
  const big = reply([call("1", "read", { path: "add.js" })], { usage: { ...emptyUsage(), input: 99_000, output: 10 } });
  const { agent, events } = setup([big, reply([say("never")])]);
  expect(await agent.prompt("go")).toBe("context_limit");
  expect(events.some((e) => e.type === "context_limit")).toBe(true);
});

test("maxTurns is opt-in", async () => {
  const loop = () => reply([call("x", "read", { path: "add.js" })]);
  const { agent } = setup([loop(), loop(), loop()], { maxTurns: 2 });
  expect(await agent.prompt("go")).toBe("max_turns");
});

test("provider errors end the run with an error event", async () => {
  const failing: Provider = {
    api: "replay",
    // biome-ignore lint: generator that throws
    async *stream() {
      throw new Error("boom");
    },
  };
  registerApi("replay", failing);
  const agent = new Agent({ model, cwd: dir, systemPrompt: "s", tools });
  const events: AgentEvent[] = [];
  agent.events.on((e) => events.push(e));
  expect(await agent.prompt("go")).toBe("error");
  expect(events).toContainEqual({ type: "error", error: "boom" });
});

test("sessions persist every message, redact secrets, and resume to the last complete turn", async () => {
  const session = SessionFile.create(join(dir, "sessions"), dir);
  session.addSecret("sk-secret-123456");
  const { agent } = setup([reply([call("1", "bash", { command: "echo sk-secret-123456" })]), reply([say("done")])], {
    session,
    permissions: new PermissionPolicy("auto"),
  });
  await agent.prompt("print it");
  const raw = readFileSync(session.path, "utf8");
  expect(raw).not.toContain("sk-secret-123456");
  expect(raw).toContain("[REDACTED]");

  const { entries } = SessionFile.open(session.path);
  expect(restore(entries).messages).toHaveLength(4);
  expect(restore(entries).model).toBe("test/m");
  // Simulate a crash after a tool call was issued but before its result was written.
  session.append({ type: "message", message: reply([call("9", "bash", { command: "x" })]) });
  writeFileSync(session.path, `${readFileSync(session.path, "utf8")}{"type":"mess`); // torn line
  const again = restore(SessionFile.open(session.path).entries);
  // The dangling call gets a result saying its outcome is unknown (it may have run), so the history stays valid for every API.
  expect(again.repaired).toBe(true);
  expect(again.messages).toHaveLength(6);
  expect(again.messages.at(-1)).toMatchObject({ role: "tool", toolCallId: "9", isError: true, content: [{ text: expect.stringContaining("Result unknown") }] });
});

test("built-in tools register through the plugin API", async () => {
  const agent = new Agent({ model, cwd: dir, systemPrompt: "s" });
  const host = new PluginHost({ agent, cwd: dir });
  await host.load("builtin-tools", builtinTools);
  expect(agent.getTools().map((t) => t.name)).toEqual(["read", "write", "edit", "bash"]);
  expect(host.toolOwners.get("bash")).toBe("builtin-tools");
});
