import { expect, test } from "bun:test";
import { listModels, parsePricing } from "../src/list-models.ts";

test("a models listing carries the provider's pricing", async () => {
  const body = {
    object: "list",
    data: [
      {
        id: "openai/gpt-5.5",
        object: "model",
        created: 0,
        owned_by: "openai",
        context_length: 400_000,
        pricing: { prompt: "0.0000015", completion: "0.000006" },
      },
      { id: "meta/llama-3.3-70b:free", object: "model", created: 0, owned_by: "meta", pricing: { prompt: "0", completion: "0" } },
    ],
  };
  const realFetch = globalThis.fetch;
  globalThis.fetch = (async () => new Response(JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } })) as unknown as typeof globalThis.fetch;
  try {
    const models = await listModels({ api: "openai-chat", baseUrl: "https://openrouter.ai/api/v1" }, "or-test");
    expect(models.map((m) => m.id)).toEqual(["openai/gpt-5.5", "meta/llama-3.3-70b:free"]);
    expect(models[0]!.price).toEqual({ input: 1.5, output: 6 });
    expect(models[0]!.contextWindow).toBe(400_000);
    expect(models[1]!.price).toBeUndefined();
  } finally {
    globalThis.fetch = realFetch;
  }
});

test("listing prices are per token as strings; the table is USD per 1M tokens", () => {
  expect(parsePricing({ prompt: "0.0000015", completion: "0.000006" })).toEqual({ input: 1.5, output: 6 });
  expect(parsePricing({ prompt: "0.0000025", completion: "0.00001", input_cache_read: "0.00000025", input_cache_write: "0.000003125" })).toEqual({
    input: 2.5,
    output: 10,
    cacheRead: 0.25,
    cacheWrite: 3.125,
  });
});

test("a zero price is unknown, not free: it never overrides a known one", () => {
  expect(parsePricing({ prompt: "0", completion: "0" })).toBeUndefined();
  expect(parsePricing({ prompt: "0" })).toBeUndefined();
  expect(parsePricing(undefined)).toBeUndefined();
});
