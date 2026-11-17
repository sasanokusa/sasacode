// Writes request-bodies.json: for each adapter, the exact request body the TS sends for one rich request.
//   cd crates/sasacode-ai/tests/fixtures && bun request-bodies.probe.ts > request-bodies.json
import { getProvider, replayScope, type Message, type ModelInfo } from "../../../../packages/ai/src/index.ts";

const png = { type: "image" as const, mediaType: "image/png", data: "iVBORw0KGgo=" };
const tools = [{ name: "read", description: "read a file", parameters: { type: "object", properties: { path: { type: "string" }, limit: { type: "number", default: 2.5 } }, required: ["path"] } }];

function history(m: ModelInfo, signature: string | undefined): Message[] {
  return [
    { role: "user", content: [{ type: "text", text: "look at a.png" }, png], timestamp: 0 },
    {
      role: "assistant", api: m.api, provider: m.provider, model: m.id, replayScope: replayScope(m), timestamp: 0, stopReason: "tool_use",
      usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, cost: 0 },
      content: [
        { type: "thinking", thinking: "hmm", ...(signature ? { signature } : {}) },
        { type: "text", text: "reading" },
        { type: "tool_call", id: "c1", name: "read", input: { path: "a.png", limit: 3 } },
      ],
    },
    { role: "tool", toolCallId: "c1", toolName: "read", content: [{ type: "text", text: "Image a.png" }, png], isError: false, timestamp: 0 },
    { role: "user", content: [{ type: "text", text: "and now?   \"quoted\" é" }], timestamp: 0 },
  ];
}

const cases: [ModelInfo, string | undefined, Record<string, unknown>][] = [
  [{ id: "deepseek-v4-flash", provider: "p", api: "openai-chat", baseUrl: "https://mock.invalid/v1", contextWindow: 100000, maxOutput: 1000, reasoning: true },
   undefined, { stream_options: { include_usage: false, extra: 1 }, seed: 2, top_k: 40.5 }],
  [{ id: "gpt-5.5", provider: "openai", api: "openai-responses", baseUrl: "https://mock.invalid/v1", contextWindow: 100000, maxOutput: 1000, reasoning: true },
   JSON.stringify({ type: "reasoning", id: "rs0", summary: [], encrypted_content: "ENC" }), { store: true, seed: 2, top_k: 40.5 }],
  [{ id: "claude-opus-5", provider: "anthropic", api: "anthropic", contextWindow: 100000, maxOutput: 1000, reasoning: true },
   "sig", { max_tokens: 4096, seed: 2, top_k: 40.5 }],
];

const out: Record<string, string> = {};
for (const [m, signature, extraBody] of cases) {
  let raw = "";
  const fetch = (async (_url: any, init: any) => { raw = String(init.body); throw new Error("stop"); }) as unknown as typeof globalThis.fetch;
  try {
    for await (const _ of getProvider(m.api).stream({
      model: m, apiKey: "k", system: "sys", messages: history(m, signature), tools, thinking: "high", maxTokens: 500, maxRetries: 0, fetch,
      sampling: { temperature: 0.7, topP: 1, frequencyPenalty: 0.1, extraBody },
    })) { /* drain */ }
  } catch { /* the fetch stops it */ }
  out[m.api] = raw;
}
console.log(JSON.stringify(out, null, 2));
