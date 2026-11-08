import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Agent, type AgentOptions, PermissionPolicy, PluginHost, SessionFile } from "@sasacode/agent";
import { type AssistantContent, type AssistantMessage, emptyUsage, registerApi, type ToolResultMessage } from "@sasacode/ai";
import type { PermissionMode } from "@sasacode/plugin-api";
import { replayProvider } from "@sasacode/testing";
import builtinTools from "@sasacode/tools";
import { bundledPlugins, extractTextCalls } from "../src/index.ts";

const root = mkdtempSync(join(tmpdir(), "sasacode-text-calls-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));
const file = join(root, "a.ts");
writeFileSync(file, "export const answer = 42;\n");
const at = JSON.stringify(file);

const reply = (content: AssistantContent[], extra: Partial<AssistantMessage> = {}): AssistantMessage => ({
  role: "assistant",
  content,
  api: "replay",
  provider: "t",
  model: "m",
  usage: emptyUsage(),
  stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop",
  timestamp: Date.now(),
  ...extra,
});
const say = (text: string, extra: Partial<AssistantMessage> = {}) => reply([{ type: "text", text }], extra);

interface RunOptions {
  /** Where the model is served; a server on this machine unless given. */
  baseUrl?: string;
  mode?: PermissionMode;
  settings?: Record<string, unknown>;
  approve?: AgentOptions["approve"];
  /** Replies after the first, before "done" (a judge's answer). */
  then?: AssistantMessage[];
}

async function run(first: AssistantMessage, plugins = ["text-tool-calls"], opts: RunOptions = {}) {
  const provider = replayProvider([first, ...(opts.then ?? []), say("done")]);
  registerApi("replay", provider);
  const session = SessionFile.create(join(root, "sessions"), root);
  const baseUrl = "baseUrl" in opts ? opts.baseUrl : "http://127.0.0.1:8080/v1";
  const agent = new Agent({
    model: { id: "m", provider: "t", api: "replay", baseUrl, contextWindow: 1e5, maxOutput: 1e3 },
    cwd: root,
    systemPrompt: "",
    permissions: new PermissionPolicy(opts.mode ?? "auto"),
    session,
    approve: opts.approve,
  });
  const host = new PluginHost({ agent, cwd: root, settings: (p) => (p === "text-tool-calls" ? (opts.settings ?? {}) : {}) });
  await host.load("builtin-tools", builtinTools);
  for (const p of plugins) await host.load(p, bundledPlugins[p]!);
  await agent.prompt("go");
  const stored = agent.messages[1] as AssistantMessage;
  const results = agent.messages.filter((m): m is ToolResultMessage => m.role === "tool");
  return { provider, stored, results, entries: SessionFile.open(session.path).entries };
}

test("extractTextCalls: the three formats, and only calls that end the reply", () => {
  const call = (name: string, args: string) => ({ name, args });
  expect(extractTextCalls('Let me look.\n<tool_call>\n{"name": "read", "arguments": {"path": "a.ts"}}\n</tool_call>')).toEqual({
    prose: "Let me look.",
    calls: [call("read", '{"path":"a.ts"}')],
    skipped: 0,
  });
  // Several in a row; Hermes puts the arguments first; a missing closing tag at the very end.
  expect(extractTextCalls('<tool_call>{"arguments": {"path": "a"}, "name": "read"}</tool_call>\n<tool_call>{"name": "bash", "arguments": {"command": "ls"}}')?.calls).toEqual([
    call("read", '{"path":"a"}'),
    call("bash", '{"command":"ls"}'),
  ]);
  expect(extractTextCalls('<function=read>{"path": "a.ts"}</function>')).toEqual({ prose: "", calls: [call("read", '{"path": "a.ts"}')], skipped: 0 });
  expect(extractTextCalls('Reading it.\n```json\n{"name": "read", "parameters": {"path": "a.ts"}}\n```\n')?.calls).toEqual([call("read", '{"path":"a.ts"}')]);
  // Broken JSON: the arguments are cut out as written, for tool-repair.
  expect(extractTextCalls("<tool_call>{\"name\": \"read\", \"arguments\": {path: 'a.ts',}}</tool_call>")?.calls).toEqual([call("read", "{path: 'a.ts',}")]);
  expect(extractTextCalls('<tool_call>{"arguments": {"path": "a",}, "name": "read"}</tool_call>')?.calls).toEqual([call("read", '{"path": "a",}')]);
  // A Python-style envelope (single quotes around the name too) is repaired as a whole.
  expect(extractTextCalls("<tool_call>{'name': 'read', 'arguments': {'path': 'a.ts'}}</tool_call>")?.calls).toEqual([call("read", '{"path":"a.ts"}')]);
  // A closing tag missing before the next call does not swallow that call.
  expect(extractTextCalls('<tool_call>{"name": "read", "arguments": {"path": "a"}}\n<tool_call>{"name": "bash", "arguments": {"command": "ls"}}</tool_call>')?.calls).toEqual([
    call("read", '{"path":"a"}'),
    call("bash", '{"command":"ls"}'),
  ]);
  // A tag inside the content of a call that is picked up belongs to that content.
  const nested = { path: "t.md", content: '<tool_call>{"name": "read", "arguments": {"path": "a"}}</tool_call>' };
  expect(extractTextCalls(`\`\`\`json\n${JSON.stringify({ name: "write", arguments: nested })}\n\`\`\``)?.calls).toEqual([call("write", JSON.stringify(nested))]);
  // … also an opening tag alone, in a call written in the same form.
  const mention = { path: "README.md", content: "Models write <tool_call> tags." };
  expect(extractTextCalls(`<tool_call>${JSON.stringify({ name: "write", arguments: mention })}</tool_call>`)?.calls).toEqual([call("write", JSON.stringify(mention))]);
  expect(extractTextCalls('<function=write>{"path": "a.md", "content": "use <function=name> syntax"}</function>')?.calls).toEqual([
    call("write", '{"path": "a.md", "content": "use <function=name> syntax"}'),
  ]);
  // A call followed by more prose stays text when a later one ends the reply; it is counted.
  expect(extractTextCalls('Read a.\n<tool_call>{"name": "read", "arguments": {"path": "a"}}</tool_call>\nAnd b.\n<tool_call>{"name": "read", "arguments": {"path": "b"}}</tool_call>')).toMatchObject({
    calls: [call("read", '{"path":"b"}')],
    skipped: 1,
  });
  // Examples in a code block or inline code are not counted, nor a call inside another one.
  const a = '{"name": "read", "arguments": {"path": "a"}}';
  const last = `<tool_call>${a}</tool_call>`;
  expect(extractTextCalls(`Example:\n\`\`\`json\n${a}\n\`\`\`\nNow:\n${last}`)?.skipped).toBe(0);
  expect(extractTextCalls(`See \`<tool_call>${a}</tool_call>\` then\n${last}`)?.skipped).toBe(0);
  const inner = { path: "t.md", content: `<function=read>{"path": "a"}</function>` };
  expect(extractTextCalls(`Write it.\n<tool_call>${JSON.stringify({ name: "write", arguments: inner })}</tool_call>\nThen:\n${last}`)?.skipped).toBe(1);
  // Not calls: an example followed by more text, one shown in a code block, a tool definition,
  // a JSON answer after a long explanation, and a call that cannot be read.
  expect(extractTextCalls('Write <tool_call>{"name": "read", "arguments": {"path": "a"}}</tool_call> and it runs.')).toBeUndefined();
  expect(extractTextCalls('The format:\n```\n<tool_call>{"name": "read", "arguments": {"path": "a"}}</tool_call>\n```')).toBeUndefined();
  // … also when the code block is never closed.
  expect(extractTextCalls('```\n<tool_call>{"name": "read", "arguments": {"path": "a"}}</tool_call>')).toBeUndefined();
  expect(extractTextCalls('```xml\n<function=read>{"path": "a"}</function>')).toBeUndefined();
  expect(extractTextCalls('```json\n{"name": "read", "description": "Read a file", "parameters": {"type": "object"}}\n```')).toBeUndefined();
  expect(extractTextCalls(`${"Here is how the protocol works. ".repeat(12)}\n\`\`\`json\n{"name": "read", "arguments": {"path": "a"}}\n\`\`\``)).toBeUndefined();
  expect(extractTextCalls("<tool_call>{not json at all}</tool_call>")).toBeUndefined();
  expect(extractTextCalls("Just an answer.")).toBeUndefined();
});

const formats = {
  "<tool_call>": `I'll read it.\n<tool_call>\n{"name": "read", "arguments": {"path": ${at}}}\n</tool_call>`,
  "```json": `I'll read it.\n\`\`\`json\n{"name": "read", "parameters": {"path": ${at}}}\n\`\`\``,
  "<function=>": `I'll read it.\n<function=read>{"path": ${at}}</function>`,
};

for (const [format, text] of Object.entries(formats))
  test(`text-tool-calls: a ${format} call in the reply runs, and the model is told to call natively`, async () => {
    const { provider, stored, results, entries } = await run(say(text));
    expect(provider.requests).toHaveLength(2);
    expect(stored.stopReason).toBe("tool_use");
    expect(stored.content).toEqual([
      { type: "text", text: "I'll read it." },
      { type: "tool_call", id: expect.stringMatching(/^textcall[0-9a-f]{16}$/), name: "read", input: { path: file } },
    ]);
    expect(results).toHaveLength(1);
    expect(results[0]!.isError).toBe(false);
    expect(results[0]!.content[0]).toEqual({ type: "text", text: expect.stringContaining("call tools with native tool calls") });
    expect(JSON.stringify(results[0]!.content)).toContain("export const answer = 42;");
    // The history the model sees next holds the native call; the session keeps what it wrote.
    expect(provider.requests[1]!.messages[1]).toMatchObject({ content: stored.content });
    expect(entries).toContainEqual(expect.objectContaining({ type: "custom", plugin: "text-tool-calls", kind: "text_tool_call", data: { text, calls: [(stored.content[1] as { id: string }).id] } }));
  });

test("text-tool-calls: broken JSON and another harness's names in a text call are mended by tool-repair", async () => {
  const text = `<tool_call>{"name": "read_file", "arguments": {file_path: ${at.replaceAll('"', "'")}, limit: '1',}}</tool_call>`;
  const { stored, results, entries } = await run(say(text), ["tool-repair", "text-tool-calls"]);
  expect(stored.content).toEqual([{ type: "tool_call", id: expect.any(String), name: "read", input: { path: file, limit: 1 } }]);
  expect(results[0]!.isError).toBe(false);
  const note = (results[0]!.content[0] as { text: string }).text;
  expect(note).toContain('tool "read_file" → "read"');
  expect(note).toContain('argument "file_path" → "path"');
  expect(note).toContain("call tools with native tool calls");
  expect(JSON.stringify(results[0]!.content)).toContain("export const answer = 42;");
  expect(entries).toContainEqual(expect.objectContaining({ type: "tool_repair", original: { name: "read_file", input: {}, rawInput: expect.stringContaining("limit: '1',") } }));
});

test("text-tool-calls: a write cut off where the server stopped on a closing tag in its content does not run (#82)", async () => {
  const out = join(root, "cut.txt");
  writeFileSync(out, "before\n");
  const content = 'Example:\n<tool_call>{"name": "read", "arguments": {"path": "a"}}</tool_call>\nDone.';
  const whole = `<tool_call>${JSON.stringify({ name: "write", arguments: { path: out, content } })}</tool_call>`;
  // The server stops on the inner </tool_call> (and drops it): the reply ends inside the content string.
  const cut = whole.slice(0, whole.indexOf("</tool_call>"));
  expect(extractTextCalls(cut)).toBeUndefined();
  const { stored, results } = await run(say(cut), ["tool-repair", "text-tool-calls"]);
  expect(results).toEqual([]);
  expect(stored.content).toEqual([{ type: "text", text: cut }]);
  expect(readFileSync(out, "utf8")).toBe("before\n");
  // Whole, the tags in its content make it no call either: left alone, as before.
  expect(extractTextCalls(whole)).toBeUndefined();
  // Without its closing tag at the end, a complete body still runs, also one that is only badly quoted.
  expect(extractTextCalls('<tool_call>{"name": "read", "arguments": {"path": "a"}}')?.calls[0]?.name).toBe("read");
  expect(extractTextCalls("<tool_call>{'name': 'read', 'arguments': {'path': 'a'}}")?.calls[0]?.name).toBe("read");
  expect(extractTextCalls('<function=write>{"path": "f", "content": "x <function=read>{\\"path\\": \\"a\\"}')).toBeUndefined();
});

test("text-tool-calls: a call whose JSON cannot be mended still tells the model to call natively (#85)", async () => {
  const { results } = await run(say(`<tool_call>{"name": "read", "arguments": {"path": ${at}, "limit": }}</tool_call>`));
  expect(results).toHaveLength(1);
  expect(results[0]!.isError).toBe(true);
  const text = JSON.stringify(results[0]!.content);
  expect(text).toContain("not valid JSON");
  expect(text).toContain("call tools with native tool calls");
});

test("text-tool-calls: without tool-repair, another harness's tool name still resolves", async () => {
  const { stored, results } = await run(say(`<tool_call>{"name": "read_file", "arguments": {"path": ${at}}}</tool_call>`));
  expect(stored.content).toMatchObject([{ type: "tool_call", name: "read", input: { path: file } }]);
  expect(results[0]!.isError).toBe(false);
});

test("text-tool-calls: leaves alone a reply with a native call, an unknown tool, an example, or signed thinking", async () => {
  const tagged = `<tool_call>{"name": "read", "arguments": {"path": ${at}}}</tool_call>`;
  const native = reply([{ type: "text", text: tagged }, { type: "tool_call", id: "n1", name: "read", input: { path: file } }]);
  const withNative = await run(native);
  expect(withNative.stored.content).toEqual(native.content);
  expect(withNative.results.map((r) => r.toolCallId)).toEqual(["n1"]);

  const untouched = [
    say('<tool_call>{"name": "launch_rockets", "arguments": {"count": 3}}</tool_call>'),
    // A name only a typo away from a tool (head → read) is not taken for it.
    say('Example:\n```json\n{"name": "head", "arguments": {"n": 5}}\n```'),
    say(`To call a tool, write ${tagged} on its own, and the harness runs it.`),
    reply([{ type: "thinking", thinking: "…", signature: "sig" }, { type: "text", text: tagged }]),
    say(tagged, { stopReason: "max_tokens" }),
  ];
  for (const m of untouched) {
    const { provider, stored, results, entries } = await run(m);
    expect(provider.requests).toHaveLength(1);
    expect(stored.content).toEqual(m.content);
    expect(results).toEqual([]);
    expect(entries.some((e) => e.type === "custom" && e.plugin === "text-tool-calls")).toBe(false);
  }
});

test("text-tool-calls: tells the model when an earlier call in the text did not run", async () => {
  const text = `Read a.\n<tool_call>{"name": "read", "arguments": {"path": "a"}}</tool_call>\nAnd this.\n<tool_call>{"name": "read", "arguments": {"path": ${at}}}</tool_call>`;
  const { results } = await run(say(text));
  expect(results).toHaveLength(1);
  expect((results[0]!.content[0] as { text: string }).text).toContain("1 other call written earlier in the text did not run");
});

// What a cloud model may answer when asked what a write call looks like.
const target = join(root, "config.json");
const example = say(`It looks like this:\n\`\`\`json\n{"name": "write", "arguments": {"path": ${JSON.stringify(target)}, "content": "{}"}}\n\`\`\``);

test("text-tool-calls: off for a model served from a public address or with no baseUrl, unless always is set", async () => {
  writeFileSync(target, '{"keep": true}');
  for (const baseUrl of ["https://api.anthropic.com", undefined]) {
    const { provider, stored, results } = await run(example, ["tool-repair", "text-tool-calls"], { baseUrl, mode: "edits" });
    expect(provider.requests).toHaveLength(1);
    expect(stored.content).toEqual(example.content);
    expect(results).toEqual([]);
  }
  expect(readFileSync(target, "utf8")).toBe('{"keep": true}');
  // This machine and the local network count as local; always: true picks calls up from any model.
  const local: [string, Record<string, unknown>][] = [
    ["http://[::1]:8080/v1", {}],
    ["http://192.168.1.20:1234/v1", {}],
    ["http://llama:8080/v1", {}],
    ["http://host.docker.internal:11434/v1", {}],
    ["https://api.example.com/v1", { always: true }],
  ];
  for (const [baseUrl, settings] of local) {
    const { results } = await run(say(formats["<tool_call>"]), undefined, { baseUrl, settings });
    expect([baseUrl, results.length]).toEqual([baseUrl, 1]);
  }
});

test("text-tool-calls: in edits mode a write picked up from text is asked about; a read is not", async () => {
  writeFileSync(target, '{"keep": true}');
  const asked: string[] = [];
  const approve: AgentOptions["approve"] = async (req) => (asked.push(req.reason), { decision: "deny" });
  const { results } = await run(example, ["tool-repair", "text-tool-calls"], { mode: "edits", approve });
  expect(asked).toEqual([expect.stringContaining("written as text")]);
  expect(results[0]!.isError).toBe(true);
  expect(readFileSync(target, "utf8")).toBe('{"keep": true}');
  const read = await run(say(formats["<tool_call>"]), undefined, { mode: "edits", approve });
  expect(read.results[0]!.isError).toBe(false);
  expect(asked).toHaveLength(1);
});

test("text-tool-calls: in agent mode a write picked up from text is asked about, not judged", async () => {
  writeFileSync(target, '{"keep": true}');
  const asked: string[] = [];
  const approve: AgentOptions["approve"] = async (req) => (asked.push(req.reason), { decision: "deny" });
  // A judge asked about the write would answer this, and the write would run unasked.
  const judge = say('{"safe": true, "reason": "inside the working directory"}');
  const { results } = await run(example, ["tool-repair", "text-tool-calls", "agent-judge"], { mode: "agent", approve, then: [judge] });
  expect(asked).toEqual([expect.stringContaining("written as text")]);
  expect(results[0]!.isError).toBe(true);
  expect(readFileSync(target, "utf8")).toBe('{"keep": true}');
});
