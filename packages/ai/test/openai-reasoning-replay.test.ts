import { expect, test } from "bun:test";
import { openaiChatProvider } from "../src/openai-chat.ts";
import { replayScope } from "../src/replay.ts";
import { emptyUsage, type AssistantMessage, type Message, type Request } from "../src/types.ts";

const tool = { name: "add", description: "Add", parameters: { type: "object", properties: {} } };
const user: Message = { role: "user", content: [{ type: "text", text: "go" }], timestamp: 0 };
function assistant(model: string, calls = true, thoughts: string[] = ["first", "second"]): AssistantMessage {
  return { replayScope: replayScope({ id: model, provider: "test", api: "openai-chat", baseUrl: "https://mock.invalid/v1", contextWindow: 1e6, maxOutput: 1000 }), role: "assistant", api: "openai-chat", provider: "test", model, timestamp: 0, stopReason: calls ? "tool_use" : "stop", usage: emptyUsage(), content: [
    ...thoughts.map(thinking => ({ type: "thinking" as const, thinking })),
    { type: "text", text: calls ? "working" : "done" },
    ...(calls ? [1, 2].map(i => ({ type: "tool_call" as const, id: `call${i}`, name: "add", input: {} })) : []),
  ] };
}
function response(field = "reasoning_content") {
  const chunks = [
    { choices: [{ index: 0, delta: { [field]: "new-thought" }, finish_reason: null }] },
    { choices: [{ index: 0, delta: { content: "ok" }, finish_reason: "stop" }], usage: { prompt_tokens: 4, completion_tokens: 7, prompt_tokens_details: { cached_tokens: 0 } } },
  ];
  return new Response(chunks.map(c => `data: ${JSON.stringify(c)}\n\n`).join("") + "data: [DONE]\n\n", { headers: { "content-type": "text/event-stream" } });
}
async function run(id: string, messages: Message[], check: (b: any) => void | Response, tools = [tool], field = "reasoning_content") {
  let done: AssistantMessage | undefined;
  const fetchMock = (async (_url: any, init: any) => { const b = JSON.parse(init.body); return check(b) ?? response(field); }) as unknown as typeof fetch;
  const req: Request = { model: { id, provider: "test", api: "openai-chat", baseUrl: "https://mock.invalid/v1", reasoning: false, contextWindow: 1e6, maxOutput: 1000 }, apiKey: "fake", system: "system", messages, tools, maxRetries: 0, fetch: fetchMock };
  for await (const ev of openaiChatProvider.stream(req)) if (ev.type === "done") done = ev.message;
  expect(done?.content.find(c => c.type === "thinking" && c.thinking === "new-thought")).toBeDefined();
  return done!;
}
const results: Message[] = [1, 2].map(i => ({ role: "tool", toolCallId: `call${i}`, toolName: "add", content: [{ type: "text", text: "42" }], isError: false, timestamp: 0 }));

test("DeepSeek keeps complete reasoning for tool requests, including plain replies across user turns", async () => {
  for (const id of ["deepseek-flash", "deepseek-v4-flash", "deepseek-v4.1-flash", "deepseek/deepseek-v4.1-flash"]) {
    await run(id, [user, assistant(id), ...results, assistant(id, false, ["final thought"]), user], b => {
      const a = b.messages.filter((m: any) => m.role === "assistant");
      expect(a.map((m: any) => m.reasoning_content)).toEqual(["firstsecond", "final thought"]);
      expect(a[0].tool_calls.map((c: any) => c.id)).toEqual(["call1", "call2"]);
      expect(b.messages.filter((m: any) => m.role === "tool").map((m: any) => m.content)).toEqual(["42", "42"]);
    });
  }
});

test("DeepSeek without tools omits ignored reasoning", async () => {
  const id = "deepseek-flash";
  await run(id, [user, assistant(id, false), user], b => expect(b.messages[2]).not.toHaveProperty("reasoning_content"), []);
});

test("MiMo preserves tool-call reasoning across user turns, including when tools are subsequently absent", async () => {
  for (const id of ["mimo-v2.6-flash", "xiaomi/mimo-v2.6-flash"]) {
    for (const tools of [[tool], []]) {
      await run(id, [user, assistant(id), ...results, assistant(id, false), user], b => {
        const a = b.messages.filter((m: any) => m.role === "assistant");
        expect(a[0].reasoning_content).toBe("firstsecond");
        expect(a[1]).not.toHaveProperty("reasoning_content");
      }, tools, "reasoning");
    }
  }
});

test("required fields are empty for no-thinking messages; foreign and redacted thinking are not replayed", async () => {
  for (const id of ["deepseek-flash", "mimo-v2.6-flash"]) {
    const redacted = assistant(id); redacted.content = [{ type: "thinking", thinking: "do not replay", redacted: true, signature: "opaque" }, ...redacted.content.filter(c => c.type !== "thinking")];
    const foreign = assistant("other-model");
    const otherApi = assistant(id); otherApi.api = "anthropic";
    await run(id, [user, assistant(id, true, []), foreign, otherApi, redacted], b => {
      expect(b.messages.filter((m: any) => m.role === "assistant").map((m: any) => m.reasoning_content)).toEqual(["", "", "", ""]);
    });
  }
});

test("other models keep the standard OpenAI shape without vendor-only fields", async () => {
  for (const id of ["gpt-5.5", "claude-sonnet-5-5", "mimo-v2.6-flash-other", "deepseek-v4.1-flash-other", "deepseek-chat"]) {
    await run(id, [user, assistant(id), ...results], b => {
      expect(b.messages.filter((m: any) => m.role === "assistant").every((m: any) => !("reasoning_content" in m))).toBe(true);
    });
  }
});

test("a strict compatible server accepts preserved reasoning instead of returning missing-reasoning 400", async () => {
  const id = "deepseek-flash";
  await run(id, [user, assistant(id), ...results], b => {
    for (const m of b.messages) if (m.role === "assistant") {
      if (m.reasoning_content !== "firstsecond") return Response.json({ error: { message: "missing reasoning_content", type: "invalid_request_error" } }, { status: 400 });
      expect(m.reasoning_content).toBe("firstsecond");
    }
  });
});
