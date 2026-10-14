// End to end: the real CLI, as a child process, against a fake OpenAI-compatible server. Each test
// is a thing a user does, checked by what they would see (output, files, exit code) and by what
// the server was actually sent.
import { afterEach, expect, test } from "bun:test";
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { type FakeServer, fakeServer } from "./fake-server.ts";
import { answer, fakeConfig, type Live, roles, type Sandbox, sandbox } from "./harness.ts";

let server: FakeServer | undefined;
let box: Sandbox | undefined;
let lives: Live[] = [];
afterEach(() => {
  for (const l of lives) l.kill();
  lives = [];
  server?.stop();
  box?.cleanup();
  server = box = undefined;
});

function setup(script: Parameters<typeof fakeServer>[0], config: Record<string, unknown> = {}) {
  server = fakeServer(script);
  box = sandbox(fakeConfig(server.url, config));
  return { server, box };
}

test("a task that writes a file and checks it runs the real tools, and -r continues the conversation", async () => {
  const { server, box } = setup(
    [
      { reasoning: "I will write it.", toolCalls: [{ name: "write", args: { path: "hello.txt", content: "hi there\n" } }] },
      { toolCalls: [{ name: "read", args: { path: "hello.txt" } }] },
      { text: "Wrote hello.txt." },
      { text: "It says hi there." },
    ],
    { permissions: { mode: "edits" } },
  );
  const first = await box.run(["-p", "make hello.txt"]);
  expect(first.code).toBe(0);
  expect(first.stdout).toContain("Wrote hello.txt.");
  expect(readFileSync(join(box.project, "hello.txt"), "utf8")).toBe("hi there\n");
  // The model got each tool result back, in order.
  expect(roles(server.requests[1])).toEqual(["system", "user", "assistant", "tool"]);
  expect(JSON.stringify(server.requests[2].messages.at(-1))).toContain("hi there");

  const id = /sasacode -r (\S+)/.exec(first.stderr)?.[1];
  expect(id).toBeTruthy();
  const second = await box.run(["-r", id!, "-p", "what does it say?"]);
  expect(second.code).toBe(0);
  expect(second.stdout).toContain("It says hi there.");
  // The resumed run sent the whole earlier conversation.
  expect(JSON.stringify(server.requests[3].messages)).toContain("make hello.txt");
});

test("a reply cut off mid-stream is asked for again instead of ending the turn in silence", async () => {
  const { server, box } = setup([{ reasoning: "Let me think about the fi", cut: true }, { text: "The answer is 42." }]);
  const r = await box.run(["-p", "question"]);
  expect(r.code).toBe(0);
  expect(r.stdout).toContain("The answer is 42.");
  expect(r.stderr).toContain("the connection dropped mid-reply; asking again");
  expect(server.requests).toHaveLength(2);
});

test("a server that keeps cutting replies ends the run with an error, not a silent success", async () => {
  const cut = { reasoning: "hm", cut: true };
  const { box } = setup([cut, cut, cut, cut]);
  const r = await box.run(["-p", "question"]);
  expect(r.code).toBe(1);
  expect(r.stderr).toContain("the stream ended before the reply was complete");
}, 20_000);

test("an effort the server refuses inside a 200 stream (vLLM) is dropped and the request retried", async () => {
  const { server, box } = setup([{ streamError: "reasoning_effort 'none' is not supported for this model" }, { text: "ok" }]);
  const r = await box.run(["-p", "hi", "--thinking", "off"]);
  expect(r.code).toBe(0);
  expect(r.stdout).toContain("ok");
  expect(server.requests[0].reasoning_effort).toBe("none");
  expect(server.requests.at(-1).reasoning_effort).not.toBe("none");
});

test("a headless run never executes a call that needs approval, and says so", async () => {
  const { server, box } = setup([{ toolCalls: [{ name: "bash", args: { command: "touch made-by-bash" } }] }, { text: "done" }], {
    permissions: { mode: "ask" },
  });
  const r = await box.run(["-p", "run it"]);
  expect(existsSync(join(box.project, "made-by-bash"))).toBe(false);
  expect(JSON.stringify(server.requests[1].messages.at(-1))).toContain("needs user approval");
  expect(r.code).toBe(0);
});

