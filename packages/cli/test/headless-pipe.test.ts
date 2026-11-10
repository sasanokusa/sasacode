// `sasacode -p … | slow reader`: the answer's tail must not be dropped when sasacode exits (#102).
// The reader sits in a shell pipeline: Bun.spawn reads a child's pipe eagerly, so it is never full.
import { afterAll, expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const MAIN = resolve(import.meta.dir, "../src/main.ts");
const NO_BUNFIG = resolve(import.meta.dir, "../../../scripts/no-bunfig.toml");
// About 200 KB: well past a pipe's 64 KB buffer.
const LINES = Array.from({ length: 6000 }, (_, i) => `line ${String(i).padStart(4, "0")} ${"x".repeat(24)}\n`);
const ANSWER = `${LINES.join("")}END OF ANSWER\n`;

let answered = 0;
const server = Bun.serve({
  port: 0,
  fetch(req) {
    const path = new URL(req.url).pathname;
    if (path === "/v1/models") return Response.json({ object: "list", data: [{ id: "m", object: "model", owned_by: "x" }] });
    if (path !== "/v1/chat/completions") return new Response("", { status: 404 });
    const chunk = (delta: object, finish: string | null = null) => ({ id: "c", object: "chat.completion.chunk", created: 0, model: "m", choices: [{ index: 0, delta, finish_reason: finish }] });
    const parts = [];
    for (let i = 0; i < ANSWER.length; i += 4000) parts.push(chunk({ content: ANSWER.slice(i, i + 4000) }));
    const body = [...parts, chunk({}, "stop")].map((e) => `data: ${JSON.stringify(e)}\n\n`).join("") + "data: [DONE]\n\n";
    answered = Date.now();
    return new Response(body, { headers: { "content-type": "text/event-stream" } });
  },
});
const root = mkdtempSync(join(tmpdir(), "sasacode-pipe-"));
afterAll(() => {
  server.stop(true);
  rmSync(root, { recursive: true, force: true });
});

/** Runs `sasacode -p` into a reader that starts only after sasacode has exited (or 1 s after the answer came). */
async function slowPipe(name: string, output: "text" | "jsonl") {
  const dir = join(root, name);
  const home = join(dir, "home");
  const proj = join(dir, "proj");
  mkdirSync(home, { recursive: true });
  mkdirSync(proj, { recursive: true });
  const fake = { api: "openai-chat", baseUrl: `http://127.0.0.1:${server.port}/v1` };
  writeFileSync(join(home, "config.json"), JSON.stringify({ providers: { fake }, model: "fake/m", plugins: { disabled: ["openai-codex"] } }));
  writeFileSync(join(home, ".env"), "");
  const go = join(dir, "go");
  const exited = join(dir, "exited");
  const script = `{ "$BUN" --no-env-file "--config=$NO_BUNFIG" "$MAIN" -p hi --no-session --output ${output} 2>/dev/null; echo $? > "$EXITED"; } | { while [ ! -e "$GO" ]; do sleep 0.02; done; cat; }`;
  const shell = Bun.spawn(["sh", "-c", script], {
    cwd: proj,
    env: { PATH: process.env.PATH ?? "", HOME: dir, SASACODE_HOME: home, SASACODE_LANG: "en", NO_COLOR: "1", BUN: process.execPath, NO_BUNFIG, MAIN, GO: go, EXITED: exited },
    stdout: "pipe",
    stderr: "ignore",
  });
  const text = new Response(shell.stdout).text();
  // Without the fix sasacode exits while most of the answer is still waiting for the reader; with it,
  // it waits for the reader, so the reader is let go after a while either way.
  for (let i = 0; i < 1000 && !existsSync(exited) && !(answered && Date.now() - answered > 1000); i++) await Bun.sleep(20);
  writeFileSync(go, "");
  await shell.exited;
  return { stdout: await text, code: readFileSync(exited, "utf8").trim() };
}

test.skipIf(process.platform === "win32")("-p into a slow pipe reader: the whole answer arrives, and jsonl ends with agent_end (#102)", async () => {
  const [plain, jsonl] = await Promise.all([slowPipe("text", "text"), slowPipe("jsonl", "jsonl")]);
  expect(plain.code).toBe("0");
  expect(plain.stdout.length).toBe(ANSWER.length);
  expect(plain.stdout.endsWith("END OF ANSWER\n")).toBe(true);
  expect(jsonl.code).toBe("0");
  const events = jsonl.stdout.trim().split("\n").map((l) => JSON.parse(l));
  expect(events.at(-1)).toEqual({ type: "agent_end", cause: "done" });
}, 30_000);
