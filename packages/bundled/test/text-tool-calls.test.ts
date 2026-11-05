import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Agent, PermissionPolicy, PluginHost, SessionFile } from "@sasacode/agent";
import { type AssistantContent, type AssistantMessage, emptyUsage, registerApi, type ToolResultMessage } from "@sasacode/ai";
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

async function run(first: AssistantMessage, plugins = ["text-tool-calls"]) {
  const provider = replayProvider([first, say("done")]);
  registerApi("replay", provider);
  const session = SessionFile.create(join(root, "sessions"), root);
  const agent = new Agent({
    model: { id: "m", provider: "t", api: "replay", contextWindow: 1e5, maxOutput: 1e3 },
    cwd: root,
    systemPrompt: "",
    permissions: new PermissionPolicy("auto"),
    session,
  });
  const host = new PluginHost({ agent, cwd: root });
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
  });
  // Several in a row; Hermes puts the arguments first; a missing closing tag at the very end.
  expect(extractTextCalls('<tool_call>{"arguments": {"path": "a"}, "name": "read"}</tool_call>\n<tool_call>{"name": "bash", "arguments": {"command": "ls"}}')?.calls).toEqual([
    call("read", '{"path":"a"}'),
    call("bash", '{"command":"ls"}'),
  ]);
  expect(extractTextCalls('<function=read>{"path": "a.ts"}</function>')).toEqual({ prose: "", calls: [call("read", '{"path": "a.ts"}')] });
  expect(extractTextCalls('Reading it.\n```json\n{"name": "read", "parameters": {"path": "a.ts"}}\n```\n')?.calls).toEqual([call("read", '{"path":"a.ts"}')]);
  // Broken JSON: the arguments are cut out as written, for tool-repair.
  expect(extractTextCalls("<tool_call>{\"name\": \"read\", \"arguments\": {path: 'a.ts',}}</tool_call>")?.calls).toEqual([call("read", "{path: 'a.ts',}")]);
  expect(extractTextCalls('<tool_call>{"arguments": {"path": "a",}, "name": "read"}</tool_call>')?.calls).toEqual([call("read", '{"path": "a",}')]);
  // Not calls: an example followed by more text, one shown in a code block, a tool definition,
  // a JSON answer after a long explanation, and a call that cannot be read.
  expect(extractTextCalls('Write <tool_call>{"name": "read", "arguments": {"path": "a"}}</tool_call> and it runs.')).toBeUndefined();
  expect(extractTextCalls('The format:\n```\n<tool_call>{"name": "read", "arguments": {"path": "a"}}</tool_call>\n```')).toBeUndefined();
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
