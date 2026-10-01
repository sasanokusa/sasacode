import { afterAll, expect, test } from "bun:test";
import { anthropicProvider, thinkingFields } from "../src/anthropic.ts";
import { type AssistantMessage, emptyUsage, replayScope, type Message, type ModelInfo, type StreamEvent } from "../src/index.ts";

test("effort off: said explicitly on Claude 5; low effort where thinking cannot be turned off", () => {
  expect(thinkingFields("claude-opus-5", "off", true)).toEqual({ thinking: { type: "disabled" } });
  expect(thinkingFields("claude-sonnet-5", "off", true)).toEqual({ thinking: { type: "disabled" } });
  for (const id of ["claude-opus-5-5", "claude-fable-5-1"])
    expect(thinkingFields(id, "off", true)).toEqual({ thinking: { type: "adaptive", display: "summarized" }, output_config: { effort: "low" } });
  // A proxy or another server speaking the Messages API gets what it got before.
  expect(thinkingFields("claude-opus-5", "off", false)).toEqual({});
  expect(thinkingFields("claude-opus-5", "high", true)).toEqual({ thinking: { type: "adaptive", display: "summarized" }, output_config: { effort: "high" } });
});

// A Messages API server: rejects the first request's thinking blocks as bound elsewhere, or xhigh.
const bodies: any[] = [];
const server = Bun.serve({
  port: 0,
  async fetch(req) {
    const body = await req.json();
    bodies.push(body);
    const hasThinking = body.messages.some((m: any) => Array.isArray(m.content) && m.content.some((b: any) => b.type === "thinking"));
    const error = (message: string) => Response.json({ type: "error", error: { type: "invalid_request_error", message } }, { status: 400 });
    if (hasThinking) return error("messages.1.content.0: Invalid `signature` in `thinking` block. The block is bound to a different conversation.");
    if (body.output_config?.effort === "xhigh") return error("output_config.effort: xhigh is not supported for this model");
    const events = [
      { type: "message_start", message: { id: "x", type: "message", role: "assistant", model: body.model, content: [], stop_reason: null, usage: { input_tokens: 5, output_tokens: 0 } } },
      { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
      { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "ok" } },
      { type: "content_block_stop", index: 0 },
      { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 1 } },
      { type: "message_stop" },
    ];
    return new Response(events.map((e) => `event: ${e.type}\ndata: ${JSON.stringify(e)}\n\n`).join(""), { headers: { "content-type": "text/event-stream" } });
  },
});
afterAll(() => server.stop(true));

const model = (id: string): ModelInfo => ({ id, provider: "anthropic", api: "anthropic", baseUrl: `http://localhost:${server.port}`, contextWindow: 1e6, maxOutput: 1000, reasoning: true });
const user = (text: string): Message => ({ role: "user", content: [{ type: "text", text }], timestamp: 0 });
async function run(id: string, messages: Message[], thinking?: "xhigh") {
  let done: AssistantMessage | undefined;
  for await (const e of anthropicProvider.stream({ model: model(id), apiKey: "k", system: "", messages, tools: [], thinking, maxRetries: 0 }) as AsyncGenerator<StreamEvent>)
    if (e.type === "done") done = e.message;
  return done!;
}

test("thinking bound to another conversation: sent again once without thinking blocks", async () => {
  bodies.length = 0;
  const earlier: AssistantMessage = {
    replayScope: replayScope(model("claude-opus-5-5")), role: "assistant", api: "anthropic", provider: "anthropic", model: "claude-opus-5-5", usage: emptyUsage(), stopReason: "stop", timestamp: 0,
    content: [{ type: "thinking", thinking: "hmm", signature: "sig" }, { type: "text", text: "earlier" }],
  };
  const out = await run("claude-opus-5-5", [user("a"), earlier, user("b")]);
  expect(out.content).toEqual([{ type: "text", text: "ok" }]);
  expect(bodies).toHaveLength(2);
  expect(JSON.stringify(bodies[1].messages)).not.toContain("thinking");
  expect(JSON.stringify(bodies[1].messages)).toContain("earlier");
});

test("xhigh rejected: the closest effort is sent instead, and remembered", async () => {
  bodies.length = 0;
  const say = [user("a")];
  expect((await run("claude-sonnet-4-6", say, "xhigh")).content).toEqual([{ type: "text", text: "ok" }]);
  expect(bodies.map((b) => b.output_config.effort)).toEqual(["xhigh", "high"]);
  await run("claude-sonnet-4-6", say, "xhigh");
  expect(bodies.at(-1).output_config.effort).toBe("high");
  expect(bodies).toHaveLength(3);
});
