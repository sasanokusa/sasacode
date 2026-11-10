import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Agent, PermissionPolicy, PluginHost } from "@sasacode/agent";
import { type AssistantMessage, emptyUsage, registerApi } from "@sasacode/ai";
import { replayProvider } from "@sasacode/testing";
import builtinTools, { editTool, readTool, writeTool } from "@sasacode/tools";
import { bundledPlugins, closestName, detectRepetition, fitToSchema, repairJson, resolveToolName, TOOL_SYNONYMS } from "../src/index.ts";

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
  // Anthropic's text editor calls the content file_text.
  expect(fitToSchema({ path: "a", file_text: "x" }, writeTool.parameters).value).toEqual({ path: "a", content: "x" });
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

test("repetition-guard: a run of newlines is cut where it starts at the end, not at the answer's first paragraph break (#119)", async () => {
  const answer = "## Findings\n\nFirst, src/config.ts parses the port as a string, which is the bug.\n\nSecond, the fix is to wrap it in Number().\n\nThird, a test for it goes in config.test.ts.\n";
  const { agent } = await run([reply([{ type: "text", text: answer + "\n".repeat(900) }], 333), reply([{ type: "text", text: "Done." }], 444)], "repetition-guard");
  const kept = agent.messages[1] as AssistantMessage;
  expect(kept.stopReason).toBe("stopped");
  expect(kept.content).toEqual([{ type: "text", text: answer }]);
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

test("closestName: tool names other harnesses use, only to a tool that is offered", () => {
  const tools = ["read", "write", "edit", "bash", "web_fetch"];
  const said: [string, string][] = [
    ["read_file", "read"], ["view_file", "read"], ["cat", "read"], ["write_file", "write"], ["create_file", "write"],
    ["edit_file", "edit"], ["str_replace", "edit"], ["str_replace_editor", "edit"], ["replace_in_file", "edit"],
    ["run_command", "bash"], ["execute_command", "bash"], ["execute_bash", "bash"], ["run_shell_command", "bash"], ["shell", "bash"], ["terminal", "bash"],
  ];
  for (const [name, meant] of said) expect([name, closestName(name, tools, TOOL_SYNONYMS)]).toEqual([name, meant]);
  expect(closestName("read_file", tools)).toBeUndefined(); // too far for a typo: only the table knows it
  expect(closestName("shell", ["read", "write", "edit"], TOOL_SYNONYMS)).toBeUndefined(); // bash is not offered
  expect(closestName("view", ["read", "view"], TOOL_SYNONYMS)).toBe("view"); // a tool of that very name wins
  expect(closestName("x", ["a", "b"], { a: ["x"], b: ["x"] })).toBeUndefined(); // listed under two: no fix
  // Each spelling belongs to one tool and is no tool's own name, or it would never resolve.
  const norm = (s: string) => s.toLowerCase().replace(/[^a-z0-9]/g, "");
  const spellings = Object.values(TOOL_SYNONYMS).flat().map(norm);
  expect(new Set(spellings).size).toBe(spellings.length);
  expect(spellings.filter((s) => Object.keys(TOOL_SYNONYMS).includes(s))).toEqual([]);
});

test("resolveToolName: an MCP tool's own name wins over a built-in's, and typos only when allowed", () => {
  const names = ["read", "write", "bash", "mcp__github__get_file_contents", "mcp__filesystem__read_file"];
  expect(resolveToolName("get_file_contents", names)).toBe("mcp__github__get_file_contents");
  expect(resolveToolName("get_file_contents", ["read", "bash"])).toBeUndefined(); // GitHub's name, not one for read
  expect(resolveToolName("read_file", names)).toBe("mcp__filesystem__read_file");
  expect(resolveToolName("read_file", ["read"])).toBe("read");
  expect(resolveToolName("read_file", ["read", "mcp__a__read_file", "mcp__b__read_file"])).toBeUndefined(); // two such tools
  expect(resolveToolName("head", names)).toBe("read");
  expect(resolveToolName("head", names, false)).toBeUndefined();
  expect(resolveToolName("functions.read_file", ["read"], false)).toBe("read");
});

test("tool-repair: another harness's tool and argument names run as the built-in tool", async () => {
  const dir = mkdtempSync(join(tmpdir(), "sasacode-repair-"));
  const file = join(dir, "a.ts");
  writeFileSync(file, "const a = 1;\nconst b = 2;\n");
  const call = (id: number, name: string, input: Record<string, unknown>) => reply([{ type: "tool_call", id: String(id), name, input }], id);
  const { provider } = await run(
    [
      call(1, "read_file", { file_path: file }),
      call(2, "str_replace", { path: file, old_str: "const a = 1;", new_str: "const a = 10;" }),
      call(3, "str_replace_editor", { command: "str_replace", path: file, old_str: "const b = 2;", new_str: "const b = 20;" }),
      call(4, "str_replace_editor", { command: "view", path: file }),
      // The command that picks the tool can be inside broken JSON: it is repaired first.
      reply([{ type: "tool_call", id: "5", name: "str_replace_editor", input: {}, rawInput: `{command: 'view', path: ${JSON.stringify(file)},}` }], 5),
      call(6, "run_shell_command", { command: "echo shell-ran" }),
      reply([{ type: "text", text: "done" }], 7),
    ],
    "tool-repair",
  );
  const results = provider.requests.slice(1).map((r) => r.messages.at(-1) as { content: { text: string }[]; isError: boolean });
  expect(results.map((r) => r.isError)).toEqual([false, false, false, false, false, false]);
  expect(results[0]!.content.at(-1)!.text).toContain("const a = 1;");
  expect(readFileSync(file, "utf8")).toBe("const a = 10;\nconst b = 20;\n");
  expect(results[2]!.content[0]!.text).toContain('tool "str_replace_editor" (command "str_replace") → "edit"');
  expect(results[3]!.content.at(-1)!.text).toContain("const b = 20;");
  expect(results[4]!.content.at(-1)!.text).toContain("const b = 20;");
  expect(JSON.stringify(results[5])).toContain("shell-ran");
  // The history keeps each call as it ran: the model sees the built-in names and arguments.
  const history = provider.requests.at(-1)!.messages.flatMap((m) => (m.role === "assistant" ? m.content : []));
  expect(history.map((c) => c.type === "tool_call" && c.name)).toEqual(["read", "edit", "edit", "read", "read", "bash"]);
  expect(history[2]).toMatchObject({ input: { path: file, old_string: "const b = 2;", new_string: "const b = 20;" } });
  expect((history[2] as { input: object }).input).not.toHaveProperty("command");
  expect((history[4] as { input: object }).input).toEqual({ path: file });
  rmSync(dir, { recursive: true, force: true });
});
