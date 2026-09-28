import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { ToolCall } from "@sasacode/ai";
import { text, type ToolContext, type ToolDefinition, type ToolResult } from "@sasacode/plugin-api";
import { Agent, EventBus, HookRunner, PermissionPolicy, PluginHost, SessionFile, type AgentEvent } from "../src/index.ts";
import { executeTools, type ToolRunContext } from "../src/tool-exec.ts";

const cwd = mkdtempSync(join(tmpdir(), "plugin-api-v19-"));
afterAll(() => rmSync(cwd, { recursive: true, force: true }));
const result = (message: string): ToolResult => ({ content: [text(message)] });
const body = (value: ToolResult) => JSON.stringify(value.content);
const makeTool = (name: string, execute: ToolDefinition["execute"]): ToolDefinition => ({
  name, description: name, kind: "exec", parameters: { type: "object", properties: {} }, execute,
});
function setup(tools: ToolDefinition[], policy = new PermissionPolicy("auto"), agent?: string) {
  const events: AgentEvent[] = [];
  const ctx: ToolRunContext = { cwd, hooks: new HookRunner({ agent }), events: new EventBus(), permissions: policy,
    judge: async () => ({ safe: false, reason: "test" }), findTool: (name) => tools.find((t) => t.name === name), allTools: () => tools };
  ctx.events.on((event) => events.push(event));
  return { ctx, events, async run(name = "wrapper", input: Record<string, unknown> = {}, signal = new AbortController().signal) {
    let output!: ToolResult;
    const messages: unknown[] = [];
    const call: ToolCall = { type: "tool_call", id: "parent", name, input };
    await executeTools(ctx, [call], { signal, truncated: false, repairs: new Map(), onResult: (m) => messages.push(m), onToolResult: (r) => { output = r; } });
    return { output, messages };
  } };
}

test("nested calls obey target deny rules despite a parent allow, and emit ran=false", async () => {
  let ran = false;
  const h = setup([makeTool("wrapper", (_, ctx) => ctx.callTool!("target", {})), makeTool("target", async () => { ran = true; return result("bad"); })],
    new PermissionPolicy("auto", { allow: ["wrapper"], deny: ["target"] }));
  const results: boolean[] = [];
  h.ctx.hooks.on("tool_result", ({ call, ran }) => { if (call.name === "target") results.push(ran); });
  const { output, messages } = await h.run();
  expect(ran).toBe(false); expect(output.isError).toBe(true); expect(body(output)).toContain("Permission denied");
  expect(results).toEqual([false]); expect(messages).toHaveLength(1);
  expect(h.events.find((e) => e.type === "tool_end" && e.call.name === "target")).toMatchObject({ parentCallId: "parent" });
});

test("rewrites are validated, permission hooks see the actual input, details survive, and subagent identity is preserved", async () => {
  const target = makeTool("target", async (args) => ({ content: [text(String(args.n))], details: { ok: true } }));
  target.parameters = { type: "object", properties: { n: { type: "integer", minimum: 1 } }, required: ["n"], additionalProperties: false };
  const h = setup([makeTool("wrapper", (_, ctx) => ctx.callTool!("target", { n: 1 })), target], undefined, "sub-example");
  const contexts: (string | undefined)[] = [];
  h.ctx.hooks.on("tool_call", ({ call }, ctx) => { contexts.push(ctx.agent); if (call.name === "target") return { args: { n: 2 } }; });
  h.ctx.hooks.on("permission", ({ call, args }) => { if (call.name === "target") expect(args.n).toBe(2); });
  const { output } = await h.run();
  expect(body(output)).toContain("2"); expect(output.details).toEqual({ ok: true }); expect(contexts).toEqual(["sub-example", "sub-example"]);
  h.ctx.hooks.on("tool_call", ({ call }) => call.name === "target" ? { args: { n: "bad" } } : undefined);
  expect(body((await h.run()).output)).toContain("Invalid arguments after tool_call");
});

test("target asks independently; headless cannot approve and invalid/unknown tools never run", async () => {
  const h = setup([makeTool("wrapper", (_, ctx) => ctx.callTool!("target", {})), makeTool("target", async () => result("ran"))],
    new PermissionPolicy("auto", { ask: ["target"] }));
  expect(body((await h.run()).output)).toContain("no user is available");
  const asked: string[] = [];
  h.ctx.approve = async (r) => { asked.push(r.tool.name); return { decision: "allow" }; };
  expect(body((await h.run()).output)).toContain("ran"); expect(asked).toEqual(["target"]);
  expect(body((await h.run("missing")).output)).toContain("Unknown tool");
});

