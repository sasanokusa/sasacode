// End to end: the real CLI, as a child process, against a fake OpenAI-compatible server. Each test
// is a thing a user does, checked by what they would see (output, files, exit code) and by what
// the server was actually sent.
import { afterEach, expect, test } from "bun:test";
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { type FakeServer, fakeServer } from "./fake-server.ts";
import { answer, fakeConfig, roles, type Sandbox, sandbox } from "./harness.ts";

let server: FakeServer | undefined;
let box: Sandbox | undefined;
afterEach(() => {
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
