// --control stdio in real runs, for what the desktop GUI needs beyond approvals: edited arguments,
// stopping one call, rules that outlast a run (--allow), the plugins' questions, notices, status
// and commands, slash commands from -p (--slash), and the agent mode's judge.
import { afterAll, beforeAll, beforeEach, expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { type AssistantMessage, emptyUsage, registerApi } from "@sasacode/ai";
import { setLang } from "@sasacode/host";
import { replayProvider } from "@sasacode/testing";
import type { ControlInput } from "../src/control.ts";
import { type HeadlessOptions, runHeadless } from "../src/headless.ts";
import { setup } from "../src/setup.ts";

beforeAll(() => setLang("en"));

const root = mkdtempSync(join(tmpdir(), "sasacode-control-run-"));
const home = join(root, "home");
const proj = join(root, "proj");
afterAll(() => rmSync(root, { recursive: true, force: true }));

// A global plugin with a tool that asks the user, and a command that talks.
const PLUGIN = `export default (api) => {
  api.registerTool({
    name: "ask_user", description: "ask the user", kind: "read",
    parameters: { type: "object", properties: { q: { type: "string" } }, required: ["q"] },
    async execute(args) {
      const v = await api.ui.select(args.q, [{ value: "red", label: "Red" }, { value: "blue", label: "Blue" }]);
      return { content: [{ type: "text", text: "picked " + v }] };
    },
  });
  api.registerCommand({
    name: "hello", description: "say hello", argumentHint: "[name]",
    async run({ args }) {
      api.ui.setStatus("greeting", "hi " + args);
      api.ui.notify("hello " + args);
      if (args === "model") api.session.inject("say hi", "now");
      if (args === "boom") throw new Error("kaboom");
    },
  });
};
`;

function config(over: object = {}) {
  writeFileSync(join(home, "config.json"), JSON.stringify({ providers: { test: { api: "replay" } }, model: "test/m", permissions: { mode: "ask" }, ...over }));
}

beforeEach(() => {
  process.env.SASACODE_HOME = home;
  mkdirSync(join(home, "plugins"), { recursive: true });
  mkdirSync(proj, { recursive: true });
  writeFileSync(join(home, "plugins", "asker.ts"), PLUGIN);
  config();
  for (const f of ["original", "edited", "allowed", "judged"]) rmSync(join(proj, f), { force: true });
});

function channel() {
  const queue: string[] = [];
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
    send(message: unknown) {
      queue.push(`${JSON.stringify(message)}\n`);
      wake?.();
    },
  };
}

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
const call = (id: string, name: string, input: Record<string, unknown>) => reply([{ type: "tool_call", id, name, input }]);
const bash = (id: string, command: string) => call(id, "bash", { command });
const text = (t: string) => reply([{ type: "text", text: t }]);

/** runHeadless with the channel on (jsonl); `react` sees each stdout line as it is written. */
async function drive(
  script: AssistantMessage[],
  react: (e: any, ch: ReturnType<typeof channel>) => void,
  o: { prompt?: string; opts?: HeadlessOptions; allow?: string[] } = {},
) {
  const provider = replayProvider(script);
  registerApi("replay", provider);
  const h = await setup({ cwd: proj, noSession: true });
  if (o.allow) h.agent.permissions.addRules({ allow: o.allow }); // what --allow does
  const ch = channel();
  const events: any[] = [];
  const orig = process.stdout.write.bind(process.stdout);
  process.stdout.write = ((s: string, cb?: () => void) => {
    const e = JSON.parse(s);
    events.push(e);
    react(e, ch);
    cb?.();
    return true;
  }) as typeof process.stdout.write;
  let code: number;
  try {
    code = await runHeadless(h, o.prompt ?? "go", "jsonl", ch.input, o.opts);
  } finally {
    process.stdout.write = orig;
  }
  return { code, events, provider, types: events.map((e) => e.type) };
}

const toolResult = (provider: ReturnType<typeof replayProvider>, request = 1) => JSON.stringify(provider.requests[request]!.messages.at(-1));

test("edited args: the call runs with them, the model is told, and the approval_request named the rule always adds", async () => {
  const { code, events, provider } = await drive([bash("c1", "touch original"), text("done")], (e, ch) => {
    if (e.type === "approval_request") ch.send({ type: "approval", id: e.id, decision: "allow", args: { command: "touch edited" } });
  });
  expect(code).toBe(0);
  expect(events.find((e) => e.type === "approval_request").rules).toEqual(["bash(touch original)"]);
  expect(existsSync(join(proj, "edited"))).toBe(true);
  expect(existsSync(join(proj, "original"))).toBe(false);
  expect(toolResult(provider)).toContain('[the user edited the arguments; the call ran with {\\"command\\":\\"touch edited\\"}]');
});