test("cycles and excessive nesting are stopped without invoking the rejected tool", async () => {
  const h = setup([makeTool("wrapper", (_, ctx) => ctx.callTool!("other", {})), makeTool("other", (_, ctx) => ctx.callTool!("wrapper", {}))]);
  expect(body((await h.run()).output)).toContain("cycle");
  const chain = Array.from({ length: 10 }, (_, i) => makeTool(`t${i}`, (_, ctx) => ctx.callTool!(`t${i + 1}`, {})));
  const deep = setup(chain);
  expect(body((await deep.run("t0")).output)).toContain("depth exceeds 8");
  expect(deep.events.filter((e) => e.type === "tool_start")).toHaveLength(8);
});

test("nested calls serialize, copy inputs at invocation, join unawaited calls, and reject expired contexts", async () => {
  const seen: number[] = [];
  let active = 0, peak = 0, captured!: ToolContext;
  const h = setup([makeTool("wrapper", async (_, ctx) => {
    captured = ctx;
    const args = { n: 1 };
    void ctx.callTool!("target", args); args.n = 99;
    void ctx.callTool!("target", { n: 2 });
    return result("parent");
  }), makeTool("target", async ({ n }) => { peak = Math.max(peak, ++active); await Bun.sleep(5); seen.push(n); active--; return result("child"); })]);
  await h.run(); expect(seen).toEqual([1, 2]); expect(peak).toBe(1);
  expect(body(await captured.callTool!("target", { n: 3 }))).toContain("no longer active"); expect(seen).toEqual([1, 2]);
});

test("abort propagates to a running child and prevents queued children from executing", async () => {
  const controller = new AbortController(); let runs = 0;
  const h = setup([makeTool("wrapper", async (_, ctx) => {
    const one = ctx.callTool!("target", {}); const two = ctx.callTool!("target", {});
    return (await Promise.all([one, two]))[1];
  }), makeTool("target", async (_, ctx) => { runs++; controller.abort(); expect(ctx.signal.aborted).toBe(true); return result("cancelled"); })]);
  expect(body((await h.run("wrapper", {}, controller.signal)).output)).toContain("Interrupted"); expect(runs).toBe(1);
});

test("nested calls are audited without unmatched tool messages in model history", async () => {
  const h = setup([makeTool("wrapper", (_, ctx) => ctx.callTool!("target", {})), makeTool("target", async () => result("child"))]);
  const session = SessionFile.create(join(cwd, "sessions"), cwd); h.ctx.session = session;
  const { messages } = await h.run();
  expect(messages).toHaveLength(1);
  const entries = SessionFile.open(session.path).entries;
  expect(entries.filter((e) => e.type === "custom").map((e) => e.kind)).toEqual(["nested_tool_call", "nested_tool_execute", "nested_tool_result"]);
});

function host(settings: Record<string, unknown> = {}) {
  const agent = new Agent({ cwd, systemPrompt: "test", model: { id: "m", provider: "test", api: "replay", contextWindow: 10000, maxOutput: 1000 } });
  return new PluginHost({ agent, cwd, settings: () => settings });
}

test("defineSettings merges nested defaults, replaces arrays/null, and does not mutate either source", () => {
  const raw = { options: { timeout: 30 }, values: [2], nullable: null };
  const defaults = { options: { timeout: 10, enabled: true }, values: [1, 3], nullable: "default" };
  const api = host(raw).api("demo");
  const value = api.defineSettings<{ options: { timeout: number; enabled: boolean }; values: number[]; nullable: string | null }>({ schema: { type: "object", properties: { options: { type: "object" }, values: { type: "array", items: { type: "integer" } }, nullable: { type: ["null", "string"] } }, additionalProperties: false }, defaults });
  expect(value).toEqual({ options: { timeout: 30, enabled: true }, values: [2], nullable: null });
  value.options!.timeout = 77;
  expect(raw.options.timeout).toBe(30); expect(defaults.options.timeout).toBe(10); expect(api.settings).toBe(raw);
});

