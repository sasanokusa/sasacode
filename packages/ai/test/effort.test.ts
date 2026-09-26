import { expect, test } from "bun:test";
import { getProvider, type ModelInfo, type Request, type ThinkingLevel } from "../src/index.ts";

const sse = (chunks: unknown[]) =>
  new Response(chunks.map((c) => `data: ${JSON.stringify(c)}\n\n`).join("") + "data: [DONE]\n\n", { headers: { "content-type": "text/event-stream" } });
const ok = () => sse([{ id: "1", object: "chat.completion.chunk", created: 0, model: "m", choices: [{ index: 0, delta: { content: "hi" }, finish_reason: "stop" }] }]);
const refuse = (message: string) => new Response(JSON.stringify({ error: { message, type: "invalid_request_error" } }), { status: 400, headers: { "content-type": "application/json" } });

/** A server that takes only the listed efforts, and records what each request sent. */
function server(accepts: (string | undefined)[], refusal = 'Invalid option: expected one of "low"|"high" (reasoning_effort)', inStream = false) {
  const sent: (string | undefined)[] = [];
  const fetch = (async (_url: string, init: RequestInit) => {
    const effort = JSON.parse(String(init.body)).reasoning_effort as string | undefined;
    sent.push(effort);
    if (accepts.includes(effort)) return ok();
    return inStream ? sse([{ error: { message: refusal, type: "BadRequestError", param: null, code: 400 } }]) : refuse(refusal);
  }) as unknown as typeof globalThis.fetch;
  return { sent, fetch };
}

let n = 0;
const model = (m: Partial<ModelInfo> = {}): ModelInfo => ({ id: `m${++n}`, provider: "p", api: "openai-chat", baseUrl: "http://x/v1", contextWindow: 1000, maxOutput: 100, reasoning: true, ...m });

async function ask(fetch: typeof globalThis.fetch, thinking: ThinkingLevel, m = model()): Promise<string> {
  const req: Request = { model: m, system: "s", messages: [{ role: "user", content: [{ type: "text", text: "q" }], timestamp: 0 }], tools: [], thinking, fetch, maxRetries: 0 };
  let text = "";
  for await (const e of getProvider("openai-chat").stream(req)) if (e.type === "done") text = e.message.content.map((c) => (c.type === "text" ? c.text : "")).join("");
  return text;
}

test("off is sent as none, so servers that think by default (vLLM) stop thinking", async () => {
  const s = server(["none"]);
  expect(await ask(s.fetch, "off")).toBe("hi");
  expect(s.sent).toEqual(["none"]);
});

test("a refused effort falls back to the closest one, and the server's answer is remembered", async () => {
  const s = server(["low", "high"]); // Command Code: no "none", no "xhigh"
  const m = model();
  await ask(s.fetch, "off", m);
  expect(s.sent).toEqual(["none", "minimal", "low"]);
  await ask(s.fetch, "off", m);
  expect(s.sent.slice(3)).toEqual(["low"]);
  const x = server(["high"]);
  await ask(x.fetch, "max");
  expect(x.sent).toEqual(["max", "xhigh", "high"]);
});

test("vLLM refuses inside an HTTP 200 stream; that is recognized too", async () => {
  const s = server(["xhigh"], "Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low.", true);
  expect(await ask(s.fetch, "high")).toBe("hi");
  expect(s.sent).toEqual(["high", "xhigh"]);
});

test("a model that cannot think at all gets no effort; other errors are not retried", async () => {
  const s = server([undefined], '"llama" does not support thinking');
  expect(await ask(s.fetch, "high")).toBe("hi");
  expect(s.sent).toEqual(["high", "xhigh", "medium", undefined]);
  const t = server([], "messages: too long");
  await expect(ask(t.fetch, "high")).rejects.toThrow(/too long/);
  expect(t.sent).toEqual(["high"]);
});

test("a model known not to reason is sent no effort", async () => {
  const s = server([undefined]);
  await ask(s.fetch, "high", model({ reasoning: false }));
  expect(s.sent).toEqual([undefined]);
});
