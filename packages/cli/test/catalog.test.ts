import { afterAll, expect, test } from "bun:test";
import type { ProviderConfig } from "@sasacode/ai";
import { ModelCatalog } from "../src/catalog.ts";

// One fake OpenAI-compatible endpoint standing in for a multi-format service like Command Code.
const server = Bun.serve({
  port: 0,
  fetch(req) {
    if (req.headers.get("authorization") !== "Bearer k") return new Response("{}", { status: 401 });
    return Response.json({
      object: "list",
      data: [
        { id: "claude-x", object: "model", context_length: 1_000_000, supported_endpoints: ["/messages"] },
        { id: "gpt-y", object: "model", context_length: 400_000, supported_endpoints: ["/chat/completions", "/responses"] },
        { id: "oddball", object: "model", supported_endpoints: ["/systemone"] },
        { id: "text-embedding-3-small", object: "model" },
        { id: "plain", object: "model" },
      ],
    });
  },
});
afterAll(() => server.stop(true));
const base = `http://localhost:${server.port}/v1`;

const providers: Record<string, ProviderConfig> = {
  svc: { api: "openai-chat", baseUrl: base, apiKeyEnv: "SVC_KEY" },
  "svc-responses": { api: "openai-responses", baseUrl: base, apiKeyEnv: "SVC_KEY" },
  "svc-anthropic": { api: "anthropic", baseUrl: `http://localhost:${server.port}`, apiKeyEnv: "SVC_KEY" },
  nokey: { api: "openai-chat", baseUrl: "http://127.0.0.1:9/v1", apiKeyEnv: "MISSING_KEY" },
};

test("lists each service once, routes models to the provider speaking their endpoint, keeps context windows", async () => {
  const catalog = new ModelCatalog(providers, async (p) => (p.startsWith("svc") ? "k" : undefined), () => "svc");
  const specs = (await catalog.list()).map((m) => m.spec);
  expect(specs).toEqual(["svc-anthropic/claude-x", "svc/gpt-y", "svc/plain"]);
  expect(catalog.discovered.get("svc/gpt-y")).toEqual({ contextWindow: 400_000 });
  expect(catalog.discovered.get("svc-anthropic/claude-x")).toEqual({ contextWindow: 1_000_000 });
});

test("providers without a key are skipped, and failures just yield nothing", async () => {
  const bad = new ModelCatalog(providers, async () => "wrong", () => "none");
  expect(await bad.list()).toEqual([]); // 401 from the service, nokey unreachable
  let calls = 0;
  const cached = new ModelCatalog({ svc: providers.svc! }, async () => (calls++, "k"), () => "svc");
  await cached.list();
  await cached.list();
  expect(calls).toBe(1);
  await cached.list(true);
  expect(calls).toBe(2);
});
