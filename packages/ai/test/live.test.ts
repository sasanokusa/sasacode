// Against the real APIs, and only when asked: SASACODE_LIVE=1 plus the provider's key. Each run
// costs a few cents. Checks what the recorded tests cannot: that the providers still accept what
// the adapters send (tool calls, thinking replay, caching).
//
//   SASACODE_LIVE=1 ANTHROPIC_API_KEY=… OPENAI_API_KEY=… bun test packages/ai/test/live.test.ts
import { expect, test } from "bun:test";
import { type AssistantMessage, getProvider, type Message, resolveModel, BUILTIN_PROVIDERS, type ThinkingLevel } from "../src/index.ts";

const live = process.env.SASACODE_LIVE === "1";
const cases = [
  { spec: "anthropic/claude-sonnet-5", key: "ANTHROPIC_API_KEY" },
  { spec: "openai/gpt-5.5", key: "OPENAI_API_KEY" },
];
const tools = [{ name: "add", description: "Add two numbers", parameters: { type: "object", properties: { a: { type: "number" }, b: { type: "number" } }, required: ["a", "b"] } }];

async function send(spec: string, key: string, messages: Message[], thinking: ThinkingLevel): Promise<AssistantMessage> {
  const model = resolveModel(spec, BUILTIN_PROVIDERS, {});
  let done: AssistantMessage | undefined;
  for await (const e of getProvider(model.api).stream({ model, apiKey: process.env[key], system: "Use the add tool for arithmetic.", messages, tools, thinking, maxTokens: 2048 }))
    if (e.type === "done") done = e.message;
  return done!;
}

for (const c of cases)
  test.skipIf(!live || !process.env[c.key])(`live ${c.spec}: a tool call, its result, and the reply after it (thinking replayed)`, async () => {
    const messages: Message[] = [{ role: "user", content: [{ type: "text", text: "What is 17 + 25?" }], timestamp: Date.now() }];
    const first = await send(c.spec, c.key, messages, "low");
    const call = first.content.find((b) => b.type === "tool_call");
    expect(call?.type === "tool_call" && call.name).toBe("add");
    if (call?.type !== "tool_call") return;
    messages.push(first, { role: "tool", toolCallId: call.id, toolName: "add", content: [{ type: "text", text: "42" }], isError: false, timestamp: Date.now() });
    const second = await send(c.spec, c.key, messages, "low");
    expect(second.stopReason).toBe("stop");
    expect(second.content.some((b) => b.type === "text" && b.text.includes("42"))).toBe(true);
    expect(second.usage.input + second.usage.cacheRead).toBeGreaterThan(0);
  }, 120_000);
