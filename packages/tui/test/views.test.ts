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
