// A local model's real context window is learned after its first reply (#61): a fake Ollama whose
// /api/ps is empty until the model has been asked something, as the real one is before loading,
// and a fake LM Studio whose /api/v0/models has no loaded_context_length until then (#59).
import { afterAll, beforeAll, beforeEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setLang } from "@sasacode/host";
import { runHeadless } from "../src/headless.ts";
import { type Harness, setup } from "../src/setup.ts";

beforeAll(() => setLang("en")); // these assertions quote the English strings

/** A streamed Chat Completions answer, "ok", to the model `req` asks for. */
async function reply(req: Request): Promise<Response> {
  const { model } = (await req.json()) as { model: string };
  const chunk = (delta: object, finish: string | null = null) => ({ id: "c", object: "chat.completion.chunk", created: 0, model, choices: [{ index: 0, delta, finish_reason: finish }] });
  const events = [chunk({ content: "ok" }), chunk({}, "stop"), { id: "c", object: "chat.completion.chunk", created: 0, model, choices: [], usage: { prompt_tokens: 50, completion_tokens: 1, total_tokens: 51 } }];
  return new Response(events.map((e) => `data: ${JSON.stringify(e)}\n\n`).join("") + "data: [DONE]\n\n", { headers: { "content-type": "text/event-stream" } });
}

let loaded = false;
let psHits = 0;
let listHits = 0;
let listDown = false; // e.g. Ollama started after sasacode, or a refresh that fails
const ollama = Bun.serve({
  port: 0,
  async fetch(req) {
    const path = new URL(req.url).pathname;
    if (path === "/v1/models" && listDown) return new Response("", { status: 404 });
    // "ghost" never shows in /api/ps: its size stays unknown.
    if (path === "/v1/models") return listHits++, Response.json({ object: "list", data: ["plain", "small-32k", "ghost"].map((id) => ({ id, object: "model", owned_by: "library" })) });
    if (path === "/api/show") {
      const { model } = (await req.json()) as { model: string };
      return Response.json({ model_info: { "gemma4.context_length": 131072 }, parameters: model === "small-32k" ? "num_ctx 32768" : "" });
    }
    if (path === "/api/ps") {
      psHits++;
      return Response.json({ models: loaded ? [{ name: "plain", model: "plain", context_length: 8192 }] : [] });
    }
    if (path === "/v1/chat/completions") {
      loaded = true; // Ollama loads the model to answer, with its own default length (no num_ctx)
      return reply(req);
    }
    return new Response("", { status: 404 });
  },
});
// LM Studio loads a model at its first request, with its own default length, not the maximum.
let studioLoaded = false;
let studioDown = false; // not started yet when sasacode lists the models
let studioBodies: Record<string, unknown>[] = [];
let nativeHits = 0;
const lmstudio = Bun.serve({
  port: 0,
  async fetch(req) {
    const path = new URL(req.url).pathname;
    if (studioDown) return new Response("", { status: 503 });
    const ids = { qwen: 32768, big: 131072 };
    if (path === "/v1/models") return Response.json({ object: "list", data: Object.keys(ids).map((id) => ({ id, object: "model", owned_by: "organization_owner" })) });
    if (path === "/api/v0/models") {
      nativeHits++;
      const size = studioLoaded ? { state: "loaded", loaded_context_length: 4096 } : { state: "not-loaded" };
      return Response.json({ object: "list", data: Object.entries(ids).map(([id, max]) => ({ id, object: "model", type: "llm", max_context_length: max, ...size })) });
    }
    if (path === "/v1/chat/completions") {
      studioLoaded = true;
      studioBodies.push(await req.clone().json());
      return reply(req);
    }
    return new Response("", { status: 404 });
  },
});
// llama-server: one model, the size of each request slot in /props.
let cppListHits = 0;
const llamacpp = Bun.serve({
  port: 0,
  async fetch(req) {
    const path = new URL(req.url).pathname;
    if (path === "/v1/models") return cppListHits++, Response.json({ object: "list", data: [{ id: "qwen", object: "model", owned_by: "llamacpp", meta: { n_ctx_train: 131072 } }] });
    if (path === "/props") return Response.json({ default_generation_settings: { n_ctx: 16384 } });
    if (path === "/v1/chat/completions") return reply(req);
    return new Response("", { status: 404 });
  },
});
// A cloud gateway added by hand (no flag): no /api/v0/models, and a /v1/models that leaves out the
// model in use (an alias it still answers to) or is missing.
let gwList: "other" | "missing" = "other";
let gwHits = 0; // everything but chat
const gateway = Bun.serve({
  port: 0,
  async fetch(req) {
    const path = new URL(req.url).pathname;
    if (path === "/v1/chat/completions") return reply(req);
    gwHits++;
    if (path === "/v1/models" && gwList === "other") return Response.json({ object: "list", data: [{ id: "flash-v2", object: "model", owned_by: "gw" }] });
    return new Response("", { status: 404 });
  },
});
const studioUrl = `http://localhost:${lmstudio.port}`;
const root = mkdtempSync(join(tmpdir(), "sasacode-ctx-"));
afterAll(() => {
  [ollama, lmstudio, llamacpp, gateway].forEach((s) => s.stop(true));
  rmSync(root, { recursive: true, force: true });
  delete process.env.SASACODE_TEST_GW_KEY;
});