test("args sent back unchanged run the call as asked: the loop guard does not take it for a repeat", async () => {
  const { code, provider } = await drive([bash("c1", "touch original"), text("done")], (e, ch) => {
    if (e.type === "approval_request") ch.send({ type: "approval", id: e.id, decision: "allow", args: e.args });
  });
  expect(code).toBe(0);
  expect(existsSync(join(proj, "original"))).toBe(true);
  expect(toolResult(provider)).not.toContain("the user edited the arguments");
});

test("edited args that do not fit the tool, or that a deny rule matches, are a denial with the reason", async () => {
  config({ permissions: { mode: "ask", deny: ["bash(rm *)"] } });
  const answers = [{ command: 5 }, { command: "rm -rf original" }];
  const { code, events, provider } = await drive([bash("c1", "touch original"), bash("c2", "touch original"), text("ok")], (e, ch) => {
    if (e.type === "approval_request") ch.send({ type: "approval", id: e.id, decision: "allow", args: answers.shift() });
  });
  expect(code).toBe(0);
  expect(events.some((e) => e.type === "tool_start")).toBe(false);
  expect(toolResult(provider, 1)).toContain("The user's edited arguments were refused (");
  expect(toolResult(provider, 1)).toContain("command");
  expect(toolResult(provider, 2)).toContain("rule deny: bash(rm *)");
});

test('"always" with edited args allows the edited call from then on', async () => {
  const { events } = await drive([bash("c1", "touch original"), bash("c2", "touch edited"), text("done")], (e, ch) => {
    if (e.type === "approval_request") ch.send({ type: "approval", id: e.id, decision: "always", args: { command: "touch edited" } });
  });
  expect(events.filter((e) => e.type === "approval_request")).toHaveLength(1);
  expect(events.filter((e) => e.type === "tool_start")).toHaveLength(2);
  expect(existsSync(join(proj, "original"))).toBe(false);
});

test("--allow with an approval_request's rules: the next run allows the same call without asking", async () => {
  let rules: string[] = [];
  await drive([bash("c1", "touch allowed"), text("done")], (e, ch) => {
    if (e.type !== "approval_request") return;
    rules = e.rules;
    ch.send({ type: "approval", id: e.id, decision: "always" });
  });
  rmSync(join(proj, "allowed"));
  const { code, types } = await drive([bash("c1", "touch allowed"), text("done")], () => {}, { allow: rules });
  expect(code).toBe(0);
  expect(types).not.toContain("approval_request");
  expect(existsSync(join(proj, "allowed"))).toBe(true);
});

test("abort with a callId stops that call alone: its result is an error, and the run goes on", async () => {
  config({ permissions: { mode: "auto" } });
  const started = Date.now();
  const { code, events, provider } = await drive([bash("c1", "sleep 20"), text("carried on")], (e, ch) => {
    if (e.type === "tool_start") ch.send({ type: "abort", callId: e.call.id });
  });
  expect(Date.now() - started).toBeLessThan(10_000);
  expect(code).toBe(0);
  expect(events.find((e) => e.type === "tool_end").result.isError).toBe(true);
  expect(toolResult(provider)).toContain("[The user stopped this tool call; the rest of the run goes on.]");
  expect(events.at(-1)).toEqual({ type: "agent_end", cause: "done" });
});

test("a plugin's question reaches the parent and its answer the tool; the plugins' commands come first", async () => {
  config({ permissions: { mode: "auto" } });
  const { code, events, types, provider } = await drive([call("c1", "ask_user", { q: "Color?" }), text("done")], (e, ch) => {
    if (e.type === "ui_request") ch.send({ type: "ui_response", id: e.id, value: "blue" });
  });
  expect(code).toBe(0);
  expect(events.find((e) => e.type === "ui_request")).toEqual({
    type: "ui_request",
    id: "ui-1",
    kind: "select",
    title: "Color?",
    options: [{ value: "red", label: "Red" }, { value: "blue", label: "Blue" }],
  });
  expect(toolResult(provider)).toContain("picked blue");
  const listed = events.find((e) => e.type === "commands");
  expect(listed.commands).toContainEqual({ name: "hello", description: "say hello", argumentHint: "[name]", owner: "asker" });
  expect(types.indexOf("commands")).toBeLessThan(types.indexOf("agent_start"));
});