test("a server error surfaces as an error and exit code 1", async () => {
  const { box } = setup([{ status: 500 }]);
  const r = await box.run(["-p", "hi"]);
  expect(r.code).toBe(1);
  expect(r.stderr.toLowerCase()).toContain("error");
});

test("a plugin installed from a local folder loads, and the model can call its tool", async () => {
  // A plugin tool that declares no paths needs approval in edits mode: the user allows it by rule.
  const { server, box } = setup([{ toolCalls: [{ name: "shout", args: { text: "hey" } }] }, { text: "done" }], { permissions: { allow: ["shout"] } });
  const dir = join(box.project, "..", "my-plugin");
  mkdirSync(dir, { recursive: true });
  writeFileSync(
    join(dir, "package.json"),
    JSON.stringify({ name: "sasacode-plugin-shout", version: "1.0.0", type: "module", files: ["index.ts"], sasacode: { name: "shout", apiVersion: "^1.4.0", extensions: ["index.ts"] } }),
  );
  writeFileSync(
    join(dir, "index.ts"),
    `export default (api) => api.registerTool({ name: "shout", description: "upper-case", kind: "read", parameters: { type: "object", properties: { text: { type: "string" } }, required: ["text"] }, execute: async ({ text }) => ({ content: [{ type: "text", text: text.toUpperCase() + "!" }] }) });\n`,
  );
  const install = await box.run(["plugin", "install", dir, "--yes"]);
  expect(install.code).toBe(0);
  const r = await box.run(["-p", "shout hey"]);
  expect(r.stderr).not.toContain("failed to load");
  expect(JSON.stringify(server.requests[1].messages.at(-1))).toContain("HEY!");
});

test("jsonl output carries the whole run, and a saved session is a file that -c finds again", async () => {
  const { box } = setup([{ text: "first" }, { text: "second" }]);
  const r = await box.jsonl("one");
  expect(r.code).toBe(0);
  expect(r.events[0].type).toBe("session");
  expect(answer(r.events)).toBe("first");
  expect(r.events.at(-1)).toEqual({ type: "agent_end", cause: "done" });
  const sessions = readdirSync(join(box.home, "sessions"));
  expect(sessions.length).toBe(1);
  const again = await box.run(["-c", "-p", "two"]);
  expect(again.stdout).toContain("second");
});

const control = ["--output", "jsonl", "--control", "stdio"];

test("--control stdio: the parent answers an approval request over stdin and the call runs", async () => {
  const { server, box } = setup([{ toolCalls: [{ name: "bash", args: { command: "touch made-by-bash" } }] }, { text: "done" }], { permissions: { mode: "ask" } });
  const run = box.start(["-p", "run it", ...control]);
  lives.push(run);
  const before = await run.until("approval_request");
  expect(before[0].type).toBe("session");
  const request = before.at(-1);
  expect(request).toEqual({ type: "approval_request", id: request.id, callId: expect.any(String), tool: "bash", args: { command: "touch made-by-bash" }, reason: expect.any(String), cwd: expect.any(String) });
  expect(existsSync(join(box.project, "made-by-bash"))).toBe(false); // nothing ran before the answer
  run.send({ type: "approval", id: request.id, decision: "allow" });
  const rest = await run.until("agent_end");
  expect(rest.at(-1)).toEqual({ type: "agent_end", cause: "done" });
  run.end();
  expect(await run.exited).toBe(0);
  expect(existsSync(join(box.project, "made-by-bash"))).toBe(true);
  expect(JSON.stringify(server.requests[1].messages.at(-1))).not.toContain("denied");
});

test("--control stdio: a deny reaches the model with its feedback; a bad line is reported and the run goes on", async () => {
  const { server, box } = setup([{ toolCalls: [{ name: "bash", args: { command: "touch made-by-bash" } }] }, { text: "ok" }], { permissions: { mode: "ask" } });
  const run = box.start(["-p", "run it", ...control]);
  lives.push(run);
  const { id } = (await run.until("approval_request")).at(-1);
  run.send("not a control message");
  run.send({ type: "approval", id, decision: "deny", feedback: "use the editor instead" });
  const rest = await run.until("agent_end");
  expect(rest.filter((e) => e.type === "control_error")).toHaveLength(1);
  expect(await run.exited).toBe(0);
  expect(existsSync(join(box.project, "made-by-bash"))).toBe(false);
  expect(JSON.stringify(server.requests[1].messages.at(-1))).toContain("User feedback: use the editor instead");
});

