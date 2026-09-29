// The three adapters, without a model: a fake fetch records what is sent and replays a recorded
// stream. Covers the conversion of the history and the parsing of the stream.
import { expect, test } from "bun:test";
import { anthropicProvider } from "../src/anthropic.ts";
import { type AssistantMessage, emptyUsage, INCOMPLETE_STREAM, type Message, type ModelInfo, type Request, type StreamEvent } from "../src/index.ts";
import { openaiChatProvider } from "../src/openai-chat.ts";
import { openaiResponsesProvider } from "../src/openai-responses.ts";

function fakeFetch(body: string, status = 200) {
  const sent: any[] = [];
  const fetch = (async (_url: string, init?: RequestInit) => {
    sent.push(JSON.parse(String(init?.body)));
    return new Response(body, { status, headers: { "content-type": status === 200 ? "text/event-stream" : "application/json" } });
  }) as unknown as typeof globalThis.fetch;
  return { fetch, sent };
}
const sse = (events: object[], named = false) =>
  events.map((e) => `${named ? `event: ${(e as { type: string }).type}\n` : ""}data: ${JSON.stringify(e)}\n\n`).join("") + (named ? "" : "data: [DONE]\n\n");

async function run(provider: typeof openaiChatProvider, req: Request) {
  const events: StreamEvent[] = [];
  for await (const e of provider.stream(req)) events.push(e);
  const done = events.at(-1);
  if (done?.type !== "done") throw new Error("no done event");
  return { message: done.message, events };
}

const png = { type: "image" as const, mediaType: "image/png", data: "iVBORw0KGgo=" };
const user = (text: string): Message => ({ role: "user", content: [{ type: "text", text }], timestamp: 0 });
const assistant = (model: ModelInfo, content: AssistantMessage["content"]): AssistantMessage => ({
  role: "assistant", content, api: model.api, provider: model.provider, model: model.id, usage: emptyUsage(), stopReason: "tool_use", timestamp: 0,
});
const history = (model: ModelInfo): Message[] => [
  user("look at a.png"),
  assistant(model, [{ type: "thinking", thinking: "hmm", signature: model.api === "openai-responses" ? JSON.stringify({ type: "reasoning", id: "rs0", summary: [] }) : "sig" }, { type: "text", text: "reading" }, { type: "tool_call", id: "c1", name: "read", input: { path: "a.png" } }]),
  { role: "tool", toolCallId: "c1", toolName: "read", content: [{ type: "text", text: "Image a.png" }, png], isError: false, timestamp: 0 },
  user("and now?"),
];
const tools = [{ name: "read", description: "read a file", parameters: { type: "object", properties: { path: { type: "string" } } } }];

test("openai-chat: tool results are tool messages, their images follow as a user message; stream parsed", async () => {
  const model: ModelInfo = { id: "gpt-x", provider: "openai", api: "openai-chat", baseUrl: "http://x/v1", contextWindow: 1e5, maxOutput: 1000, price: { input: 1, output: 2 } };
  const chunk = (delta: object, finish: string | null = null) => ({ id: "1", object: "chat.completion.chunk", created: 0, model: "gpt-x", choices: [{ index: 0, delta, finish_reason: finish }] });
  const { fetch, sent } = fakeFetch(
    sse([
      chunk({ role: "assistant", reasoning_content: "think" }),
      chunk({ content: "Hel" }),
      chunk({ content: "lo" }),
      chunk({ tool_calls: [{ index: 0, id: "c2", type: "function", function: { name: "read", arguments: '{"pa' } }] }),
      chunk({ tool_calls: [{ index: 0, function: { arguments: 'th":"b"}' } }] }, "tool_calls"),
      { id: "1", object: "chat.completion.chunk", created: 0, model: "gpt-x", choices: [], usage: { prompt_tokens: 100, completion_tokens: 20, prompt_tokens_details: { cached_tokens: 40 } } },
    ]),
  );
  const { message } = await run(openaiChatProvider, { model, system: "sys", messages: history(model), tools, fetch, maxRetries: 0 });
  const body = sent[0];
  expect(body.messages.map((m: { role: string }) => m.role)).toEqual(["system", "user", "assistant", "tool", "user", "user"]);
  expect(body.messages[2].tool_calls[0].function).toEqual({ name: "read", arguments: '{"path":"a.png"}' });
  expect(body.messages[4].content[1].image_url.url).toStartWith("data:image/png;base64,");
  expect(message.content).toEqual([
    { type: "thinking", thinking: "think" },
    { type: "text", text: "Hello" },
    { type: "tool_call", id: "c2", name: "read", input: { path: "b" } },
  ]);
  expect(message.stopReason).toBe("tool_use");
  expect(message.usage).toMatchObject({ input: 60, cacheRead: 40, output: 20 });
});

