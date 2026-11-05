// A local model's real context window is learned after its first reply (#61): a fake Ollama whose
// /api/ps is empty until the model has been asked something, as the real one is before loading.
import { afterAll, beforeAll, beforeEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setLang } from "@sasacode/host";
import { runHeadless } from "../src/headless.ts";
import { setup } from "../src/setup.ts";

beforeAll(() => setLang("en")); // these assertions quote the English strings

const enc = new TextEncoder();
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
      const { model } = (await req.json()) as { model: string };
      loaded = true; // Ollama loads the model to answer, with its own default length (no num_ctx)
      const chunk = (delta: object, finish: string | null = null) => ({ id: "c", object: "chat.completion.chunk", created: 0, model, choices: [{ index: 0, delta, finish_reason: finish }] });
      const events = [chunk({ content: "ok" }), chunk({}, "stop"), { id: "c", object: "chat.completion.chunk", created: 0, model, choices: [], usage: { prompt_tokens: 50, completion_tokens: 1, total_tokens: 51 } }];
      return new Response(events.map((e) => `data: ${JSON.stringify(e)}\n\n`).join("") + "data: [DONE]\n\n", { headers: { "content-type": "text/event-stream" } });
    }
    return new Response("", { status: 404 });
  },
});
const root = mkdtempSync(join(tmpdir(), "sasacode-ctx-"));
afterAll(() => {
  ollama.stop(true);
  rmSync(root, { recursive: true, force: true });
});

let proj = "";
function configure(extra: object = {}, model = "local/plain") {
  const dir = mkdtempSync(join(root, "t-"));
  const home = join(dir, "home");
  proj = join(dir, "proj");
  mkdirSync(home, { recursive: true });
  mkdirSync(proj, { recursive: true });
  process.env.SASACODE_HOME = home;
  const local = { api: "openai-chat", baseUrl: `http://localhost:${ollama.port}/v1`, ollama: true };
  writeFileSync(join(home, "config.json"), JSON.stringify({ providers: { local }, model, ...extra }));
}
beforeEach(() => {
  loaded = false;
  psHits = 0;
  listHits = 0;
  listDown = false;
});

/** stderr of `fn`, stdout dropped. */
async function quiet(fn: () => Promise<unknown>): Promise<string> {
  const out = process.stdout.write.bind(process.stdout);
  const err = process.stderr.write.bind(process.stderr);
  const said: string[] = [];
  process.stdout.write = (() => true) as typeof process.stdout.write;
  process.stderr.write = ((s: string) => (said.push(s), true)) as typeof process.stderr.write;
  try {
    await fn();
  } finally {
    process.stdout.write = out;
    process.stderr.write = err;
  }
  return said.join("");
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
  // Once: the next turns do not ask again.
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

test("headless: warned on stderr like the TUI when num_ctx is unset, then the real size is used", async () => {
  configure();
  const h = await setup({ cwd: proj, noSession: true });
  const err = await quiet(() => runHeadless(h, "hi", "text"));
  expect(err).toContain("warning: local/plain has no num_ctx");
  expect(err).toContain("this model goes up to 128k");
  expect(h.agent.model.contextWindow).toBe(8192);
});

test("headless: a model with num_ctx gets its size before the run, and no warning", async () => {
  configure({}, "local/small-32k");
  const h = await setup({ cwd: proj, noSession: true });
  const err = await quiet(() => runHeadless(h, "hi", "text"));
  expect(err).not.toContain("num_ctx");
  expect(h.agent.model.contextWindow).toBe(32768);
});
