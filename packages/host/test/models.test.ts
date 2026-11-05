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

test("a provider's local-server flag reaches the model, whatever the provider is called (#58)", () => {
  const providers = { myollama: { api: "openai-chat", baseUrl: "http://host:11434/v1", ollama: true }, cpp: { api: "openai-chat", llamacpp: true }, studio: { api: "openai-chat", lmstudio: true }, cloud: { api: "openai-chat", baseUrl: "https://api.commandcode.ai/provider/v1" } } as const;
  expect(resolveModel("myollama/gemma4:e4b", providers).server).toBe("ollama");
  expect(resolveModel("cpp/qwen", providers).server).toBe("llamacpp");
  expect(resolveModel("studio/qwen", providers).server).toBe("lmstudio");
  expect("server" in resolveModel("cloud/deepseek/deepseek-v4-flash", providers)).toBe(false);
  expect(resolveModel("cloud/x", providers, { "cloud/x": { server: "llamacpp" } }).server).toBe("llamacpp");
});

test("a model override supplies the price resolveModel reports", () => {
  const m = resolveModel("myproxy/some-model", { myproxy: { api: "openai-chat" } }, { "myproxy/some-model": { price: { input: 1.25, output: 10 } } });
  expect(m.price).toEqual({ input: 1.25, output: 10 });
});
