import { expect, test } from "bun:test";
import plugin, { diagnosticFetch, responseStats, wireStats } from "../src/diagnostics.ts";
import { loadPlugin } from "../../../plugins/test-support.ts";

const secret = "PRIVATE_CANARY_署名_KEY";
const body = JSON.stringify({ system: secret, tools: [{ description: secret }], messages: [{ role: "assistant", content: [{ type: "thinking", thinking: secret, signature: secret }] }] });
const event = { type: "message_start", message: { usage: { input_tokens: 5, cache_read_input_tokens: 20 }, input_transformations: [{ type: "thinking_dropped", reason: "prefix_binding_mismatch", path: secret }, { reason: secret }] } };
const stream = `event: message_start\ndata: ${JSON.stringify(event)}\n\ndata: ${JSON.stringify({ choices: [{ delta: { content: secret } }], usage: { prompt_tokens_details: { cached_tokens: 2, cache_write_tokens: 3 }, cost: 0.01, key: secret } })}\n\ndata: [DONE]\n\n`;

test("diagnostics only extract counters and documented drop reasons", () => {
  const stats = wireStats(body);
  expect(stats).toMatchObject({ messages: 1, thinkingBlocks: 1, signedBlocks: 1, requestBytes: Buffer.byteLength(body) });
  const response = { usage: {}, transformations: 0, drops: {} };
  responseStats(event, response);
  expect(response).toEqual({ usage: { input_tokens: 5, cache_read_input_tokens: 20 }, transformations: 2, drops: { prefix_binding_mismatch: 1 } });
  expect(JSON.stringify({ stats, response })).not.toContain(secret);
});

test("wire bytes, headers and split UTF-8 SSE pass through; retries are distinguished", async () => {
  const records: Record<string, unknown>[] = [];
  let calls = 0;
  const inner = (async (_input: any, init: any) => {
    expect(init.body).toBe(body); expect(init.headers.authorization).toBe(secret);
    if (++calls === 1) return new Response("rate limited " + secret, { status: 429 });
    const bytes = new TextEncoder().encode(stream);
    let index = 0;
    return new Response(new ReadableStream({ pull(c) { if (index < bytes.length) c.enqueue(bytes.slice(index, index += 7)); else c.close(); } }), { headers: { "content-type": "text/event-stream" } });
  }) as unknown as typeof fetch;
  const observed = diagnosticFetch(inner, r => records.push(r));
  const init = { method: "POST", body, headers: { authorization: secret } };
  expect((await observed(`https://user:${secret}@example.test?key=${secret}`, init)).status).toBe(429);
  expect(await (await observed("https://example.test", init)).text()).toBe(stream);
  expect(records[0]).toMatchObject({ status: 429, attempt: 1, repeatedBody: false });
  expect(records[1]).toMatchObject({ status: 200, attempt: 2, repeatedBody: true, outcome: "complete", transformations: 2, usage: { "prompt_tokens_details.cached_tokens": 2, "prompt_tokens_details.cache_write_tokens": 3, cost: 0.01 } });
  expect(JSON.stringify(records)).not.toContain(secret);
});

test("cancellation and recorder failure preserve transport semantics", async () => {
  let cancelled = false;
  const records: Record<string, unknown>[] = [];
  const inner = (async () => new Response(new ReadableStream({ cancel() { cancelled = true; } }), { headers: { "content-type": "text/event-stream" } })) as unknown as typeof fetch;
  const res = await diagnosticFetch(inner, r => records.push(r))("https://example.test", { body });
  await res.body!.cancel();
  expect(cancelled).toBe(true); expect(records[0]?.outcome).toBe("cancelled");
  const failed = new Error(secret);
  const ac = new AbortController(); ac.abort();
  const throwing = (async () => { throw failed; }) as unknown as typeof fetch;
  await expect(diagnosticFetch(throwing, () => { throw new Error("observer failed"); })("https://example.test", { signal: ac.signal })).rejects.toBe(failed);
});

test("the plugin is disabled by default and composes an existing fetch only when enabled", async () => {
  const h = await loadPlugin("diagnostics", plugin, import.meta.dir);
  const transport = (async () => new Response("ok")) as unknown as typeof fetch;
  let wrapped: typeof fetch | undefined;
  const event = { messages: [], model: h.agent.model, sampling: {}, fetch: transport };
  await h.agent.hooks.run("before_request", event, r => { wrapped = r.fetch; });
  expect(wrapped).toBeUndefined();
  await h.host.commands.find(c => c.name === "diagnostics")!.run({ args: "on" });
  await h.agent.hooks.run("before_request", event, r => { wrapped = r.fetch; });
  expect(await (await wrapped!("https://example.test")).text()).toBe("ok");
  await h.host.commands.find(c => c.name === "diagnostics")!.run({ args: "off" });
  expect(h.notices.join()).toContain("diagnostics off; 1 records");
});

test("agent fetch hook observes actual SDK 429 retries and final usage without secrets", async () => {
  const h = await loadPlugin("diagnostics", plugin, import.meta.dir, { enabled: true });
  h.agent.model = { ...h.agent.model, api: "openai-chat", baseUrl: "https://mock.invalid/v1" };
  let calls = 0, shown = "";
  const transport = (async () => ++calls === 1 ? Response.json({ error: { message: secret } }, { status: 429, headers: { "retry-after": "0" } }) : new Response('data: {"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}\n\ndata: [DONE]\n\n', { headers: { "content-type": "text/event-stream" } })) as unknown as typeof fetch;
  // A transport plugin registered ahead of diagnostics is composed instead of replaced.
  const { Agent, PluginHost, PermissionPolicy } = await import("@sasacode/agent");
  const agent = new Agent({ model: h.agent.model, cwd: import.meta.dir, systemPrompt: secret, maxRetries: 1, permissions: new PermissionPolicy("auto") });
  const host = new PluginHost({ agent, cwd: agent.cwd, settings: () => ({ enabled: true }) });
  host.setUI({ interactive: false, notify: () => {}, confirm: async () => false, select: async () => undefined, showText: async o => { shown = o.text; } });
  await host.load("transport", api => api.on("before_request", () => ({ fetch: transport })));
  await host.load("diagnostics", plugin);
  expect(await agent.prompt(secret)).toBe("done"); expect(calls).toBe(2);
  await host.commands.find(c => c.name === "diagnostics")!.run({ args: "show" });
  const records = JSON.parse(shown).records;
  expect(records.filter((r: any) => r.kind === "request").map((r: any) => r.status)).toEqual([429, 200]);
  expect(records.find((r: any) => r.status === 200)).toMatchObject({ attempt: 2, repeatedBody: true, usage: { prompt_tokens: 10, completion_tokens: 2 } });
  expect(shown).not.toContain(secret);
});