test("openai-chat: a context overflow error becomes ContextOverflowError; a truncated reply is max_tokens", async () => {
  const model: ModelInfo = { id: "m", provider: "p", api: "openai-chat", baseUrl: "http://x/v1", contextWindow: 1e5, maxOutput: 1000 };
  const { fetch } = fakeFetch(JSON.stringify({ error: { message: "This model's maximum context length is 8192 tokens", code: "context_length_exceeded" } }), 400);
  await expect(run(openaiChatProvider, { model, system: "", messages: [user("x")], tools: [], fetch, maxRetries: 0 })).rejects.toThrow("maximum context length");
  const cut = fakeFetch(sse([{ id: "1", object: "chat.completion.chunk", created: 0, model: "m", choices: [{ index: 0, delta: { content: "par" }, finish_reason: "length" }] }]));
  expect((await run(openaiChatProvider, { model, system: "", messages: [user("x")], tools: [], fetch: cut.fetch, maxRetries: 0 })).message.stopReason).toBe("max_tokens");
});

test("openai-responses: history as input items; reasoning, text and function calls parsed", async () => {
  const model: ModelInfo = { id: "gpt-r", provider: "openai", api: "openai-responses", baseUrl: "http://x/v1", contextWindow: 1e5, maxOutput: 1000, reasoning: true };
  const { fetch, sent } = fakeFetch(
    sse(
      [
        { type: "response.created", sequence_number: 0, response: { id: "r", output: [] } },
        { type: "response.output_item.added", sequence_number: 1, output_index: 0, item: { type: "reasoning", id: "rs", summary: [] } },
        { type: "response.reasoning_summary_text.delta", sequence_number: 2, item_id: "rs", output_index: 0, summary_index: 0, delta: "plan" },
        { type: "response.output_item.done", sequence_number: 3, output_index: 0, item: { type: "reasoning", id: "rs", summary: [{ type: "summary_text", text: "plan" }] } },
        { type: "response.output_item.added", sequence_number: 4, output_index: 1, item: { type: "message", id: "m1", role: "assistant", content: [] } },
        { type: "response.output_text.delta", sequence_number: 5, item_id: "m1", output_index: 1, content_index: 0, delta: "ok" },
        { type: "response.output_item.added", sequence_number: 6, output_index: 2, item: { type: "function_call", id: "f1", call_id: "c3", name: "read", arguments: "" } },
        { type: "response.function_call_arguments.delta", sequence_number: 7, item_id: "f1", output_index: 2, delta: '{"path":"c"}' },
        { type: "response.output_item.done", sequence_number: 8, output_index: 2, item: { type: "function_call", id: "f1", call_id: "c3", name: "read", arguments: '{"path":"c"}' } },
        { type: "response.completed", sequence_number: 9, response: { id: "r", output: [], usage: { input_tokens: 50, output_tokens: 7, input_tokens_details: { cached_tokens: 10 } } } },
      ],
      true,
    ),
  );
  const { message } = await run(openaiResponsesProvider, { model, system: "sys", messages: history(model), tools, fetch, maxRetries: 0, thinking: "high" });
  const types = sent[0].input.map((i: { type?: string; role?: string }) => i.type ?? i.role);
  expect(types).toContain("reasoning");
  expect(types).toContain("function_call");
  expect(types).toContain("function_call_output");
  expect(message.content[0]).toMatchObject({ type: "thinking", thinking: "plan" });
  expect(message.content.slice(1)).toEqual([
    { type: "text", text: "ok" },
    { type: "tool_call", id: "c3", name: "read", input: { path: "c" } },
  ]);
  expect(message.stopReason).toBe("tool_use");
  expect(message.usage).toMatchObject({ input: 40, cacheRead: 10, output: 7 });
});

