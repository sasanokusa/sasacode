import { expect, test } from "bun:test";
import { TextViewer } from "../src/text-viewer.ts";
const plain = (lines: string[]) => lines.join("\n").replace(/\x1b\[[0-9;]*m/g, "");

test("pager scrolls, searches forwards/backwards and closes once", () => {
  let closed = 0;
  const viewer = new TextViewer({ title: "Preview", text: Array.from({ length: 30 }, (_, i) => `row-${String(i).padStart(2, "0")}`).join("\n") },
    () => { closed++; }, async () => {}, () => {}, () => 4, "en");
  expect(plain(viewer.render(90))).toContain("row-00");
  viewer.handleInput(" "); expect(plain(viewer.render(90))).toContain("row-04");
  viewer.handleInput("/"); for (const key of "row-2") viewer.handleInput(key); viewer.handleInput("\r");
  expect(plain(viewer.render(90))).toContain("row-20");
  viewer.handleInput("n"); expect(plain(viewer.render(90))).toContain("row-21");
  viewer.handleInput("N"); expect(plain(viewer.render(90))).toContain("row-20");
  viewer.handleInput("G"); expect(plain(viewer.render(90))).toContain("row-29");
  viewer.handleInput("g"); expect(plain(viewer.render(90))).toContain("row-00");
  viewer.handleInput("\x1b"); viewer.handleInput("q"); expect(closed).toBe(1);
});

test("untrusted text cannot emit control sequences and only a copy key writes the clipboard", async () => {
  const copied: string[] = [];
  const viewer = new TextViewer({ title: "Title\x1b[2J", text: "# heading\n[link](https://example.invalid)\n\x1b]52;c;evil\x07\n<script>bad</script>", format: "markdown" },
    () => {}, async (s) => { copied.push(s); }, () => {}, () => 12, "en");
  const rendered = viewer.render(100).join("\n");
  expect(rendered).not.toContain("\x1b]"); expect(rendered).not.toContain("\x1b[2J"); expect(rendered).toContain("heading");
  expect(copied).toEqual([]);
  viewer.handleInput("y"); await Promise.resolve();
  expect(copied).toHaveLength(1); expect(copied[0]).not.toContain("\x1b");
});

test("escape exits search before closing the viewer and narrow layouts stay bounded", () => {
  let closed = false;
  const viewer = new TextViewer({ title: "A long title", text: "text\n".repeat(100) }, () => { closed = true; }, async () => {}, () => {}, () => 3);
  viewer.render(10); viewer.handleInput("/"); viewer.handleInput("\x1b");
  expect(closed).toBe(false); expect(viewer.render(10).length).toBeLessThanOrEqual(6);
  viewer.handleInput("\x1b"); expect(closed).toBe(true);
});