let home = "";
let proj = "";
function configure(extra: object = {}, model = "local/plain", providers: object = {}) {
  const dir = mkdtempSync(join(root, "t-"));
  home = join(dir, "home");
  proj = join(dir, "proj");
  mkdirSync(home, { recursive: true });
  mkdirSync(proj, { recursive: true });
  process.env.SASACODE_HOME = home;
  const local = { api: "openai-chat", baseUrl: `http://localhost:${ollama.port}/v1`, ollama: true };
  writeFileSync(join(home, "config.json"), JSON.stringify({ providers: { local, ...providers }, model, ...extra }));
}
beforeEach(() => {
  loaded = false;
  psHits = 0;
  listHits = 0;
  listDown = false;
  studioLoaded = false;
  studioDown = false;
  studioBodies = [];
  nativeHits = 0;
  cppListHits = 0;
  gwList = "other";
  gwHits = 0;
});

/** stderr of `fn`, stdout dropped. */
async function quiet(fn: () => Promise<unknown>): Promise<string> {
  const out = process.stdout.write.bind(process.stdout);
  const err = process.stderr.write.bind(process.stderr);
  const said: string[] = [];
  process.stdout.write = ((_: string, cb?: () => void) => (cb?.(), true)) as typeof process.stdout.write;
  process.stderr.write = ((s: string, cb?: () => void) => (said.push(s), cb?.(), true)) as typeof process.stderr.write;
  try {
    await fn();
  } finally {
    process.stdout.write = out;
    process.stderr.write = err;
  }
  return said.join("");
}

/** The context window the model plans each turn with (taken when the turn starts). */
function turnSizes(h: Harness): number[] {
  const sizes: number[] = [];
  h.agent.events.on((e) => void (e.type === "turn_start" && sizes.push(h.agent.model.contextWindow)));
  return sizes;
}

test("listed before loading, the model plans with the default; after its first reply, with what Ollama loaded it with", async () => {
  configure();
  const h = await setup({ cwd: proj, noSession: true });
  expect((await h.listModels()).find((m) => m.spec === "local/plain")).toMatchObject({ contextWindow: undefined, maxContext: 131072, server: "ollama" });
  expect(h.agent.model.contextWindow).toBe(128_000);
  // A plugin's turn_end hook (compaction's threshold) runs after the size is learned, in the same turn.
  let seenByPlugins = 0;
  h.agent.hooks.on("turn_end", () => void (seenByPlugins = h.agent.model.contextWindow), "later-plugin");
  await h.agent.prompt("hi");
  expect(h.agent.model.contextWindow).toBe(8192);
  expect(seenByPlugins).toBe(8192);
  expect((await h.listModels()).find((m) => m.spec === "local/plain")?.contextWindow).toBe(8192); // /model shows it too
  // Once known, the next turns do not ask again.
  const hits = psHits;
  await h.agent.prompt("again");
  expect(psHits).toBe(hits);
  await h.shutdown();
});

