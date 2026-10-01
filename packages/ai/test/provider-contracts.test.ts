import { expect, test } from "bun:test";
import { getProvider, newReply, replayScope, type AssistantMessage, type Message, type ModelInfo, type Request } from "../src/index.ts";
import { parseChatUsage } from "../src/openai-chat.ts";

const user: Message = { role: "user", content: [{ type: "text", text: "go" }], timestamp: 0 };
const tool = { name: "read", description: "Read", parameters: { type: "object", properties: {} } };
const result: Message = { role: "tool", toolCallId: "c1", toolName: "read", content: [{ type: "text", text: "file" }], isError: false, timestamp: 0 };
const model = (api: string, provider = "test", baseUrl = "https://mock.invalid/v1"): ModelInfo => ({ api, provider, baseUrl, id: "test-contract", reasoning: true, contextWindow: 100000, maxOutput: 1000, price: { input: 1, output: 2, cacheRead: 0.1, cacheWrite: 1.25 } });
const sse = (events: unknown[]) => new Response(events.map(e => `data: ${JSON.stringify(e)}\n\n`).join("") + "data: [DONE]\n\n", { headers: { "content-type": "text/event-stream" } });
const chat = (delta: object, finish = "stop") => ({ choices: [{ index: 0, delta, finish_reason: finish }] });
const responses = (items: any[], usage: object = {}) => [
  ...items.flatMap((item, i) => [
    { type: "response.output_item.added", item, output_index: i },
    ...(item.type === "message" ? [{ type: "response.output_text.delta", item_id: item.id, output_index: i, delta: item.content[0].text }] : []),
    { type: "response.output_item.done", item, output_index: i },
  ]),
  { type: "response.completed", response: { usage: { input_tokens: 100, output_tokens: 10, input_tokens_details: { cached_tokens: 40, cache_write_tokens: 20 }, ...usage } } },
];
async function ask(m: ModelInfo, messages: Message[], reply: (body: any, attempt: number) => Response, extra: Partial<Request> = {}) {
  const bodies: any[] = [];
  const mock = (async (_input: any, init: any) => { bodies.push(JSON.parse(init.body)); return reply(bodies.at(-1), bodies.length); }) as unknown as typeof fetch;
  let out!: AssistantMessage;
  for await (const e of getProvider(m.api).stream({ model: m, system: "sys", apiKey: "fake", messages, tools: [tool], fetch: mock, maxRetries: 0, ...extra })) if (e.type === "done") out = e.message;
  return { out, bodies };
}

test("OpenRouter keeps consecutive reasoning_details exactly, prefers them to display text, and uses unified effort", async () => {
  const m = model("openai-chat", "openrouter", "https://openrouter.ai/api/v1");
  const details = [{ type: "reasoning.text", text: "visible", signature: "SIGNED", index: 0 }, { type: "reasoning.encrypted", data: "OPAQUE", format: "anthropic-v1", index: 1 }];
  const first = await ask(m, [user], b => {
    expect(b.reasoning).toEqual({ effort: "high" }); expect(b.reasoning_effort).toBeUndefined();
    return sse([chat({ reasoning: "display", reasoning_details: [details[0]] }, ""), chat({ reasoning_details: [details[1]], tool_calls: [{ index: 0, id: "c1", function: { name: "read", arguments: "{}" } }] }, "tool_calls")]);
  }, { thinking: "high" });
  // A JSON session round trip retains opaque data and the provenance digest.
  const resumed = JSON.parse(JSON.stringify(first.out));
  await ask(m, [user, resumed, result], b => {
    expect(b.messages[2].reasoning_details).toEqual(details);
    expect(b.messages[2].reasoning).toBeUndefined(); expect(b.messages[2].reasoning_content).toBeUndefined();
    return sse([chat({ content: "done" })]);
  });
  for (const changed of [model("openai-chat", "openrouter", "https://proxy.invalid/v1"), { ...m, id: "other" }, { ...m, provider: "other" }]) {
    const r = await ask(changed, [user, resumed, result], () => sse([chat({ content: "done" })]));
    expect(JSON.stringify(r.bodies[0])).not.toContain("OPAQUE"); expect(JSON.stringify(r.bodies[0])).not.toContain("SIGNED");
  }
});

