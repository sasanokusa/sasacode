import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { type AssistantContent, type AssistantMessage, emptyUsage, type ModelInfo, registerApi, replayProvider } from "@sasacode/ai";
import type { Decision, HookMap, Plugin } from "@sasacode/plugin-api";
import builtinTools from "@sasacode/tools";
import { Agent, type ApprovalRequest, type PermissionMode, PermissionPolicy, type PermissionRules, PluginHost } from "../src/index.ts";

const dir = mkdtempSync(join(tmpdir(), "sasacode-permission-hook-"));
afterAll(() => rmSync(dir, { recursive: true, force: true }));
const model: ModelInfo = { id: "m", provider: "t", api: "replay", contextWindow: 100_000, maxOutput: 4000 };
const reply = (content: AssistantContent[]): AssistantMessage => ({
  role: "assistant",
  content,
  api: "replay",
  provider: "t",
  model: "m",
  usage: emptyUsage(),
  stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop",
  timestamp: 0,
});
const bash = (command: string) => reply([{ type: "tool_call", id: "1", name: "bash", input: { command } }]);
const done = reply([{ type: "text", text: "done" }]);

type Event = HookMap["permission"]["event"];

/** Runs one bash call with a permission plugin that answers `decide`, and says what happened. */
async function run(
  command: string,
  opts: { mode?: PermissionMode; rules?: PermissionRules; decide?: (ev: Event) => { decision?: Decision; reason?: string } | void; judge?: string; approve?: boolean },
) {
  const script = [bash(command), ...(opts.judge ? [reply([{ type: "text", text: opts.judge }])] : []), done];
  const provider = replayProvider(script);
  registerApi("replay", provider);
  const asked: ApprovalRequest[] = [];
  const agent = new Agent({
    model,
    cwd: dir,
    systemPrompt: "s",
    permissions: new PermissionPolicy(opts.mode ?? "auto", opts.rules),
    approve: opts.approve === undefined ? undefined : async (r) => (asked.push(r), { decision: opts.approve ? "allow" : "deny" }),
  });
  const host = new PluginHost({ agent, cwd: dir });
  await host.load("builtin-tools", builtinTools);
  const seen: Event[] = [];
  const plugin: Plugin = (api) =>
    api.on("permission", (ev) => {
      seen.push(structuredClone({ ...ev, tool: undefined, call: undefined }) as unknown as Event);
      return opts.decide?.(ev);
    });
  await host.load("p", plugin);
  await agent.prompt("go");
  const result = JSON.stringify(agent.messages.find((m) => m.role === "tool")?.content);
  return { seen, asked, result, requests: provider.requests.length };
}

test("permission hook: sees the verdict and its limit; can make any verdict stricter", async () => {
  const r = await run("echo hi", { decide: () => ({ decision: "deny", reason: "second opinion" }) });
  expect(r.seen[0]).toMatchObject({ mode: "auto", verdict: { decision: "allow", source: "mode" }, lowest: "allow", args: { command: "echo hi" } });
  expect(r.result).toContain("Permission denied (second opinion)");
});

test("permission hook: cannot loosen the user's deny rule, nor an ask rule", async () => {
  const deny = await run("echo nope", { rules: { deny: ["bash(echo nope*)"] }, decide: () => ({ decision: "allow" }) });
  expect(deny.seen[0]).toMatchObject({ verdict: { decision: "deny", source: "rule" }, lowest: "deny" });
  expect(deny.result).toContain("Permission denied (rule deny: bash(echo nope*))");

  const ask = await run("echo q", { rules: { ask: ["bash(echo q*)"] }, decide: () => ({ decision: "allow" }), approve: false });
  expect(ask.asked).toHaveLength(1);
  expect(ask.result).toContain("The user denied");
});

test("softDeny: denied like deny; a permission hook can only hand it to the user", async () => {
  const rules = { softDeny: ["bash(echo soft*)"] };
  const plain = await run("echo soft", { rules });
  expect(plain.result).toContain("Permission denied (rule softDeny: bash(echo soft*))");

  const handed = await run("echo soft", { rules, decide: () => ({ decision: "allow", reason: "looks fine" }), approve: true });
  expect(handed.seen[0]).toMatchObject({ verdict: { decision: "deny", source: "soft" }, lowest: "ask" });
  expect(handed.asked[0]!.reason).toBe("looks fine");
  expect(handed.result).toContain("soft");

  // A hard deny still wins when both match.
  const both = await run("echo soft", { rules: { ...rules, deny: ["bash(echo soft*)"] }, decide: () => ({ decision: "ask" }), approve: true });
  expect(both.asked).toHaveLength(0);
  expect(both.result).toContain("rule deny");
});

test("agent mode: a hook's decision replaces the model judge; without one the judge still runs", async () => {
  const decided = await run("echo a", { mode: "agent", decide: () => ({ decision: "allow" }) });
  expect(decided.seen[0]).toMatchObject({ mode: "agent", verdict: { decision: "ask", source: "mode" } });
  expect(decided.result).toContain("a");
  expect(decided.requests).toBe(2); // the call and the final answer: no judge request

  const judged = await run("echo b", { mode: "agent", judge: '{"safe": true, "reason": "echo"}' });
  expect(judged.requests).toBe(3);
  expect(judged.result).not.toContain("denied");
});

test("a tool_call decision cannot be loosened by a permission hook", async () => {
  const provider = replayProvider([bash("echo t"), done]);
  registerApi("replay", provider);
  const agent = new Agent({ model, cwd: dir, systemPrompt: "s", permissions: new PermissionPolicy("auto") });
  const host = new PluginHost({ agent, cwd: dir });
  await host.load("builtin-tools", builtinTools);
  let lowest: Decision | undefined;
  await host.load("first", (api) => api.on("tool_call", () => ({ decision: "deny", reason: "tool_call says no" })));
  await host.load("second", (api) =>
    api.on("permission", (ev) => {
      lowest = ev.lowest;
      return { decision: "allow" };
    }),
  );
  await agent.prompt("go");
  expect(lowest).toBe("deny");
  expect(JSON.stringify(agent.messages.find((m) => m.role === "tool")?.content)).toContain("tool_call says no");
});
