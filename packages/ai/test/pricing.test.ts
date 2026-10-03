import { expect, test } from "bun:test";
import { computeCost, type ModelInfo, resolveModel, type Usage } from "../src/index.ts";
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