test("settings reject unknown keys, wrong types/ranges and unsupported schema keywords; no coercion or secret echo", async () => {
  const h = host({ timeout: "private-token" }); const notices: string[] = [];
  h.setUI({ interactive: false, notify: (s) => notices.push(s), confirm: async () => false, select: async () => undefined });
  expect(await h.load("demo", (api) => { api.defineSettings({ schema: { type: "object", properties: { timeout: { type: "integer", minimum: 1 } } } }); })).toBe(false);
  expect(notices.join()).toContain("plugins.settings.demo.timeout"); expect(notices.join()).not.toContain("private-token");
  expect(() => host({ extra: true }).api("demo").defineSettings({ schema: { type: "object", additionalProperties: false } })).toThrow("extra");
  expect(() => host({ timeout: 0 }).api("demo").defineSettings({ schema: { type: "object", properties: { timeout: { minimum: 1 } } } })).toThrow("minimum");
  expect(() => host().api("demo").defineSettings({ schema: { type: "object", $ref: "https://example.invalid" } })).toThrow("unsupported");
  expect(() => host().api("demo").defineSettings({ schema: { type: "object", properties: { x: { pattern: "[" } } } })).toThrow("invalid schema");
});

test("settings support schema-valued maps and combinations without prototype pollution", () => {
  const schema = { type: "object", additionalProperties: { anyOf: [{ const: false }, { type: "string", minLength: 2, maxLength: 8, pattern: "^[a-z]+$" }] } };
  expect(host({ a: false, b: "good" }).api("x").defineSettings({ schema })).toEqual({ a: false, b: "good" });
  expect(() => host({ a: "X" }).api("x").defineSettings({ schema })).toThrow("anyOf");
  const raw = JSON.parse('{"__proto__":{"polluted":true},"constructor":"own"}');
  const value = host(raw).api("x").defineSettings({ schema: { type: "object" }, defaults: {} });
  expect(Object.hasOwn(value, "__proto__")).toBe(true);
  expect(({} as any).polluted).toBeUndefined();
  expect(() => host(raw).api("x").defineSettings({ schema: { type: "object", additionalProperties: false } })).toThrow("not an allowed property");
});

test("showText sanitizes controls, awaits the viewer, and falls back without changing messages", async () => {
  const h = host(); const seen: unknown[] = []; let close!: () => void;
  h.setUI({ interactive: true, notify: () => {}, confirm: async () => false, select: async () => undefined,
    showText: async (options) => { seen.push(options); await new Promise<void>((r) => { close = r; }); } });
  let finished = false;
  const pending = h.api("demo").ui.showText({ title: "hello\x1b[2J", text: "safe\x1b]52;c;bad\x07", format: "markdown" }).then(() => { finished = true; });
  expect(finished).toBe(false); expect(JSON.stringify(seen)).not.toContain("\\u001b");
  close(); await pending; expect(finished).toBe(true);
  const messages = h.api("demo").session.messages();
  const fallback: string[] = [];
  h.setUI({ interactive: false, notify: (s) => fallback.push(s), confirm: async () => false, select: async () => undefined });
  await h.api("demo").ui.showText({ title: "title", text: "body" });
  expect(fallback).toEqual(["title\nbody"]); expect(h.api("demo").session.messages()).toEqual(messages);
});


test("abort releases a pending child approval without executing or adding an always rule", async () => {
  const controller = new AbortController(); let ran = false;
  const h = setup([makeTool("wrapper", (_, ctx) => ctx.callTool!("target", {})), makeTool("target", async () => { ran = true; return result("bad"); })],
    new PermissionPolicy("auto", { ask: ["target"] }));
  let started!: () => void;
  const asking = new Promise<void>((resolve) => { started = resolve; });
  h.ctx.approve = async (req) => { expect(req.signal).toBe(controller.signal); started(); return new Promise(() => {}); };
  const run = h.run("wrapper", {}, controller.signal);
  await asking; controller.abort();
  expect(body((await run).output)).toContain("Interrupted"); expect(ran).toBe(false);
});

test("nested lookup cannot reach a tool omitted from this agent's registry", async () => {
  const h = setup([makeTool("wrapper", (_, ctx) => ctx.callTool!("excluded", {}))]);
  expect(body((await h.run()).output)).toContain('Unknown tool');
});
