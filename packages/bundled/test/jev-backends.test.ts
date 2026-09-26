import { afterEach, expect, test } from "bun:test";
import type { PluginAPI } from "@sasacode/plugin-api";
import { detectService, makeBackend, parseVercel, type Question } from "../src/jev-backends.ts";

const realFetch = globalThis.fetch;
const saved = { ...process.env };
afterEach(() => {
  globalThis.fetch = realFetch;
  for (const k of ["CMD_API_KEY", "TYPESAFE_API_KEY", "OPENROUTER_API_KEY", "AI_GATEWAY_API_KEY", "MY_JEV_KEY"]) {
    if (saved[k] === undefined) delete process.env[k];
    else process.env[k] = saved[k];
  }
});

const questions: Record<string, Question> = {
  verdict: { type: "choice", instructions: "How?", criteria: { allow: "a", ask: "b", deny: "c" } },
  risky: { type: "noul", instructions: "Risky?", criteria: { true: "yes", false: "no" } },
};
const api = {} as PluginAPI;
const signal = new AbortController().signal;

function capture(reply: unknown, status = 200) {
  const sent: { url: string; headers: Record<string, string>; body: any }[] = [];
  globalThis.fetch = (async (url: string, init: RequestInit) => {
    sent.push({ url, headers: init.headers as Record<string, string>, body: JSON.parse(String(init.body)) });
    return new Response(JSON.stringify(reply), { status });
  }) as typeof fetch;
  return sent;
}

const native = {
  model: "jev-1.13.0",
  answers: {
    verdict: { type: "choice", choice: "ask", probabilities: { allow: 0.2, ask: 0.7, deny: 0.1 }, confidence: 0.8 },
    risky: { type: "noul", noul: 0.6 },
  },
};

test("native dialect: Command Code, TypeSafe and OpenRouter differ only in URL, model and key", async () => {
  const cases = [
    ["commandcode", "CMD_API_KEY", "https://api.commandcode.ai/provider/v1/systemone", "typesafe/jev"],
    ["typesafe", "TYPESAFE_API_KEY", "https://api.typesafe.ai/v1/systemone", "jev-latest"],
    ["openrouter", "OPENROUTER_API_KEY", "https://openrouter.ai/api/alpha/decisions", "typesafe/jev-latest"],
  ] as const;
  for (const [name, env, url, model] of cases) {
    process.env[env] = `key-${name}`;
    const sent = capture(native);
    const raw = await makeBackend(api, name, {}).ask({ call: "x" }, questions, signal);
    expect(sent[0]!.url).toBe(url);
    expect(sent[0]!.headers.authorization).toBe(`Bearer key-${name}`);
    expect(sent[0]!.body).toEqual({ model, state: { call: "x" }, questions });
    expect(raw).toEqual({ choice: { verdict: { choice: "ask", probabilities: { allow: 0.2, ask: 0.7, deny: 0.1 }, confidence: 0.8 } }, noul: { risky: 0.6 } });
  }
});

test("vercel dialect: model in a header, noul as boolean/probability, confidence out of band", async () => {
  process.env.AI_GATEWAY_API_KEY = "gw";
  const sent = capture({
    answers: {
      verdict: { type: "choice", choice: "allow", probabilities: { allow: 0.9, ask: 0.1, deny: 0 } },
      risky: { type: "boolean", probability: 0.05 },
    },
    providerMetadata: { typesafe: { confidence: { verdict: 0.85 } } },
  });
  const raw = await makeBackend(api, "vercel", {}).ask({ call: "x" }, questions, signal);
  expect(sent[0]!.url).toBe("https://ai-gateway.vercel.sh/v4/ai/evaluation-model");
  expect(sent[0]!.headers["ai-model-id"]).toBe("typesafe-ai/jev");
  expect(sent[0]!.body.model).toBeUndefined();
  expect(sent[0]!.body.questions.risky.type).toBe("boolean");
  expect(raw.choice.verdict!.confidence).toBe(0.85);
  expect(raw.noul.risky).toBe(0.05);
  // Without the reported confidence, the top probability stands in.
  const est = parseVercel({ answers: { verdict: { choice: "ask", probabilities: { allow: 0.3, ask: 0.6, deny: 0.1 } }, risky: { probability: 1 } } }, questions);
  expect(est.choice.verdict!.confidence).toBe(0.6);
});