test("abort while a plugin's question is open: ui_cancelled, the question is dismissed, and the run ends as aborted", async () => {
  config({ permissions: { mode: "auto" } });
  const { code, events, types } = await drive([call("c1", "ask_user", { q: "Color?" }), text("unreachable")], (e, ch) => {
    if (e.type === "ui_request") ch.send({ type: "abort" });
  });
  expect(code).toBe(1);
  expect(events.filter((e) => e.type === "ui_cancelled")).toEqual([{ type: "ui_cancelled", id: "ui-1" }]);
  expect(types.indexOf("ui_cancelled")).toBeLessThan(types.indexOf("agent_end"));
  expect(events.at(-1)).toEqual({ type: "agent_end", cause: "aborted" });
});

test("--slash runs a plugin command: what it says and shows are lines, and no model is asked", async () => {
  const { code, events, provider, types } = await drive([], () => {}, { prompt: "/hello world", opts: { slash: true } });
  expect(code).toBe(0);
  expect(events).toContainEqual({ type: "plugin_status", key: "asker:greeting", text: "hi world" });
  expect(events).toContainEqual({ type: "plugin_notify", level: "info", text: "hello world" });
  expect(types).not.toContain("agent_start");
  expect(provider.requests).toHaveLength(0);
});

test("--slash: a run the command starts runs to its end; an unknown command or one that throws is an error line and exit 1", async () => {
  const started = await drive([text("hi there")], () => {}, { prompt: "/hello model", opts: { slash: true } });
  expect(started.code).toBe(0);
  expect(started.events.at(-1)).toEqual({ type: "agent_end", cause: "done" });
  expect(JSON.stringify(started.provider.requests[0]!.messages)).toContain("say hi");
  const unknown = await drive([], () => {}, { prompt: "/nope x", opts: { slash: true } });
  expect(unknown.code).toBe(1);
  expect(unknown.events).toContainEqual({ type: "error", error: "unknown command: /nope" });
  const thrown = await drive([], () => {}, { prompt: "/hello boom", opts: { slash: true } });
  expect(thrown.code).toBe(1);
  expect(thrown.events).toContainEqual({ type: "error", error: "/hello failed: kaboom" });
  // Without --slash the same prompt goes to the model as it always did.
  const plain = await drive([text("a reply")], () => {}, { prompt: "/hello world" });
  expect(plain.code).toBe(0);
  expect(JSON.stringify(plain.provider.requests[0]!.messages)).toContain("/hello world");
});

test("--slash in text mode: the command's notices go to stderr", async () => {
  registerApi("replay", replayProvider([]));
  const h = await setup({ cwd: proj, noSession: true });
  const err: string[] = [];
  const orig = process.stderr.write.bind(process.stderr);
  process.stderr.write = ((s: string, cb?: () => void) => {
    err.push(s);
    cb?.();
    return true;
  }) as typeof process.stderr.write;
  let code: number;
  try {
    code = await runHeadless(h, "/hello world", "text", undefined, { slash: true });
  } finally {
    process.stderr.write = orig;
  }
  expect(code).toBe(0);
  expect(err.join("")).toContain("hello world");
});

test("agent mode: the judge's verdict is a judged line", async () => {
  config({ permissions: { mode: "agent" } });
  const verdict = text('{"safe": true, "reason": "creates one file here"}');
  const { code, events } = await drive([bash("c1", "touch judged"), verdict, text("done")], () => {});
  expect(code).toBe(0);
  expect(events).toContainEqual({ type: "judged", callId: "c1", tool: "bash", verdict: "allow", reason: "agent judged safe: creates one file here" });
  expect(events.some((e) => e.type === "approval_request")).toBe(false);
  expect(existsSync(join(proj, "judged"))).toBe(true);
});

// ── the flags, as the command line takes them ──

const MAIN = resolve(import.meta.dir, "../src/main.ts");
const NO_BUNFIG = resolve(import.meta.dir, "../../../scripts/no-bunfig.toml");

async function cli(...args: string[]) {
  const p = Bun.spawn(["bun", "--no-env-file", `--config=${NO_BUNFIG}`, MAIN, ...args], {
    cwd: proj,
    env: { ...process.env, SASACODE_HOME: home, SASACODE_LANG: "en" },
    stdout: "pipe",
    stderr: "pipe",
  });
  const [stdout, stderr, code] = await Promise.all([new Response(p.stdout).text(), new Response(p.stderr).text(), p.exited]);
  return { stdout, stderr, code };
}

test("--allow must be a permission rule, and --slash needs -p", async () => {
  const bad = await cli("-p", "x", "--allow", "bash(ls)", "--allow", "(oops)");
  expect(bad.code).toBe(1);
  expect(bad.stderr).toContain('--allow "(oops)" is not a permission rule');
  const slash = await cli("--slash");
  expect(slash.code).toBe(1);
  expect(slash.stderr).toContain("--slash needs -p");
});