test("anthropic: same-role messages merged, tool result images kept, own thinking replayed; stream parsed", async () => {
  const model: ModelInfo = { id: "claude-sonnet-4-6", provider: "anthropic", api: "anthropic", baseUrl: "http://x", contextWindow: 1e5, maxOutput: 1000, reasoning: true };
  const { fetch, sent } = fakeFetch(
    sse(
      [
        { type: "message_start", message: { id: "x", type: "message", role: "assistant", model: model.id, content: [], stop_reason: null, usage: { input_tokens: 30, output_tokens: 0, cache_read_input_tokens: 5 } } },
        { type: "content_block_start", index: 0, content_block: { type: "thinking", thinking: "", signature: "" } },
        { type: "content_block_delta", index: 0, delta: { type: "thinking_delta", thinking: "t" } },
        { type: "content_block_delta", index: 0, delta: { type: "signature_delta", signature: "S" } },
        { type: "content_block_stop", index: 0 },
        { type: "content_block_start", index: 1, content_block: { type: "tool_use", id: "c4", name: "read", input: {} } },
        { type: "content_block_delta", index: 1, delta: { type: "input_json_delta", partial_json: '{"path":"d"}' } },
        { type: "content_block_stop", index: 1 },
        { type: "message_delta", delta: { stop_reason: "tool_use" }, usage: { output_tokens: 9 } },
        { type: "message_stop" },
      ],
      true,
    ),
  );
  const { message } = await run(anthropicProvider, { model, apiKey: "k", system: "sys", messages: history(model), tools, fetch, maxRetries: 0 });
  const msgs = sent[0].messages;
  expect(msgs.map((m: { role: string }) => m.role)).toEqual(["user", "assistant", "user"]);
  expect(msgs[1].content[0]).toEqual({ type: "thinking", thinking: "hmm", signature: "sig" });
  expect(msgs[2].content[0].type).toBe("tool_result");
  expect(msgs[2].content[0].content[1].type).toBe("image");
  expect(msgs[2].content[1]).toEqual({ type: "text", text: "and now?" });
  expect(message.content).toEqual([
    { type: "thinking", thinking: "t", signature: "S" },
    { type: "tool_call", id: "c4", name: "read", input: { path: "d" } },
  ]);
  expect(message.stopReason).toBe("tool_use");
  expect(message.usage).toMatchObject({ input: 30, cacheRead: 5, output: 9 });
});

test("a stream cut before the reply finished is an error with what arrived, not a finished reply (all three formats)", async () => {
  const req = (model: ModelInfo, fetch: typeof globalThis.fetch): Request => ({ model, system: "", messages: [user("x")], tools: [], fetch, maxRetries: 0 });
  const chunk = (delta: object, extra: object = {}) => ({ id: "1", object: "chat.completion.chunk", created: 0, model: "m", choices: [{ index: 0, delta, finish_reason: null }], ...extra });
  const chat: ModelInfo = { id: "m", provider: "p", api: "openai-chat", baseUrl: "http://x/v1", contextWindow: 1e5, maxOutput: 1000 };
  // Thinking, then nothing: no finish reason, no usage chunk, no [DONE].
  const cutChat = fakeFetch(sse([chunk({ reasoning_content: "let me see" })]).replace("data: [DONE]\n\n", ""));
  const m1 = (await run(openaiChatProvider, req(chat, cutChat.fetch))).message;
  expect(m1.stopReason).toBe("error");
  expect(m1.errorMessage).toBe(INCOMPLETE_STREAM);
  expect(m1.content).toEqual([{ type: "thinking", thinking: "let me see" }]);
  // A server that never sends finish_reason but does send the usage chunk has finished.
  const usageOnly = fakeFetch(sse([chunk({ content: "hi" }), { ...chunk({}), choices: [], usage: { prompt_tokens: 3, completion_tokens: 1, total_tokens: 4 } }]));
  expect((await run(openaiChatProvider, req(chat, usageOnly.fetch))).message.stopReason).toBe("stop");

  const responses: ModelInfo = { id: "gpt-r", provider: "openai", api: "openai-responses", baseUrl: "http://x/v1", contextWindow: 1e5, maxOutput: 1000 };
  const cutResponses = fakeFetch(
    sse(
      [
        { type: "response.output_item.added", output_index: 0, item: { type: "message", id: "m1", role: "assistant", content: [] } },
        { type: "response.output_text.delta", item_id: "m1", output_index: 0, content_index: 0, delta: "par" },
      ],
      true,
    ),
  );
  const m2 = (await run(openaiResponsesProvider, req(responses, cutResponses.fetch))).message;
  expect([m2.stopReason, m2.errorMessage]).toEqual(["error", INCOMPLETE_STREAM]);

  const claude: ModelInfo = { id: "claude-x", provider: "anthropic", api: "anthropic", baseUrl: "http://x", contextWindow: 1e5, maxOutput: 1000 };
  const cutClaude = fakeFetch(
    sse(
      [
        { type: "message_start", message: { id: "m", type: "message", role: "assistant", model: "claude-x", content: [], stop_reason: null, usage: { input_tokens: 5, output_tokens: 0 } } },
        { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
        { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "par" } },
      ],
      true,
    ),
  );
  const m3 = (await run(anthropicProvider, { ...req(claude, cutClaude.fetch), apiKey: "k" })).message;
  expect([m3.stopReason, m3.errorMessage]).toEqual(["error", INCOMPLETE_STREAM]);
  expect(m3.content).toEqual([{ type: "text", text: "par" }]);
});