test("a server that could not list the model yet (down at startup, a failed refresh) is asked again at a later turn's end", async () => {
  configure();
  listDown = true;
  const h = await setup({ cwd: proj, noSession: true });
  expect((await h.listModels()).find((m) => m.spec === "local/plain")).toBeUndefined();
  await h.agent.prompt("hi"); // the reply comes, but the refresh at its end fails too
  expect(h.agent.model.contextWindow).toBe(128_000);
  listDown = false;
  await h.agent.prompt("again");
  expect(h.agent.model.contextWindow).toBe(8192);
  await h.shutdown();
});

test("a size that stays unknown is asked at three turns' ends, and no more", async () => {
  configure({}, "local/ghost");
  const h = await setup({ cwd: proj, noSession: true });
  await h.listModels();
  const hits = listHits;
  for (const n of [1, 2, 3, 4, 5]) await h.agent.prompt(`turn ${n}`);
  expect(listHits - hits).toBe(3);
  expect(h.agent.model.contextWindow).toBe(128_000);
  await h.shutdown();
});

test("a contextWindow set in modelOverrides is kept, and the server is not asked again for it", async () => {
  configure({ modelOverrides: { "local/plain": { contextWindow: 1234 } } });
  const h = await setup({ cwd: proj, noSession: true });
  await h.listModels();
  const hits = psHits;
  await h.agent.prompt("hi");
  expect(h.agent.model.contextWindow).toBe(1234);
  expect(psHits).toBe(hits);
  await h.shutdown();
});

test("LM Studio: a model not loaded yet shows its maximum; after its first reply it plans with the length LM Studio loaded it with (#59)", async () => {
  configure({}, "studio/qwen", { studio: { api: "openai-chat", baseUrl: `${studioUrl}/v1`, lmstudio: true } });
  const h = await setup({ cwd: proj, noSession: true });
  expect((await h.listModels()).find((m) => m.spec === "studio/qwen")).toEqual({ spec: "studio/qwen", contextWindow: undefined, maxContext: 32768, server: "lmstudio" });
  // Not the default 128K: whatever LM Studio loads it with, it is no more than the maximum.
  expect(h.agent.model.contextWindow).toBe(32768);
  await h.agent.prompt("hi");
  expect(h.agent.model.contextWindow).toBe(4096);
  expect((await h.listModels()).find((m) => m.spec === "studio/qwen")?.contextWindow).toBe(4096);
  // Unloaded later (LM Studio's idle TTL): the length it was loaded with stays the better guess.
  studioLoaded = false;
  await h.listModels(true);
  expect(h.agent.model.contextWindow).toBe(4096);
  await h.shutdown();
});

// Added before sasacode told LM Studio apart: `endpoint add` wrote { api, baseUrl } and no flag, and
// a URL-only entry was detected and cached in endpoints.json the same way.
for (const [how, studio] of [
  ["added with endpoint add", { api: "openai-chat", baseUrl: `${studioUrl}/v1` }],
  ["URL-only, detected and cached", { baseUrl: studioUrl }],
] as const)
  test(`LM Studio set up before it was told apart (${how}) is recognised when listed (#59)`, async () => {
    configure({}, "studio/qwen", { studio });
    writeFileSync(join(home, "endpoints.json"), JSON.stringify({ [studioUrl]: { api: "openai-chat", baseUrl: `${studioUrl}/v1` } }));
    const h = await setup({ cwd: proj, noSession: true });
    expect((await h.listModels()).find((m) => m.spec === "studio/qwen")).toMatchObject({ maxContext: 32768, server: "lmstudio" });
    expect(nativeHits).toBe(1); // the list that told it apart is the one the sizes come from
    await h.agent.prompt("hi");
    expect(h.agent.model.contextWindow).toBe(4096);
    await h.shutdown();
  });

test("LM Studio set up before it was told apart gets max_tokens from its first request once listed, whatever the model's maximum (#59)", async () => {
  configure({}, "studio/big", { studio: { api: "openai-chat", baseUrl: `${studioUrl}/v1` } });
  const h = await setup({ cwd: proj, noSession: true });
  await h.listModels();
  // Its maximum (131072) is above the default: no size is learned, but the server is.
  expect(h.agent.model).toMatchObject({ server: "lmstudio", contextWindow: 128_000 });
  await h.agent.prompt("hi");
  expect(studioBodies[0]).toHaveProperty("max_tokens");
  expect(studioBodies[0]).not.toHaveProperty("max_completion_tokens");
  await h.shutdown();
});

