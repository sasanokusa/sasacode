import { expect, test } from "bun:test";
import { Agent, PermissionPolicy, PluginHost } from "@sasacode/agent";
import { type AssistantMessage, emptyUsage, registerApi, replayProvider } from "@sasacode/ai";
import builtinTools, { editTool, readTool, writeTool } from "@sasacode/tools";
import { bundledPlugins, closestName, detectRepetition, fitToSchema, repairJson } from "../src/index.ts";

test("repairJson: the usual small-model mistakes", () => {
  const ok = (raw: string) => repairJson(raw)?.value;
  expect(ok('{"a": 1,}')).toEqual({ a: 1 });
  expect(ok('{"a": [1, 2,],}')).toEqual({ a: [1, 2] });
  expect(ok("{path: 'a.txt', recursive: True, x: None}")).toEqual({ path: "a.txt", recursive: true, x: null });
  expect(ok('{"a": {"b": 1')).toEqual({ a: { b: 1 } });
  expect(ok('{"content": "line1\nline2')).toEqual({ content: "line1\nline2" });
  expect(ok('```json\n{"a": 1}\n```')).toEqual({ a: 1 });
  expect(ok('Sure! Here are the arguments: {"a": 1} hope that helps')).toEqual({ a: 1 });
  expect(ok(JSON.stringify(JSON.stringify({ a: 1 })))).toEqual({ a: 1 });
  expect(ok(`{'text': 'it\\'s "quoted"'}`)).toEqual({ text: `it's "quoted"` });
  // Text inside strings is never touched.
  expect(ok('{"a": "x, } True: y",}')).toEqual({ a: "x, } True: y" });
  expect(repairJson("no json here")).toBeUndefined();
  expect(repairJson("[1, 2]")).toBeUndefined(); // arguments must be an object
  expect(repairJson('{"a": 1,}')!.fixes).toEqual(["removed trailing comma"]);
});

test("fitToSchema: rename, convert and drop only when unambiguous", () => {
  expect(fitToSchema({ file: "a.txt", limit: "20", offset: 3.0 }, readTool.parameters)).toEqual({
    value: { path: "a.txt", limit: 20, offset: 3 },
    fixes: ['argument "file" → "path"', 'limit: "20" → 20'],
  });
  expect(fitToSchema({ path: "a", old: "x", new: "y", replace_all: "TRUE" }, editTool.parameters).value).toEqual({
    path: "a",
    old_string: "x",
    new_string: "y",
    replace_all: true,
  });
  // Unknown extra argument with no candidate: dropped only when allowed (read-only tools) and reported.
  expect(fitToSchema({ path: "a", encoding: "utf8" }, readTool.parameters, { dropUnknown: true })).toEqual({
    value: { path: "a" },
    fixes: ['dropped unknown argument "encoding"'],
  });
  // On a tool that changes things it may be a constraint the model meant: kept for validation to report.
  expect(fitToSchema({ file: "a", content: "x", overwrite: false }, writeTool.parameters)).toEqual({
    value: { path: "a", content: "x", overwrite: false },
    fixes: ['argument "file" → "path"'],
  });
  // "20abc" is not a number: left for validation to report.
  expect(fitToSchema({ path: "a", limit: "20abc" }, readTool.parameters).value).toEqual({ path: "a", limit: "20abc" });
  // Nested objects, arrays and enums.
  const schema = {
    type: "object",
    properties: {
      todos: {
        type: "array",
        items: { type: "object", properties: { content: { type: "string" }, status: { type: "string", enum: ["pending", "done"] } } },
      },
    },
  };
  expect(fitToSchema({ todos: '[{"content": 1, "status": "Done"}]' }, schema).value).toEqual({ todos: [{ content: "1", status: "done" }] });
  expect(fitToSchema({ todos: { content: "x", status: "pending" } }, schema).value).toEqual({ todos: [{ content: "x", status: "pending" }] });
});

test("closestName: unique matches only", () => {
  const tools = ["read", "write", "edit", "bash", "web_fetch"];
  expect(closestName("raed", tools)).toBe("read");
  expect(closestName("Bash", tools)).toBe("bash");
  expect(closestName("webFetch", tools)).toBe("web_fetch");
  expect(closestName("rite", tools)).toBe("write");
  expect(closestName("xyz", tools)).toBeUndefined();
  expect(closestName("ab", ["aa", "bb"])).toBeUndefined(); // two equally close candidates
});

const reply = (content: AssistantMessage["content"], ts = Date.now()): AssistantMessage => ({
  role: "assistant",
  content,
  api: "replay",
  provider: "t",
  model: "m",
  usage: emptyUsage(),
  stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop",
  timestamp: ts,
});