test("settings override model, endpoint and key variable; a missing key is an error, not a guess", async () => {
  process.env.MY_JEV_KEY = "mine";
  const sent = capture(native);
  await makeBackend(api, "typesafe", { model: "jev-1.13.0", endpoint: "https://proxy.example/v1/systemone", apiKeyEnv: "MY_JEV_KEY" }).ask({}, questions, signal);
  expect(sent[0]).toMatchObject({ url: "https://proxy.example/v1/systemone", headers: { authorization: "Bearer mine" }, body: { model: "jev-1.13.0" } });
  delete process.env.MY_JEV_KEY;
  await expect(makeBackend(api, "typesafe", { apiKeyEnv: "MY_JEV_KEY" }).ask({}, questions, signal)).rejects.toThrow("MY_JEV_KEY");
  expect(() => makeBackend(api, "nope" as never, {})).toThrow("unknown backend");
});

test("errors: HTTP failures and malformed answers are reported, 5xx retried once", async () => {
  process.env.TYPESAFE_API_KEY = "k";
  let calls = 0;
  globalThis.fetch = (async () => {
    calls++;
    return new Response("overloaded", { status: 529 });
  }) as unknown as typeof fetch;
  await expect(makeBackend(api, "typesafe", {}).ask({}, questions, signal)).rejects.toThrow("HTTP 529");
  expect(calls).toBe(2);
  capture({ answers: { verdict: native.answers.verdict } });
  await expect(makeBackend(api, "typesafe", {}).ask({}, questions, signal)).rejects.toThrow("no answer for risky");
});

test("auto-detection picks Command Code, then TypeSafe; OpenRouter and Vercel only when named", async () => {
  for (const k of ["CMD_API_KEY", "TYPESAFE_API_KEY", "OPENROUTER_API_KEY", "AI_GATEWAY_API_KEY"]) process.env[k] = "";
  process.env.OPENROUTER_API_KEY = "or";
  process.env.TYPESAFE_API_KEY = "ts";
  expect(await detectService()).toBe("typesafe");
  process.env.CMD_API_KEY = "cc";
  expect(await detectService()).toBe("commandcode");
});

test("chat backend: any sasacode model answers the same questions as JSON", async () => {
  const asked: { model?: string; text: string }[] = [];
  const chatApi = {
    agent: {
      complete: async (o: { model?: string; messages: { content: { text: string }[] }[] }) => {
        asked.push({ model: o.model, text: o.messages[0]!.content[0]!.text });
        return 'Sure:\n```json\n{"verdict": "deny", "verdict_confidence": 0.9, "risky": true}\n```';
      },
    },
  } as unknown as PluginAPI;
  const raw = await makeBackend(chatApi, "chat", { model: "ollama/gemma4:e4b" }).ask({ call: { command: "rm -rf ~" } }, questions, signal);
  expect(asked[0]!.model).toBe("ollama/gemma4:e4b");
  expect(asked[0]!.text).toContain('"verdict": one of "allow", "ask", "deny"');
  expect(asked[0]!.text).toContain("rm -rf ~");
  expect(raw.choice.verdict!.choice).toBe("deny");
  expect(raw.choice.verdict!.probabilities.deny).toBe(0.9);
  expect(raw.choice.verdict!.probabilities.allow).toBeCloseTo(0.05);
  expect(raw.noul.risky).toBe(1);

  const bad = { agent: { complete: async () => '{"verdict": "maybe"}' } } as unknown as PluginAPI;
  await expect(makeBackend(bad, "chat", {}).ask({}, questions, signal)).rejects.toThrow('chose "maybe"');
  const none = { agent: { complete: async () => "I cannot help" } } as unknown as PluginAPI;
  await expect(makeBackend(none, "chat", {}).ask({}, questions, signal)).rejects.toThrow("no JSON");
});