test("--control stdio: abort cancels the open request and ends the run as aborted", async () => {
  const { box } = setup([{ toolCalls: [{ name: "bash", args: { command: "touch made-by-bash" } }] }, { text: "unreachable" }], { permissions: { mode: "ask" } });
  const run = box.start(["-p", "run it", ...control]);
  lives.push(run);
  const { id } = (await run.until("approval_request")).at(-1);
  run.send({ type: "abort" });
  const rest = await run.until("agent_end");
  expect(rest.find((e) => e.type === "approval_cancelled")).toEqual({ type: "approval_cancelled", id });
  expect(rest.at(-1)).toEqual({ type: "agent_end", cause: "aborted" });
  expect(await run.exited).toBe(1);
  expect(existsSync(join(box.project, "made-by-bash"))).toBe(false);
});

test("--control stdio: a closed stdin denies approvals, and what is piped is not added to the prompt", async () => {
  const { server, box } = setup([{ toolCalls: [{ name: "bash", args: { command: "touch made-by-bash" } }] }, { text: "ok" }], { permissions: { mode: "ask" } });
  const r = await box.run(["-p", "run it", ...control], { stdin: "SECRET-LOG-LINE\n" });
  expect(r.code).toBe(0);
  const events = r.stdout.split("\n").filter((l) => l.trim()).map((l) => JSON.parse(l));
  expect(events.filter((e) => e.type === "control_error")).toEqual([{ type: "control_error", error: "not valid JSON" }]);
  expect(JSON.stringify(server.requests[0].messages)).not.toContain("SECRET-LOG-LINE");
  expect(existsSync(join(box.project, "made-by-bash"))).toBe(false);
  expect(JSON.stringify(server.requests[1].messages.at(-1))).toContain("the controlling process closed stdin");
});

test("--control needs stdio, -p and --output jsonl, and says so before anything runs", async () => {
  const { server, box } = setup([{ text: "never asked" }]);
  for (const [args, message] of [
    [["-p", "hi", "--control", "stdio"], "--control stdio needs -p and --output jsonl"],
    [["-p", "hi", "--output", "text", "--control", "stdio"], "--control stdio needs -p and --output jsonl"],
    [["--output", "jsonl", "--control", "stdio"], "--control stdio needs -p and --output jsonl"],
    [["-p", "hi", "--output", "jsonl", "--control", "tcp"], "--control must be stdio"],
  ] as const) {
    const r = await box.run([...args]);
    expect(r.code).toBe(1);
    expect(r.stderr).toContain(message);
    expect(r.stdout).toBe("");
  }
  expect(server.requests).toHaveLength(0);
});

test("--append-system-prompt-file adds the file's text after the config's instructions", async () => {
  const { server, box } = setup([{ text: "ok" }, { text: "ok" }], { instructions: "FROM-CONFIG" });
  const file = join(box.project, "..", "extra.txt");
  writeFileSync(file, "FROM-FILE persona: ひとこと\n");
  expect((await box.run(["-p", "hi", "--append-system-prompt-file", file])).code).toBe(0);
  const system = server.requests[0].messages[0];
  expect(system.role).toBe("system");
  const text = typeof system.content === "string" ? system.content : JSON.stringify(system.content);
  expect(text).toContain("FROM-CONFIG");
  expect(text.indexOf("FROM-CONFIG")).toBeLessThan(text.indexOf("FROM-FILE persona: ひとこと"));
  expect(text.indexOf("FROM-FILE")).toBeLessThan(text.indexOf("Environment:"));
  // An empty file adds nothing.
  const empty = join(box.project, "..", "empty.txt");
  writeFileSync(empty, "");
  expect((await box.run(["-p", "hi", "--append-system-prompt-file", empty])).code).toBe(0);
  expect(JSON.stringify(server.requests[1].messages[0])).not.toContain("FROM-FILE");
});

test("--append-system-prompt-file with a file that cannot be read fails before the run", async () => {
  const { server, box } = setup([{ text: "never asked" }]);
  for (const path of [join(box.project, "missing.txt"), box.project]) {
    const r = await box.run(["-p", "hi", "--append-system-prompt-file", path]);
    expect(r.code).toBe(1);
    expect(r.stderr).toContain(`failed to read ${path}`);
  }
  expect(server.requests).toHaveLength(0);
});