async function run(script: AssistantMessage[], plugin: string) {
  const provider = replayProvider(script);
  registerApi("replay", provider);
  const agent = new Agent({
    model: { id: "m", provider: "t", api: "replay", contextWindow: 1e5, maxOutput: 1e3 },
    cwd: import.meta.dir,
    systemPrompt: "",
    permissions: new PermissionPolicy("auto"),
  });
  const host = new PluginHost({ agent, cwd: agent.cwd });
  await host.load("builtin-tools", builtinTools);
  await host.load(plugin, bundledPlugins[plugin]!);
  await agent.prompt("go");
  return { agent, provider };
}

test("tool-repair plugin: a broken call from a small model runs, and the model learns what was fixed", async () => {
  const { provider } = await run(
    [
      reply([{ type: "tool_call", id: "1", name: "functions.Read", input: {}, rawInput: "{file: 'repair.test.ts', limit: '3',}" }]),
      reply([{ type: "text", text: "done" }]),
    ],
    "tool-repair",
  );
  const [assistant, result] = provider.requests[1]!.messages.slice(1);
  expect((assistant as AssistantMessage).content[0]).toEqual({ type: "tool_call", id: "1", name: "read", input: { path: "repair.test.ts", limit: 3 } });
  const text = JSON.stringify(result);
  expect(text).toContain("harness repaired this call");
  expect(text).toContain('tool \\"functions.Read\\" → \\"read\\"');
  expect(text).toContain("import { expect, test }");
  expect(result).toMatchObject({ isError: false });
});

test("detectRepetition: loops are found, ordinary text and short runs are not", () => {
  const limits = { minRepeats: 4, minSpan: 200, minUnit: 10, shortMinSpan: 400 };
  const loop = `Intro text. ${"I will now check the file again. ".repeat(8)}`;
  expect(detectRepetition(loop, limits)).toMatchObject({ unit: "I will now check the file again. ", count: 8 });
  expect(detectRepetition("x".repeat(300), limits)).toBeUndefined(); // a short unit needs a longer run …
  expect(detectRepetition("x".repeat(500), limits)).toMatchObject({ unit: "x" }); // … but is caught then
  expect(detectRepetition("A normal paragraph that goes on for a while without repeating itself. ".repeat(1) + "abc".repeat(10), limits)).toBeUndefined();
  expect(detectRepetition("same line here\n".repeat(3), limits)).toBeUndefined(); // too few copies
});

test("repetition-guard: stops the loop, keeps one copy, and tells the model", async () => {
  const looping = `Let me think. ${"I should read the config file first. ".repeat(30)}`;
  const { agent, provider } = await run([reply([{ type: "text", text: looping }], 111), reply([{ type: "text", text: "Reading it now." }], 222)], "repetition-guard");
  const kept = agent.messages[1] as AssistantMessage;
  expect(kept.stopReason).toBe("stopped");
  expect(kept.content).toEqual([{ type: "text", text: "Let me think. I should read the config file first. " }]);
  expect(JSON.stringify(provider.requests[1]!.messages[2])).toContain("kept repeating the same text");
  expect(agent.messages.at(-1)).toMatchObject({ content: [{ text: "Reading it now." }] });
});

test("detectRepetition: short units need a long run; code and tables do not trip it", () => {
  const limits = { minRepeats: 4, minSpan: 200, minUnit: 10, shortMinSpan: 400 };
  expect(detectRepetition("なるほど。".repeat(100), limits)).toMatchObject({ unit: "なるほど。", count: 100 });
  expect(detectRepetition(`見出し\n${"なるほど。".repeat(30)}`, limits)).toBeUndefined(); // 150 chars: not yet
  const table = ["| a | b |", "| --- | --- |", ...Array.from({ length: 40 }, (_, i) => `| row ${i} | ${i * 7} |`)].join("\n");
  expect(detectRepetition(table, limits)).toBeUndefined();
  const code = Array.from({ length: 60 }, (_, i) => `  const v${i} = values[${i}] ?? 0;\n`).join("");
  expect(detectRepetition(code, limits)).toBeUndefined();
  expect(detectRepetition(`${"=".repeat(80)}\nTitle\n${"=".repeat(80)}`, limits)).toBeUndefined();
});

test("tool-repair keeps a mutating tool's unknown argument, so the model is asked again", async () => {
  const { provider } = await run(
    [
      reply([{ type: "tool_call", id: "1", name: "write", input: { path: "never-written.txt", content: "x", overwrite: false } }], 11),
      reply([{ type: "text", text: "ok" }], 12),
    ],
    "tool-repair",
  );
  const result = JSON.stringify(provider.requests[1]!.messages.at(-1));
  expect(result).toContain("input.overwrite is not an allowed property");
});
