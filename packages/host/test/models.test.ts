// The providers and models the app knows (moved out of the core's ai package): prices feed the cost line.
import { expect, test } from "bun:test";
import { computeCost, type Usage } from "@sasacode/ai";
import { resolveModel } from "../src/index.ts";

const usage = (u: Partial<Usage> = {}): Usage => ({ input: 0, output: 0, cacheRead: 0, cacheWrite: 0, cost: 0, ...u });

test("a priced model gets a cost from the table", () => {
  const m = resolveModel("openai/gpt-5.5", { openai: { api: "openai-responses" } });
  expect(m.price!.input).toBeGreaterThan(0);
  expect(computeCost(m, usage({ input: 1e6, output: 1e6 }))).toBeCloseTo(m.price!.input + m.price!.output);
  // Cache reads fall back to a tenth of the input price when the table lists none.
  expect(computeCost(m, usage({ cacheRead: 1e6 }))).toBeCloseTo(m.price!.input * 0.1);
});

test("a model override supplies the price resolveModel reports", () => {
  const m = resolveModel("myproxy/some-model", { myproxy: { api: "openai-chat" } }, { "myproxy/some-model": { price: { input: 1.25, output: 10 } } });
  expect(m.price).toEqual({ input: 1.25, output: 10 });
});
