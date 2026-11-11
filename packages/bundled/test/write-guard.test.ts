// write-guard: Gemma4の計測（#68）で write がファイルを壊した2つの形を、実際の呼び出しの形で再現する。
import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Agent, PermissionPolicy, PluginHost } from "@sasacode/agent";
import { type AssistantMessage, emptyUsage, registerApi } from "@sasacode/ai";
import { replayProvider } from "@sasacode/testing";
import builtinTools, { readTool } from "@sasacode/tools";
import { bundledPlugins, stripLineNumbers, stripPadding } from "../src/index.ts";

const root = mkdtempSync(join(tmpdir(), "sasacode-write-guard-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

const reply = (content: AssistantMessage["content"]): AssistantMessage => ({
  role: "assistant", content, api: "replay", provider: "t", model: "m", usage: emptyUsage(),
  stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop", timestamp: 0,
});
const write = (id: string, path: string, content: string) => reply([{ type: "tool_call", id, name: "write", input: { path, content } }]);

/** The writes run one per response through the real tools, tool-repair and write-guard; the results the model got. */
async function run(writes: AssistantMessage[], settings: Record<string, unknown> = {}) {
  registerApi("replay", replayProvider([...writes, reply([{ type: "text", text: "done" }])]));
  const agent = new Agent({ model: { id: "m", provider: "t", api: "replay", contextWindow: 1e5, maxOutput: 1e3 }, cwd: root, systemPrompt: "", permissions: new PermissionPolicy("auto") });
  const host = new PluginHost({ agent, cwd: root, settings: (name) => (name === "write-guard" ? settings : {}) });
  await host.load("builtin-tools", builtinTools);
  await host.load("tool-repair", bundledPlugins["tool-repair"]!);
  await host.load("write-guard", bundledPlugins["write-guard"]!);
  await agent.prompt("go");
  return agent.messages.flatMap((m) => (m.role === "tool" ? [{ text: m.content.map((c) => (c.type === "text" ? c.text : "")).join("\n"), isError: m.isError }] : []));
}

const numbered = async (path: string) => {
  const r = await readTool.execute({ path }, { cwd: root, signal: new AbortController().signal });
  return (r.content[0] as { text: string }).text;
};

test("content copied from read with its line numbers: written without them, and the model is told", async () => {
  const file = join(root, "permission.test.ts");
  const text = `${Array.from({ length: 108 }, (_, i) => (i % 9 === 4 ? "" : `line ${i + 1}\tafter a tab`)).join("\n")}\n`;
  writeFileSync(file, text.replace("line 2\t", "old 2\t"));
  // As Gemma4 e2b sent it: read's output, the last line's padding one space too wide.
  const content = (await numbered(file)).replace("old 2", "line 2").replace("   108\t", "    108\t");
  const [result] = await run([write("1", "permission.test.ts", content)]);
  expect(result!.text).toContain("removed read's line numbers from content (108 lines)");
  expect(result!.isError).toBe(false);
  // read does not show the final newline, so the content had none.
  expect(readFileSync(file, "utf8")).toBe(text.slice(0, -1));
});

test("a file written back with only read's numbers and tabs taken off: the padding left on every line is removed, and the model is told", async () => {
  const file = join(root, "padded.test.ts");
  const text = `${Array.from({ length: 30 }, (_, i) => (i % 7 === 3 ? "" : i % 2 ? `  expect(${i}).toBe(${i});` : `test("t${i}", () => {});`)).join("\n")}\n`;
  writeFileSync(file, text);
  // As Gemma4 e2b sent it (2026-11-11): "     1\t…" became "     …" on every line, one line changed.
  const content = text.replace("toBe(1)", "toBe(2)").split("\n").map((l) => (l ? `     ${l}` : l)).join("\n");
  const [result] = await run([write("1", "padded.test.ts", content)]);
  expect(result!.text).toContain("removed the 5 spaces every line of content started with");
  expect(readFileSync(file, "utf8")).toBe(text.replace("toBe(1)", "toBe(2)"));
});

test("a fragment over a long file is refused; the same write again goes through", async () => {
  const file = join(root, "edit.ts");
  const text = readFileSync(join(import.meta.dir, "../../tools/src/edit.ts"), "utf8");
  writeFileSync(file, text);
  // Gemma4 e4b's write: 5 lines of edit.ts's 81, one of them changed.
  const fragment = "    const diff = lineDiff(before, after);\n    return {\n      content: [text(`Edited ${path} (1 replacement).`)],\n      details: { path, changed: true, diff },\n    };";
  const [first, again] = await run([write("1", "edit.ts", fragment), write("2", "edit.ts", fragment)]);
  expect(first!.isError).toBe(true);
  expect(first!.text).toContain(`write-guard: edit.ts has ${text.split("\n").length - 1} lines and this content has 5`); // edit.ts changes
  expect(first!.text).toContain("use edit");
  expect(again!.text).toContain("Overwrote");
  expect(readFileSync(file, "utf8")).toBe(fragment);
});

test("ordinary writes pass: short files, moderate shrinking, new files, a TSV that counts in its first column", async () => {
  const lines = (n: number) => Array.from({ length: n }, (_, i) => `row ${i}`).join("\n");
  writeFileSync(join(root, "short.txt"), lines(19));
  writeFileSync(join(root, "long.txt"), lines(40));
  const tsv = "1\tAlice\n2\tBob\n3\tCarol\n";
  const results = await run([write("1", "short.txt", "x"), write("2", "long.txt", lines(20)), write("3", "data.tsv", tsv)]);
  expect(results.map((r) => r.isError)).toEqual([false, false, false]);
  expect(results.map((r) => r.text).join()).not.toContain("harness repaired");
  expect(readFileSync(join(root, "data.tsv"), "utf8")).toBe(tsv);
});

test("line numbers that do not count up (a line dropped or added among them) are refused, not written", async () => {
  const content = "     1\tone\n     2\ttwo\n     4\tfour\nadded\n     5\tfive";
  const [first, again] = await run([write("1", "gap.txt", content), write("2", "gap.txt", content)]);
  expect(first!.text).toContain("write-guard: 4 of content's 5 lines start with a line number as read shows it");
  expect(again!.isError).toBe(false);
  expect(readFileSync(join(root, "gap.txt"), "utf8")).toBe(content);
});

test("each guard can be turned off", async () => {
  writeFileSync(join(root, "off.txt"), Array.from({ length: 30 }, () => "x").join("\n"));
  const [shrunk, kept] = await run([write("1", "off.txt", "y"), write("2", "nums.txt", "     1\ta\n     2\tb")], { shrink: false, lineNumbers: false });
  expect([shrunk!.isError, kept!.isError]).toEqual([false, false]);
  expect(readFileSync(join(root, "nums.txt"), "utf8")).toBe("     1\ta\n     2\tb");
});

test("stripLineNumbers: blank lines with or without their tab, CRLF, a final newline; anything else is left alone", () => {
  expect(stripLineNumbers("     7\ta\r\n     8\t\r\n     9\n    10\tb\n")).toBe("a\r\n\r\n\nb\n");
  expect(stripLineNumbers("     1\ta\nb")).toBeUndefined();
  expect(stripLineNumbers("1\ta\n2\tb")).toBeUndefined();
  expect(stripLineNumbers("     2\ta\n     1\tb")).toBeUndefined();
  expect(stripLineNumbers("")).toBeUndefined();
});

test("stripLineNumbers: indented numbers with no tab anywhere are the file's text (#92)", () => {
  for (const s of [" 1\n 2\n 3\n", "  10\n  11\n  12\n", "    1\n    2\n", " 7\n", "       7\n"]) expect([s, stripLineNumbers(s)]).toEqual([s, undefined]);
  expect(stripLineNumbers("  1\tfoo\n")).toBe("foo\n");
  expect(stripLineNumbers("     1\ta\n     2\n     3\tb\n")).toBe("a\n\nb\n"); // a blank line among numbered ones
});

test("stripPadding: only spaces read could have left, beyond the file's own indentation, on a file written back", () => {
  const file = "def f():\n    return 1\n\nprint(f())\n";
  expect(stripPadding("     def f():\n         return 1\n\n     print(f())\n", file)).toEqual({ content: file, spaces: 5 });
  // A file whose lines all share an indentation keeps it.
  const nested = "  a: 1\n  b: 2\n  c: 3\n";
  expect(stripPadding("     a: 1\n     b: 2\n     c: 3\n", nested)).toEqual({ content: nested, spaces: 3 });
  // Not read's padding: more than 5 spaces, a tab, or no extra indentation at all.
  expect(stripPadding("        a\n        b\n        c\n", "a\nb\nc\n")).toBeUndefined();
  expect(stripPadding("\ta\n\tb\n\tc\n", "a\nb\nc\n")).toBeUndefined();
  expect(stripPadding("a\nb\nc\n", "a\nb\nc\n")).toBeUndefined();
  // New text, not the file written back, or too short to tell: left as the model wrote it.
  expect(stripPadding("  x\n  y\n  z\n", "a\nb\nc\n")).toBeUndefined();
  expect(stripPadding("  a\n  b\n", "a\nb\n")).toBeUndefined();
});

test("a new file, or one indented on purpose, is written as it is", async () => {
  const yaml = "  - a\n  - b\n  - c\n";
  writeFileSync(join(root, "list.yml"), "x: 1\ny: 2\nz: 3\n");
  const results = await run([write("1", "new.yml", yaml), write("2", "list.yml", yaml)]);
  expect(results.map((r) => r.text).join()).not.toContain("harness repaired");
  expect(readFileSync(join(root, "new.yml"), "utf8")).toBe(yaml);
  expect(readFileSync(join(root, "list.yml"), "utf8")).toBe(yaml);
});
