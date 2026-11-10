// /fork writes the kept conversation into a new log before any prompt: the keys the old log redacted must stay redacted (#103).
import { afterAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setup } from "../src/setup.ts";

const KEY = "sk-test-0123456789abcdefSECRET";
const server = Bun.serve({
  port: 0,
  async fetch(req) {
    const path = new URL(req.url).pathname;
    if (path === "/v1/models") return Response.json({ object: "list", data: [{ id: "m", object: "model", owned_by: "x" }] });
    if (path !== "/v1/chat/completions") return new Response("", { status: 404 });
    const chunk = (delta: object, finish: string | null = null) => ({ id: "c", object: "chat.completion.chunk", created: 0, model: "m", choices: [{ index: 0, delta, finish_reason: finish }] });
    const body = [chunk({ content: "ok" }), chunk({}, "stop")].map((e) => `data: ${JSON.stringify(e)}\n\n`).join("") + "data: [DONE]\n\n";
    return new Response(body, { headers: { "content-type": "text/event-stream" } });
  },
});
const root = mkdtempSync(join(tmpdir(), "sasacode-fork-"));
afterAll(() => {
  server.stop(true);
  rmSync(root, { recursive: true, force: true });
  delete process.env.SASACODE_TEST_FORK_KEY;
});

test("a forked log redacts the key the original log redacted (#103)", async () => {
  const home = join(root, "home"), proj = join(root, "proj");
  mkdirSync(home, { recursive: true });
  mkdirSync(proj, { recursive: true });
  process.env.SASACODE_HOME = home;
  process.env.SASACODE_TEST_FORK_KEY = KEY;
  const fake = { api: "openai-chat", baseUrl: `http://localhost:${server.port}/v1`, apiKeyEnv: "SASACODE_TEST_FORK_KEY" };
  writeFileSync(join(home, "config.json"), JSON.stringify({ providers: { fake }, model: "fake/m", plugins: { disabled: ["openai-codex"] } }));
  const h = await setup({ cwd: proj });
  try {
    await h.agent.prompt(`why is this rejected? FAKE_PROVIDER_KEY=${KEY}`);
    const original = readFileSync(h.agent.session!.path, "utf8");
    expect(original).not.toContain(KEY);
    await h.fork(h.agent.messages.length);
    const forked = h.agent.session!.path;
    expect(readdirSync(join(forked, "..")).length).toBe(2);
    const log = readFileSync(forked, "utf8");
    expect(log).toContain("[REDACTED]");
    expect(log).not.toContain(KEY);
  } finally {
    await h.shutdown();
  }
});
