import { expect, test } from "bun:test";
import { computeCost, type ModelInfo, listModels, parsePricing, resolveModel, type Usage } from "../src/index.ts";
import { parseChatUsage } from "../src/openai-chat.ts";

const usage = (u: Partial<Usage> = {}): Usage => ({ input: 0, output: 0, cacheRead: 0, cacheWrite: 0, cost: 0, ...u });
const model = (price?: ModelInfo["price"]): ModelInfo => ({ id: "m", provider: "p", api: "openai-chat", contextWindow: 1000, maxOutput: 100, price });

test("a priced model gets a cost from the table", () => {
  const m = resolveModel("openai/gpt-5.5", { openai: { api: "openai-responses" } });
  expect(m.price!.input).toBeGreaterThan(0);
  expect(computeCost(m, usage({ input: 1e6, output: 1e6 }))).toBeCloseTo(m.price!.input + m.price!.output);
  // Cache reads fall back to a tenth of the input price when the table lists none.
  expect(computeCost(m, usage({ cacheRead: 1e6 }))).toBeCloseTo(m.price!.input * 0.1);
});

test("a model without a price costs nothing computable", () => {
  expect(computeCost(model(undefined), usage({ input: 5, output: 7 }))).toBe(0);
});

test("a provider-reported cost beats the table estimate", () => {
  const u = parseChatUsage(model({ input: 1.25, output: 10 }), { prompt_tokens: 1_000_000, completion_tokens: 100_000, cost: 0.42 });
  expect(u.cost).toBe(0.42);
  expect(u.input).toBe(1_000_000);
  expect(u.output).toBe(100_000);
  expect(u.cacheRead).toBe(0);
});

test("a reported cost of zero means not reported: the estimate stands", () => {
  const m = model({ input: 1.25, output: 10 });
  const estimate = 1.25 + 1; // 1M input + 0.1M output at the table's prices
  expect(parseChatUsage(m, { prompt_tokens: 1_000_000, completion_tokens: 100_000, cost: 0 }).cost).toBeCloseTo(estimate);
  expect(parseChatUsage(m, { prompt_tokens: 1_000_000, completion_tokens: 100_000 }).cost).toBeCloseTo(estimate);
});

test("cached prompt tokens are parsed out and billed at the cache rate", () => {
  const u = parseChatUsage(model({ input: 1.25, output: 10 }), {
    prompt_tokens: 2_000_000,
    completion_tokens: 1,
    prompt_tokens_details: { cached_tokens: 1_000_000 },
  });
  expect(u.input).toBe(1_000_000);
  expect(u.cacheRead).toBe(1_000_000);
  expect(u.cost).toBeCloseTo(1.25 + 0.125 + 10 / 1e6);
});

test("a model override supplies the price resolveModel reports", () => {
  const m = resolveModel("myproxy/some-model", { myproxy: { api: "openai-chat" } }, { "myproxy/some-model": { price: { input: 1.25, output: 10 } } });
  expect(m.price).toEqual({ input: 1.25, output: 10 });
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
