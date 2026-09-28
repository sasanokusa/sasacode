import { expect, test } from "bun:test";
import { tmpdir } from "node:os";
import { Agent, PermissionPolicy, PluginHost } from "@sasacode/agent";
import { type AssistantContent, type AssistantMessage, emptyUsage, type Provider, registerApi, replayProvider } from "@sasacode/ai";
import builtinTools from "@sasacode/tools";
import { bundledPlugins } from "../src/index.ts";
import { reportOf } from "../src/subagent.ts";

const reply = (content: AssistantContent[]): AssistantMessage => ({
  role: "assistant",
  content,
  api: "replay",
  provider: "t",
  model: "m",
  usage: { ...emptyUsage(), input: 10 },
  stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop",
  timestamp: 0,
});
const say = (text: string) => ({ type: "text" as const, text });
const call = (id: string, name: string, input: Record<string, unknown>) => ({ type: "tool_call" as const, id, name, input });
const lastText = (p: { requests: { messages: unknown[] }[] }) => JSON.stringify(p.requests.at(-1)!.messages.at(-1));

/** The caller runs on api "main-<n>", subagents (model "t/sub") on "sub-<n>", so each has its own script. */
let n = 0;
async function setup(main: AssistantMessage[], sub: Provider) {
  const tag = ++n;
  const mainProvider = replayProvider(main);
  registerApi(`main-${tag}`, mainProvider);
  registerApi(`sub-${tag}`, sub);
  const model = (api: string) => ({ id: "m", provider: "t", api, contextWindow: 100_000, maxOutput: 1000 });
  const agent = new Agent({
    model: model(`main-${tag}`),
    cwd: tmpdir(),
    systemPrompt: "base",
    permissions: new PermissionPolicy("edits"),
    resolveModel: () => model(`sub-${tag}`),
  });
  const notices: string[] = [];
  const host = new PluginHost({ agent, cwd: agent.cwd, settings: () => ({}) });
  host.setUI({ interactive: false, notify: (m) => notices.push(m), confirm: async () => false, select: async () => undefined });
  await host.load("builtin-tools", builtinTools);
  await host.load("subagent", bundledPlugins.subagent!);
  return { agent, mainProvider, notices };
}

async function until(cond: () => boolean) {
  for (let i = 0; i < 200 && !cond(); i++) await new Promise((r) => setTimeout(r, 5));
  expect(cond()).toBe(true);
}

test("a subagent that fails tells the caller why, and what it had done", async () => {
  const sub = replayProvider([reply([say("checking"), call("s1", "read", { path: "nope.txt" })])]); // then the script runs out: an error
  const { agent, mainProvider } = await setup([reply([call("1", "task", { description: "look", prompt: "look", model: "t/sub" })]), reply([say("ok")])], sub);
  await agent.prompt("go");
  const result = lastText(mainProvider);
  expect(result).toContain("subagent did not finish: error: replay: no more recorded responses");
  expect(result).toContain("checking");
  expect(result).toContain("read(");
});

test("reportOf: a finished run is just its report", () => {
  expect(reportOf({ text: "found it", cause: "done" }, ["read(a)"])).toBe("found it");
  expect(reportOf({ text: "", cause: "max_turns" }, [])).toContain("did not finish: max_turns");
});

test("background: the caller keeps going and the report arrives as a message", async () => {
  const sub = replayProvider([reply([say("found 3 files")])]);
  const { agent, mainProvider, notices } = await setup(
    [reply([call("1", "task", { description: "look", prompt: "look", model: "t/sub", background: true })]), reply([say("meanwhile")]), reply([say("thanks")])],
    sub,
  );
  await agent.prompt("go");
  await until(() => mainProvider.requests.length === 3);
  await agent.waitForIdle();
  expect(JSON.stringify(mainProvider.requests[1]!.messages)).toContain("Started t1 in the background");
  expect(lastText(mainProvider)).toContain('[task t1 \\"look\\" ended: done]');
  expect(lastText(mainProvider)).toContain("found 3 files");
  expect(notices.some((m) => m.includes("t1"))).toBe(true);
});

test("task_send reaches a running background subagent at its next turn", async () => {
  let release!: () => void;
  const gate = new Promise<void>((r) => (release = r));
  const inner = replayProvider([reply([call("s1", "task_status", {})]), reply([say("changed course")])]);
  const sub: Provider & { requests: any[] } = {
    api: "gated",
    requests: inner.requests,
    async *stream(req) {
      await gate;
      yield* inner.stream(req);
    },
  };
  const { agent, mainProvider } = await setup(
    [
      reply([call("1", "task", { description: "look", prompt: "look", model: "t/sub", background: true })]),
      reply([call("2", "task_send", { id: "t1", message: "look in src instead" })]),
      reply([say("waiting")]),
      reply([say("done")]),
    ],
    sub,
  );
  await agent.prompt("go");
  release();
  await until(() => mainProvider.requests.length === 4);
  expect(JSON.stringify(inner.requests[1]!.messages)).toContain("look in src instead");
  expect(lastText(mainProvider)).toContain("changed course");
});

test("Esc on the caller stops its background runs, and the report waits for the next message", async () => {
  let release!: () => void;
  const gate = new Promise<void>((r) => (release = r));
  const inner = replayProvider([reply([say("never reached")])]);
  const sub: Provider = {
    api: "gated",
    async *stream(req) {
      await gate;
      yield* inner.stream(req);
    },
  };
  const { agent, mainProvider, notices } = await setup(
    [reply([call("1", "task", { description: "look", prompt: "look", model: "t/sub", background: true })]), reply([say("carry on")])],
    sub,
  );
  // The caller's second request is held until Esc.
  const replay = (await import("@sasacode/ai")).getProvider(agent.model.api);
  const stream = replay.stream.bind(replay);
  let calls = 0;
  replay.stream = async function* (req) {
    if (++calls === 2) await new Promise((r) => req.signal?.addEventListener("abort", r, { once: true }));
    yield* stream(req);
  };
  const run = agent.prompt("go");
  await until(() => calls === 2);
  agent.abort();
  expect(await run).toBe("aborted");
  release();
  await until(() => notices.some((m) => m.includes("t1") && m.includes("stopped")));
  await new Promise((r) => setTimeout(r, 30));
  expect(agent.isRunning).toBe(false);
  expect(mainProvider.requests).toHaveLength(2);
});