test("LM Studio set up before it was told apart and down when first listed is recognised at a turn's end (#59)", async () => {
  configure({}, "studio/qwen", { studio: { api: "openai-chat", baseUrl: `${studioUrl}/v1` } });
  studioDown = true;
  const h = await setup({ cwd: proj, noSession: true });
  expect((await h.listModels()).find((m) => m.spec === "studio/qwen")).toBeUndefined();
  studioDown = false;
  await h.agent.prompt("hi");
  expect(h.agent.model).toMatchObject({ contextWindow: 4096, server: "lmstudio" });
  await h.shutdown();
});

test("a custom server not told apart that has listed the model is not LM Studio, and is not listed again at turns' ends", async () => {
  configure({}, "cpp/qwen", { cpp: { api: "openai-chat", baseUrl: `http://localhost:${llamacpp.port}/v1` } });
  const h = await setup({ cwd: proj, noSession: true });
  expect((await h.listModels()).find((m) => m.spec === "cpp/qwen")).toMatchObject({ contextWindow: undefined });
  const hits = cppListHits;
  await h.agent.prompt("hi");
  await h.agent.prompt("again");
  expect(cppListHits).toBe(hits);
  expect(h.agent.model.server).toBeUndefined();
  await h.shutdown();
});

for (const list of ["other", "missing"] as const)
  test(`a custom server not told apart whose list ${list === "other" ? "leaves out the model" : "is missing"} is listed again once at a turn's end, not at each (#59)`, async () => {
    process.env.SASACODE_TEST_GW_KEY = "test-key";
    configure({}, "gw/flash", { gw: { api: "openai-chat", baseUrl: `http://localhost:${gateway.port}/v1`, apiKeyEnv: "SASACODE_TEST_GW_KEY" } });
    gwList = list;
    const h = await setup({ cwd: proj, noSession: true });
    await h.listModels();
    const hits = gwHits;
    await h.agent.prompt("hi"); // it has just answered, so it is up: listed once more
    expect(gwHits - hits).toBe(2); // /api/v0/models and /v1/models
    for (const n of [2, 3, 4]) await h.agent.prompt(`turn ${n}`);
    // Telling a server apart is the provider's: another of its models is not listed again either.
    h.agent.model = h.resolve("gw/flash-v3");
    await h.agent.prompt("other model");
    expect(gwHits - hits).toBe(2);
    expect(h.agent.model.server).toBeUndefined();
    await h.shutdown();
  });

test("headless: LM Studio set up before it was told apart also learns the loaded length", async () => {
  configure({}, "studio/qwen", { studio: { api: "openai-chat", baseUrl: `${studioUrl}/v1` } });
  const h = await setup({ cwd: proj, noSession: true });
  await quiet(() => runHeadless(h, "hi", "text"));
  expect(h.agent.model.contextWindow).toBe(4096);
});

test("headless: warned on stderr like the TUI when num_ctx is unset, then the real size is used", async () => {
  configure();
  const h = await setup({ cwd: proj, noSession: true });
  const err = await quiet(() => runHeadless(h, "hi", "text"));
  expect(err).toContain("warning: local/plain has no num_ctx");
  expect(err).toContain("this model goes up to 128k");
  expect(h.agent.model.contextWindow).toBe(8192);
});

test("headless: a model with num_ctx plans its first turn with that size, and gets no warning", async () => {
  configure({}, "local/small-32k");
  const h = await setup({ cwd: proj, noSession: true });
  const sizes = turnSizes(h);
  const err = await quiet(() => runHeadless(h, "hi", "text"));
  expect(err).not.toContain("num_ctx");
  expect(sizes).toEqual([32768]);
});

test("headless: llama.cpp's slot size from /props is used from the first turn", async () => {
  configure({}, "cpp/qwen", { cpp: { api: "openai-chat", baseUrl: `http://localhost:${llamacpp.port}/v1`, llamacpp: true } });
  const h = await setup({ cwd: proj, noSession: true });
  const sizes = turnSizes(h);
  await quiet(() => runHeadless(h, "hi", "text"));
  expect(sizes).toEqual([16384]);
});