test("Responses preserve every opaque reasoning item and each message phase in sequence", async () => {
  const m = model("openai-responses");
  const items = [
    { type: "reasoning", id: "rs1", summary: [], encrypted_content: "FIRST" },
    { type: "message", id: "msg1", role: "assistant", phase: "commentary", content: [{ type: "output_text", text: "checking", annotations: [] }] },
    { type: "function_call", id: "fc1", call_id: "c1", name: "read", arguments: "{}" },
    { type: "reasoning", id: "rs2", summary: [], encrypted_content: "SECOND" },
    { type: "message", id: "msg2", role: "assistant", phase: "final_answer", content: [{ type: "output_text", text: "answer", annotations: [] }] },
  ];
  const first = await ask(m, [user], b => {
    expect(b.store).toBe(false); expect(b.include).toContain("reasoning.encrypted_content");
    expect(b.reasoning).toMatchObject({ effort: "high", summary: "detailed" });
    return sse(responses(items, { cost: 0.01 }));
  }, { thinking: "high", sampling: { extraBody: { reasoning: { summary: "detailed", context: "all_turns" }, include: ["message.output_text.logprobs"] } } });
  // Provider fields are merged with extraBody, rather than silently removing caller controls.
  expect(first.bodies[0].reasoning).toMatchObject({ summary: "detailed", context: "all_turns" });
  expect(first.bodies[0].include).toContain("message.output_text.logprobs");
  expect(first.out.usage).toMatchObject({ input: 40, cacheRead: 40, cacheWrite: 20, cost: 0.01 });
  const resumed = JSON.parse(JSON.stringify(first.out));
  const second = await ask(m, [user, resumed, result], () => sse(responses([])));
  expect(second.bodies[0].input.slice(1, 6)).toEqual([
    items[0], { role: "assistant", content: "checking", phase: "commentary" },
    { type: "function_call", call_id: "c1", name: "read", arguments: "{}" },
    items[3], { role: "assistant", content: "answer", phase: "final_answer" },
  ]);
  const other = await ask({ ...m, baseUrl: "https://other.invalid/v1" }, [user, resumed, result], () => sse(responses([])));
  expect(JSON.stringify(other.bodies)).not.toContain("FIRST"); expect(JSON.stringify(other.bodies)).not.toContain("SECOND");
});

test("old sessions without endpoint provenance never forward opaque state", async () => {
  for (const api of ["anthropic", "openai-responses", "openai-chat"]) {
    const m = model(api, api === "openai-chat" ? "openrouter" : "test", api === "openai-chat" ? "https://openrouter.ai/api/v1" : "https://mock.invalid/v1");
    const earlier = newReply(m); delete earlier.replayScope;
    earlier.content = [{ type: "thinking", thinking: "OLD_REASONING", signature: api === "anthropic" ? "OLD_SIGNATURE" : api === "openai-chat" ? '[{"type":"reasoning.encrypted","data":"OLD_SIGNATURE"}]' : '{"type":"reasoning","id":"old","encrypted_content":"OLD_SIGNATURE"}', redacted: api === "openai-chat" }, { type: "text", text: "keep visible text" }];
    const mock = (async (_input: any, init: any) => {
      expect(init.body).not.toContain("OLD_SIGNATURE"); expect(init.body).not.toContain("OLD_REASONING"); expect(init.body).toContain("keep visible text");
      return api === "anthropic" ? new Response([
        { type: "message_start", message: { id: "msg", type: "message", role: "assistant", model: m.id, content: [], stop_reason: null, usage: { input_tokens: 1, output_tokens: 0 } } },
        { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 0 } }, { type: "message_stop" },
      ].map(e => `event: ${e.type}\ndata: ${JSON.stringify(e)}\n\n`).join(""), { headers: { "content-type": "text/event-stream" } }) : api === "openai-chat" ? sse([chat({ content: "ok" })]) : sse(responses([]));
    }) as unknown as typeof fetch;
    for await (const _ of getProvider(api).stream({ model: m, apiKey: "fake", system: "sys", messages: [user, earlier, user], tools: [], fetch: mock, maxRetries: 0 })) { /* drain */ }
    expect(replayScope(m)).toHaveLength(64);
  }
});

test("local Chat aliases replay their observed reasoning field; Ollama uses supported max_tokens", async () => {
  for (const field of ["reasoning", "reasoning_content"]) {
    const m = model("openai-chat", "ollama", "http://localhost:11434/v1");
    const first = await ask(m, [user], b => { expect(b.max_tokens).toBe(1000); expect(b.max_completion_tokens).toBeUndefined(); return sse([chat({ [field]: "thought", content: "ok" })]); });
    await ask(m, [user, first.out, user], b => { expect(b.messages[2][field]).toBe("thought"); expect(b.messages[2][field === "reasoning" ? "reasoning_content" : "reasoning"]).toBeUndefined(); return sse([chat({ content: "ok" })]); });
  }
});

