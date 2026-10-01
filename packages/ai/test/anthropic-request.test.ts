import { expect, test } from "bun:test";
import { anthropicProvider, thinkingFields } from "../src/anthropic.ts";
import { replayScope } from "../src/replay.ts";
import { emptyUsage, type AssistantMessage, type Request } from "../src/types.ts";

const user = { role: "user" as const, content: [{ type: "text" as const, text: "hello" }], timestamp: 0 };
function response(stop = "end_turn") {
  const events = [
    { type: "message_start", message: { id: "mock", type: "message", role: "assistant", model: "mock", content: [], stop_reason: null, usage: { input_tokens: 5, output_tokens: 0, cache_read_input_tokens: 20, cache_creation_input_tokens: 30 } } },
    { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
    { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "ok" } },
    { type: "content_block_stop", index: 0 },
    { type: "message_delta", delta: { stop_reason: stop }, usage: { output_tokens: 2 } },
    { type: "message_stop" },
  ];
  return new Response(events.map(e => `event: ${e.type}\ndata: ${JSON.stringify(e)}\n\n`).join(""), { headers: { "content-type": "text/event-stream" } });
}
const bad = (message: string) => Response.json({ type: "error", error: { type: "invalid_request_error", message } }, { status: 400 });
async function run(baseUrl: string | undefined, id: string, handler: (body: any, headers: Headers) => Response, extra: Partial<Request> = {}) {
  let done: AssistantMessage | undefined;
  const fetchMock = (async (_url: any, init: any) => handler(JSON.parse(init.body), new Headers(init.headers))) as unknown as typeof fetch;
  for await (const event of anthropicProvider.stream({ model: { id, provider: "anthropic", api: "anthropic", baseUrl, reasoning: true, contextWindow: 1e6, maxOutput: 1000 }, apiKey: "mock-key", system: "system", messages: [user], tools: [{ name: "add", description: "Add", parameters: { type: "object" } }], maxRetries: 0, fetch: fetchMock, ...extra }))
    if (event.type === "done") done = event.message;
  return done!;
}

test("official-only fields use the URL origin, not a hostname prefix", async () => {
  for (const [base, official] of [[undefined, true], ["https://api.anthropic.com/", true], ["https://api.anthropic.com.evil.invalid", false], ["https://api.anthropic.com@proxy.invalid", false], ["https://api.anthropic.com:8443", false], ["http://api.anthropic.com", false]] as const) {
    await run(base, "claude-opus-5", b => {
      expect(b.thinking).toEqual(official ? { type: "disabled" } : undefined);
      expect(b.tools[0].eager_input_streaming).toBe(official ? true : undefined);
      return response();
    }, { thinking: "off" });
  }
});

test("off stays omitted on proxies, including always-thinking models", () => {
  for (const id of ["claude-opus-5-5", "claude-fable-5-1", "claude-mythos-5-1", "claude-sonnet-5-5"])
    expect(thinkingFields(id, "off", false)).toEqual({});
  expect(thinkingFields("claude-sonnet-5-5", "off", true).thinking?.type as string).toBe("between_tools");
});

test("an effort rejection is remembered only for the endpoint that rejected it", async () => {
  const efforts: string[] = [];
  const handler = (b: any) => { efforts.push(b.output_config.effort); return b.output_config.effort === "xhigh" ? bad("effort xhigh not supported") : response(); };
  await run("https://reject-effort.invalid", "claude-test-effort", handler, { thinking: "xhigh" });
  await run("https://reject-effort.invalid", "claude-test-effort", handler, { thinking: "xhigh" });
  expect(efforts).toEqual(["xhigh", "high", "high"]);
  await run("https://accept-effort.invalid", "claude-test-effort", b => { expect(b.output_config.effort).toBe("xhigh"); return response(); }, { thinking: "xhigh" });
});

test("disabled rejection retries with low effort and does not change proxy off", async () => {
  const shapes: any[] = [];
  await run(undefined, "claude-test-disable", b => { shapes.push(b.thinking); if (b.thinking.type === "disabled") return bad("thinking.type.disabled is not supported"); expect(b.output_config.effort).toBe("low"); return response(); }, { thinking: "off" });
  expect(shapes).toEqual([{ type: "disabled" }, { type: "adaptive", display: "summarized" }]);
  await run(undefined, "claude-test-disable", b => { expect(b.thinking.type).toBe("adaptive"); expect(b.output_config.effort).toBe("low"); return response(); }, { thinking: "off" });
  await run("https://disable-proxy.invalid", "claude-test-disable", b => { expect(b.thinking).toBeUndefined(); return response(); }, { thinking: "off" });
});

test("binding beta preserves custom headers; unsupported binding retries without it", async () => {
  const binding: boolean[] = [];
  await run(undefined, "claude-sonnet-5-5", (b, h) => {
    binding.push(!!b.thinking.block_binding);
    if (b.thinking.block_binding) {
      expect(b.thinking.block_binding).toEqual({ prefix_mismatch_behavior: "drop_block" });
      expect(h.get("anthropic-beta")).toBe("custom-beta,thinking-binding-controls-2026-08-01");
      return bad("block_binding: Extra inputs are not permitted");
    }
    expect(h.get("anthropic-beta")).toBe("custom-beta");
    return response();
  }, { model: { id: "claude-sonnet-5-5", provider: "anthropic", api: "anthropic", reasoning: true, contextWindow: 1e6, maxOutput: 1000, headers: { "anthropic-beta": "custom-beta" } }, thinking: "high" });
  expect(binding).toEqual([true, false]);
});

test("unrelated block_binding errors do not repeat an unchanged request", async () => {
  let calls = 0;
  await expect(run("https://proxy-binding-error.invalid", "claude-test-binding", () => { calls++; return bad("messages contain invalid block_binding"); })).rejects.toThrow();
  expect(calls).toBe(1);
});

test("signed and redacted thinking replay, cache counters and refusal survive streaming", async () => {
  const earlier: AssistantMessage = { role: "assistant", provider: "anthropic", api: "anthropic", model: "claude-test-replay", timestamp: 0, stopReason: "stop", usage: emptyUsage(), content: [{ type: "thinking", thinking: "reason", signature: "signed" }, { type: "thinking", thinking: "", signature: "redacted", redacted: true }, { type: "text", text: "earlier" }] };
  earlier.replayScope = replayScope({ id: earlier.model, provider: "anthropic", api: "anthropic", baseUrl: "https://replay.invalid", contextWindow: 1e6, maxOutput: 1000 });
  const out = await run("https://replay.invalid", earlier.model, b => {
    expect(b.cache_control).toEqual({ type: "ephemeral" });
    expect(b.messages[1].content.slice(0, 2)).toEqual([{ type: "thinking", thinking: "reason", signature: "signed" }, { type: "redacted_thinking", data: "redacted" }]);
    return response("refusal");
  }, { messages: [user, earlier, user] });
  expect(out.stopReason).toBe("refusal");
  expect(out.usage).toMatchObject({ input: 5, output: 2, cacheRead: 20, cacheWrite: 30 });
});
