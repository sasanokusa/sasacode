// The "agent" permission mode's judge, a bundled plugin: a model decides the calls the mode leaves
// to the user, through the permission hook, so it never loosens a rule.
import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Agent, type ApprovalRequest, PermissionPolicy, type PermissionRules, PluginHost } from "@sasacode/agent";
import { type AssistantContent, type AssistantMessage, emptyUsage, type ModelInfo, registerApi } from "@sasacode/ai";
import { replayProvider } from "@sasacode/testing";
import builtinTools from "@sasacode/tools";
import agentJudge from "../src/agent-judge.ts";

const dir = mkdtempSync(join(tmpdir(), "sasacode-agent-judge-"));
afterAll(() => rmSync(dir, { recursive: true, force: true }));

const model: ModelInfo = { id: "m", provider: "t", api: "replay", contextWindow: 100_000, maxOutput: 4000 };
const reply = (content: AssistantContent[]): AssistantMessage => ({
  role: "assistant", content, api: "replay", provider: "t", model: "m", usage: emptyUsage(),
  stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop", timestamp: 0,
});
const bash = (command: string) => reply([{ type: "tool_call", id: "1", name: "bash", input: { command } }]);
const say = (text: string) => reply([{ type: "text", text }]);
const done = say("done");

async function run(script: AssistantMessage[], opts: { mode?: "agent" | "edits"; rules?: PermissionRules; settings?: Record<string, unknown> } = {}) {
  const provider = replayProvider(script);
  registerApi("replay", provider);
  const asked: ApprovalRequest[] = [];
  const agent = new Agent({
    model, cwd: dir, systemPrompt: "s",
    permissions: new PermissionPolicy(opts.mode ?? "agent", opts.rules),
    resolveModel: (spec) => ({ ...model, id: spec.slice(spec.indexOf("/") + 1) }),
    approve: async (r) => (asked.push(r), { decision: "deny" }),
  });
  const host = new PluginHost({ agent, cwd: dir, settings: () => opts.settings ?? {} });
  await host.load("builtin-tools", builtinTools);
  await host.load("agent-judge", agentJudge);
  await agent.prompt("go");
  const tool = JSON.stringify(agent.messages.find((m) => m.role === "tool")?.content);
  return { models: provider.requests.map((r) => r.model.id), asked, tool };
}

test("a call judged safe runs unasked, with the judging model from settings; the run keeps its own", async () => {
  const r = await run([bash("echo j"), say('{"safe": true, "reason": "echo"}'), done], { settings: { model: "t/judge" } });
  expect(r.models).toEqual(["m", "judge", "m"]);
  expect(r.asked).toHaveLength(0);
  expect(r.tool).toContain("j");
});

test("risky, unreadable or failed judgements go to the user", async () => {
  for (const verdict of ['{"safe": false, "reason": "deletes"}', "no json here"]) {
    const r = await run([bash("rm -rf x"), say(verdict), done]);
    expect(r.asked).toHaveLength(1);
    expect(r.asked[0]!.reason).toMatch(verdict.includes("deletes") ? /agent judged risky: deletes/ : /judge failed: judge gave no verdict/);
  }
});

test("the judge never loosens a rule, and other modes are left alone", async () => {
  const ruled = await run([bash("echo x"), done], { rules: { ask: ["bash(echo*)"] } });
  expect(ruled.models).toEqual(["m", "m"]); // no judging request
  expect(ruled.asked).toHaveLength(1);
  const edits = await run([bash("echo x"), done], { mode: "edits" });
  expect(edits.models).toEqual(["m", "m"]);
  expect(edits.asked).toHaveLength(1);
});