test("Responses negotiate a first-event effort error, never a later error or history rejection", async () => {
  const m = model("openai-responses");
  const ok = await ask(m, [user], (_b, n) => n === 1 ? sse([{ type: "error", code: "400", message: "reasoning effort xhigh unsupported", param: "reasoning.effort" }]) : sse(responses([])), { thinking: "xhigh" });
  expect(ok.bodies.map(b => b.reasoning.effort)).toEqual(["xhigh", "high"]);
  for (const before of [[], [{ type: "response.created", response: { id: "started", output: [] } }]]) {
    let calls = 0;
    const message = before.length ? "reasoning effort unsupported" : "messages[1].reasoning_content is missing for effort high";
    await expect(ask({ ...m, id: "error-" + before.length }, [user], () => { calls++; return sse([...before, { type: "error", code: "400", message, param: "reasoning.effort" }]); }, { thinking: "high" })).rejects.toThrow();
    expect(calls).toBe(1);
  }
});

test("refusal text survives Chat and Responses streaming", async () => {
  const a = await ask(model("openai-chat"), [user], () => sse([chat({ refusal: "cannot help" })]));
  expect(a.out.stopReason).toBe("refusal"); expect(a.out.content).toEqual([{ type: "text", text: "cannot help" }]);
  const b = await ask(model("openai-responses"), [user], () => sse([
    { type: "response.output_item.added", item: { type: "message", id: "refusal", role: "assistant", content: [] } },
    { type: "response.refusal.delta", item_id: "refusal", delta: "cannot help" }, ...responses([]),
  ]));
  expect(b.out.stopReason).toBe("refusal"); expect(b.out.content).toEqual([{ type: "text", text: "cannot help" }]);
});

test("cache write counts are disjoint from fresh/read input, native DeepSeek aliases and reported spend remain exact", async () => {
  const m = model("openai-chat");
  expect(parseChatUsage(m, { prompt_tokens: 100, completion_tokens: 10, prompt_tokens_details: { cached_tokens: 40, cache_write_tokens: 20 }, cost: 0.01 })).toMatchObject({ input: 40, cacheRead: 40, cacheWrite: 20, cost: 0.01 });
  expect(parseChatUsage(m, { prompt_cache_hit_tokens: 70, prompt_cache_miss_tokens: 30, completion_tokens: 5 })).toMatchObject({ input: 30, cacheRead: 70, cacheWrite: 0 });
  const r = await ask(m, [user], () => sse([{ choices: [], usage: { prompt_tokens: 100, completion_tokens: 10, cost: 0.01 } }]));
  expect(r.out.usage.cost).toBe(0.01);
});

test("implicit SDK endpoint environment cannot invalidate replay provenance", async () => {
  const before = process.env.OPENAI_BASE_URL;
  try {
    process.env.OPENAI_BASE_URL = "https://unrelated.invalid/v1";
    const m = { ...model("openai-chat"), baseUrl: undefined };
    const mock = (async (url: any) => { expect(String(url)).toStartWith("https://api.openai.com/v1/"); return sse([chat({ content: "ok" })]); }) as unknown as typeof fetch;
    for await (const _ of getProvider("openai-chat").stream({ model: m, system: "", messages: [user], tools: [], apiKey: "fake", fetch: mock })) { /* drain */ }
  } finally { if (before === undefined) delete process.env.OPENAI_BASE_URL; else process.env.OPENAI_BASE_URL = before; }
});

test("explicit plugin effort controls are preserved and do not trigger repeated unchanged negotiation", async () => {
  for (const [m, extra] of [
    [model("openai-responses"), { reasoning: { effort: "medium", context: "current_turn" } }],
    [model("openai-chat", "openrouter", "https://openrouter.ai/api/v1"), { reasoning: { effort: "medium", exclude: true } }],
    [model("openai-chat"), { reasoning_effort: "medium" }],
  ] as const) {
    let calls = 0;
    await expect(ask(m, [user], b => { calls++; expect(b.reasoning?.effort ?? b.reasoning_effort).toBe("medium"); return Response.json({ error: { message: "reasoning effort medium unsupported", type: "invalid_request_error" } }, { status: 400 }); }, { thinking: "high", sampling: { extraBody: extra } })).rejects.toThrow();
    expect(calls).toBe(1);
  }
});

test("DeepSeek and MiMo plain replies do not gain reasoning fields from the local-server replay path", async () => {
  for (const id of ["deepseek-flash", "mimo-v2.6-flash"]) {
    const m = { ...model("openai-chat"), id };
    const first = await ask(m, [user], () => sse([chat({ reasoning_content: "thought", content: "ok" })]), { tools: [] });
    await ask(m, [user, first.out, user], b => { expect(b.messages[2].reasoning_content).toBeUndefined(); return sse([chat({ content: "ok" })]); }, { tools: [] });
  }
});
