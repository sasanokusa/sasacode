import { afterAll, beforeAll, beforeEach, expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { ApprovalAnswer, ApprovalRequest } from "@sasacode/agent";
import { type AssistantMessage, emptyUsage, registerApi } from "@sasacode/ai";
import { setLang } from "@sasacode/host";
import { replayProvider } from "@sasacode/testing";
import { type ControlInput, controlMode, startControl } from "../src/control.ts";
import { runHeadless } from "../src/headless.ts";
import { setup } from "../src/setup.ts";

beforeAll(() => setLang("en")); // these assertions quote the English strings

const root = mkdtempSync(join(tmpdir(), "sasacode-control-"));
const home = join(root, "home");
const proj = join(root, "proj");
afterAll(() => rmSync(root, { recursive: true, force: true }));
beforeEach(() => {
  process.env.SASACODE_HOME = home;
  mkdirSync(home, { recursive: true });
  mkdirSync(proj, { recursive: true });
  writeFileSync(join(home, "config.json"), JSON.stringify({ providers: { test: { api: "replay" } }, model: "test/m", permissions: { mode: "ask" } }));
  for (const f of ["allowed", "denied", "cancelled", "never"]) rmSync(join(proj, f), { force: true });
});

/** A control channel the test writes to, line by line, like a parent process. */
function channel() {
  const queue: (string | Uint8Array)[] = [];
  let wake: (() => void) | undefined;
  let ended = false;
  const input: ControlInput = (async function* () {
    for (;;) {
      if (queue.length) yield queue.shift()!;
      else if (ended) return;
      else await new Promise<void>((r) => (wake = r));
    }
  })();
  return {
    input,
    /** Raw chunk: the caller adds the newline. */
    raw(chunk: string | Uint8Array) {
      queue.push(chunk);
      wake?.();
    },
    send(message: unknown) {
      this.raw(`${typeof message === "string" ? message : JSON.stringify(message)}\n`);
    },
    end() {
      ended = true;
      wake?.();
    },
  };
}

const tick = () => new Promise((r) => setTimeout(r, 0));

// ── the protocol, against a stand-in agent ───────────────────────────

function rig() {
  const ch = channel();
  const out: any[] = [];
  const agent = { aborts: 0, abort() { this.aborts++; }, approve: undefined as ((r: ApprovalRequest) => Promise<ApprovalAnswer>) | undefined };
  const control = startControl(agent, ch.input, (m) => out.push(m));
  const ask = (over: Record<string, unknown> = {}) =>
    agent.approve!({ tool: { name: "bash" }, args: { command: "ls" }, cwd: "/work", call: { type: "tool_call", id: "call_1", name: "bash", input: {} }, reason: "needs approval", ...over } as unknown as ApprovalRequest);
  return { ch, out, agent, control, ask };
}

test("an approval request is one JSON line; the answer with its id settles it", async () => {
  const { ch, out, ask } = rig();
  const first = ask();
  const second = ask({ call: { type: "tool_call", id: "call_2", name: "bash", input: {} }, args: { command: "pwd" } });
  expect(out).toHaveLength(2);
  expect(out[0]).toEqual({ type: "approval_request", id: out[0].id, callId: "call_1", tool: "bash", args: { command: "ls" }, reason: "needs approval", cwd: "/work" });
  expect(out[1].callId).toBe("call_2");
  expect(out[0].id).not.toBe(out[1].id);
  // Answered out of order: each answer reaches its own request.
  ch.send({ type: "approval", id: out[1].id, decision: "deny", feedback: "no pwd" });
  ch.send({ type: "approval", id: out[0].id, decision: "allow" });
  expect(await second).toEqual({ decision: "deny", feedback: "no pwd" });
  expect(await first).toEqual({ decision: "allow" });
  expect(out).toHaveLength(2); // nothing else was written
  ch.send({ type: "approval", id: out[0].id, decision: "always" }); // a second answer to the same id
  await tick();
  expect(out[2]).toEqual({ type: "control_error", error: expect.stringContaining("no pending approval") });
});

test('"always" and an empty feedback are passed on as sent', async () => {
  const { ch, out, ask } = rig();
  const p = ask();
  ch.send({ type: "approval", id: out[0].id, decision: "always", feedback: "" });
  expect(await p).toEqual({ decision: "always", feedback: "" });
});

test("abort calls agent.abort()", async () => {
  const { ch, agent } = rig();
  ch.send({ type: "abort" });
  await tick();
  expect(agent.aborts).toBe(1);
});

test("a line the channel cannot use is a control_error, and the channel goes on", async () => {
  const { ch, out, agent, ask } = rig();
  const p = ask();
  const bad = [
    "not json",
    "[1,2]",
    "null",
    '{"type":"nope"}',
    '{"id":"x"}',
    '{"type":"approval","decision":"allow"}',
    '{"type":"approval","id":"x","decision":"maybe"}',
    `{"type":"approval","id":${JSON.stringify(out[0].id)},"decision":"allow","feedback":3}`,
    '{"type":"approval","id":"nobody","decision":"allow"}',
  ];
  for (const line of bad) ch.send(line);
  ch.send("   "); // blank lines are not messages
  await tick();
  const errors = out.slice(1);
  expect(errors).toHaveLength(bad.length);
  for (const e of errors) expect(e).toEqual({ type: "control_error", error: expect.any(String) });
  expect(errors[0].error).toBe("not valid JSON");
  expect(errors[3].error).toBe('unknown message type "nope"');
  expect(errors[8].error).toContain('"nobody"');
  // The request is still open, and the channel still works.
  ch.send({ type: "approval", id: out[0].id, decision: "allow" });
  expect(await p).toEqual({ decision: "allow" });
  ch.send({ type: "abort" });
  await tick();
  expect(agent.aborts).toBe(1);
});

test("lines are cut from chunks: split anywhere, CRLF, multi-byte text, a last line without a newline", async () => {
  const { ch, out, agent, ask } = rig();
  const p = ask();
  const line = Buffer.from(`${JSON.stringify({ type: "approval", id: out[0].id, decision: "deny", feedback: "だめ" })}\r\n`);
  const cut = line.indexOf(Buffer.from("だ")) + 1; // in the middle of a character
  ch.raw(line.subarray(0, cut));
  await tick();
  expect(agent.aborts).toBe(0);
  ch.raw(line.subarray(cut));
  expect(await p).toEqual({ decision: "deny", feedback: "だめ" });
  ch.raw('{"type":"abort"}'); // no newline, then EOF
  ch.end();
  await tick();
  expect(agent.aborts).toBe(1);
});

test("when the request's signal aborts first: approval_cancelled, a deny, and the id is gone", async () => {
  const { ch, out, ask } = rig();
  const controller = new AbortController();
  const p = ask({ signal: controller.signal });
  const id = out[0].id;
  controller.abort();
  expect(await p).toEqual({ decision: "deny" }); // what the TUI answers when its dialog is cancelled
  expect(out[1]).toEqual({ type: "approval_cancelled", id });
  ch.send({ type: "approval", id, decision: "allow" });
  await tick();
  expect(out[2].type).toBe("control_error");
  // Already aborted when asked: nobody was asked, so there is nothing to cancel.
  const done = new AbortController();
  done.abort();
  expect(await ask({ signal: done.signal })).toEqual({ decision: "deny" });
  expect(out).toHaveLength(3);
});

test("an answer removes the abort listener; abort after it writes nothing", async () => {
  const { ch, out, ask } = rig();
  const controller = new AbortController();
  const p = ask({ signal: controller.signal });
  ch.send({ type: "approval", id: out[0].id, decision: "allow" });
  await p;
  controller.abort();
  expect(out).toHaveLength(1);
});

test("stdin closing denies every pending request and every later one, and does not abort", async () => {
  const { ch, out, agent, ask } = rig();
  const waiting = ask();
  const alsoWaiting = ask();
  ch.end();
  const closed: ApprovalAnswer = { decision: "deny", feedback: "the controlling process closed stdin" };
  expect(await waiting).toEqual(closed);
  expect(await alsoWaiting).toEqual(closed);
  expect(await ask()).toEqual(closed);
  expect(out.map((m) => m.type)).toEqual(["approval_request", "approval_request"]); // the later one was not even announced
  expect(agent.aborts).toBe(0);
});

test("a read that fails ends the channel like EOF", async () => {
  const out: any[] = [];
  const agent = { abort() {}, approve: undefined as ((r: ApprovalRequest) => Promise<ApprovalAnswer>) | undefined };
  startControl(agent, (async function* () { throw new Error("EPIPE"); })(), (m) => out.push(m));
  await tick();
  const answer = await agent.approve!({ tool: { name: "bash" }, args: {}, cwd: "/", call: { id: "c" }, reason: "r" } as unknown as ApprovalRequest);
  expect(answer.decision).toBe("deny");
  expect(out).toEqual([]);
});

test("--control is stdio only, and only with -p and --output jsonl", () => {
  expect(controlMode(undefined, "hi", "text")).toBe(false);
  expect(controlMode(undefined, undefined, "text")).toBe(false);
  expect(controlMode("stdio", "hi", "jsonl")).toBe(true);
  expect(() => controlMode("stdio", "hi", "text")).toThrow("--control stdio needs -p and --output jsonl");
  expect(() => controlMode("stdio", undefined, "jsonl")).toThrow("--control stdio needs -p and --output jsonl");
  expect(() => controlMode("stdio", undefined, "text")).toThrow("--control stdio needs -p and --output jsonl");
  expect(() => controlMode("tcp", "hi", "jsonl")).toThrow("--control must be stdio");
  expect(() => controlMode("", "hi", "jsonl")).toThrow("--control must be stdio");
});

// ── a real run, with a scripted model, driven over the channel ───────

const reply = (content: AssistantMessage["content"]): AssistantMessage => ({
  role: "assistant",
  content,
  api: "replay",
  provider: "test",
  model: "m",
  usage: emptyUsage(),
  stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop",
  timestamp: 0,
});
const bash = (id: string, command: string) => reply([{ type: "tool_call", id, name: "bash", input: { command } }]);
const text = (t: string) => reply([{ type: "text", text: t }]);

/** runHeadless in jsonl mode with the channel on; `react` sees every stdout line as it is written. */
async function drive(script: AssistantMessage[], react: (e: any, ch: ReturnType<typeof channel>) => void) {
  const provider = replayProvider(script);
  registerApi("replay", provider);
  const h = await setup({ cwd: proj, noSession: true });
  const ch = channel();
  const events: any[] = [];
  const orig = process.stdout.write.bind(process.stdout);
  process.stdout.write = ((s: string) => {
    expect(s.endsWith("\n") && s.indexOf("\n") === s.length - 1).toBe(true); // one write, one line
    const e = JSON.parse(s);
    events.push(e);
    react(e, ch);
    return true;
  }) as typeof process.stdout.write;
  let code: number;
  try {
    code = await runHeadless(h, "go", "jsonl", ch.input);
  } finally {
    process.stdout.write = orig;
  }
  return { code, events, provider, types: events.map((e) => e.type) };
}

const lastToolResult = (provider: ReturnType<typeof replayProvider>, request = 1) => JSON.stringify(provider.requests[request]!.messages.at(-1));

test("allow: the call runs; a bad line in between is reported and does not stop the run", async () => {
  const { code, events, types } = await drive([bash("c1", "touch allowed"), text("done")], (e, ch) => {
    if (e.type !== "approval_request") return;
    ch.send("this is not json");
    ch.send({ type: "approval", id: e.id, decision: "allow" });
  });
  expect(code).toBe(0);
  const asked = events.filter((e) => e.type === "approval_request");
  expect(asked).toEqual([{ type: "approval_request", id: asked[0].id, callId: "c1", tool: "bash", args: { command: "touch allowed" }, reason: expect.any(String), cwd: proj }]);
  expect(events.filter((e) => e.type === "control_error")).toEqual([{ type: "control_error", error: "not valid JSON" }]);
  expect(existsSync(join(proj, "allowed"))).toBe(true);
  expect(types.indexOf("approval_request")).toBeLessThan(types.indexOf("tool_start"));
  expect(events.at(-1)).toEqual({ type: "agent_end", cause: "done" });
});

test("deny with feedback: the call does not run and the model is told why", async () => {
  const { code, provider, events } = await drive([bash("c1", "touch denied"), text("ok, not doing that")], (e, ch) => {
    if (e.type === "approval_request") ch.send({ type: "approval", id: e.id, decision: "deny", feedback: "not now" });
  });
  expect(code).toBe(0);
  expect(existsSync(join(proj, "denied"))).toBe(false);
  expect(lastToolResult(provider)).toContain("The user denied this tool call. User feedback: not now");
  expect(events.some((e) => e.type === "tool_start")).toBe(false);
});

test('"always" allows the same call again without asking', async () => {
  const { code, events } = await drive([bash("c1", "touch allowed"), bash("c2", "touch allowed"), text("done")], (e, ch) => {
    if (e.type === "approval_request") ch.send({ type: "approval", id: e.id, decision: "always" });
  });
  expect(code).toBe(0);
  expect(events.filter((e) => e.type === "approval_request")).toHaveLength(1);
  expect(events.filter((e) => e.type === "tool_start")).toHaveLength(2);
});

test("abort while a request is open: approval_cancelled, the call does not run, the run ends as aborted", async () => {
  let id = "";
  const { code, events, types } = await drive([bash("c1", "touch cancelled"), text("unreachable")], (e, ch) => {
    if (e.type !== "approval_request") return;
    id = e.id;
    ch.send({ type: "abort" });
  });
  expect(code).toBe(1);
  expect(events.filter((e) => e.type === "approval_cancelled")).toEqual([{ type: "approval_cancelled", id }]);
  expect(types.indexOf("approval_cancelled")).toBeLessThan(types.indexOf("agent_end"));
  expect(events.at(-1)).toEqual({ type: "agent_end", cause: "aborted" });
  expect(existsSync(join(proj, "cancelled"))).toBe(false);
});

test("stdin closing mid-run denies the open request and the next one; the run still finishes", async () => {
  const { code, events, provider } = await drive([bash("c1", "touch never"), bash("c2", "touch never"), text("gave up")], (e, ch) => {
    if (e.type === "approval_request") ch.end();
  });
  expect(code).toBe(0); // not aborted: the model got the denials and answered
  expect(events.filter((e) => e.type === "approval_request")).toHaveLength(1);
  expect(existsSync(join(proj, "never"))).toBe(false);
  expect(lastToolResult(provider, 1)).toContain("User feedback: the controlling process closed stdin");
  expect(lastToolResult(provider, 2)).toContain("User feedback: the controlling process closed stdin");
  expect(events.at(-1)).toEqual({ type: "agent_end", cause: "done" });
});
