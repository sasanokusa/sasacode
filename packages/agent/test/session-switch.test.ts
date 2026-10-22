import { expect, test } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { type AssistantContent, type AssistantMessage, emptyUsage, type ModelInfo, registerApi } from "@sasacode/ai";
import { replayProvider } from "@sasacode/testing";
import { Agent } from "../src/index.ts";

// /clear, /resume and /fork keep the Agent and swap its conversation: nothing of the old one may leak in.
const dir = mkdtempSync(join(tmpdir(), "sasacode-switch-"));
const model: ModelInfo = { id: "m", provider: "test", api: "replay-switch", contextWindow: 100_000, maxOutput: 4000 };
const reply = (content: AssistantContent[]): AssistantMessage => ({
  role: "assistant", content, api: "replay-switch", provider: "test", model: "m", usage: { ...emptyUsage(), input: 100, output: 10 }, stopReason: "stop", timestamp: 0,
});
const say = (text: string) => ({ type: "text" as const, text });
const setup = (script: AssistantMessage[]) => {
  const provider = replayProvider(script, { api: "replay-switch", chunkSize: 20 });
  registerApi("replay-switch", provider);
  return { provider, agent: new Agent({ model, cwd: dir, systemPrompt: "sys" }) };
};

test("the interrupted note stays with its conversation: a reset one starts without it", async () => {
  const { provider, agent } = setup([reply([say("x".repeat(2000))]), reply([say("fresh")])]);
  const off = agent.events.on((e) => {
    if (e.type === "message_update") agent.abort();
  });
  expect(await agent.prompt("first")).toBe("aborted");
  off();
  agent.reset();
  await agent.prompt("second, in a new session");
  expect(JSON.stringify(provider.requests[1]!.messages)).not.toContain("interrupted the previous response");
});

test("halt drops queued input: it is not run on the conversation being left", async () => {
  const { provider, agent } = setup([reply([say("x".repeat(2000))]), reply([say("answer to the queued one")])]);
  let halted: Promise<void> | undefined;
  const off = agent.events.on((e) => {
    if (e.type !== "message_update" || halted) return;
    void agent.prompt("queued while running");
    halted = agent.halt(); // what /clear does mid-reply
  });
  expect(await agent.prompt("first")).toBe("aborted");
  await halted;
  off();
  expect(provider.requests).toHaveLength(1);
  agent.reset();
  await agent.prompt("in the new session");
  expect(provider.requests).toHaveLength(2);
  expect(JSON.stringify(provider.requests[1]!.messages)).not.toContain("queued while running");
});
