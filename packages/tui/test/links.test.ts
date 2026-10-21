import { afterAll, expect, test } from "bun:test";
import { homedir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { Markdown, resetCapabilitiesCache, setCapabilityOverrides } from "@earendil-works/pi-tui";
import { linkTarget, localLinks } from "../src/links.ts";
import { markdownTheme } from "../src/theme.ts";

const cwd = join(homedir(), "proj");
const file = (...p: string[]) => pathToFileURL(join(cwd, ...p)).href;
afterAll(() => {
  setCapabilityOverrides({});
  resetCapabilitiesCache();
});

test("a path becomes a file URL the terminal can open; a line suffix is dropped", () => {
  expect(linkTarget("docs/a.md", cwd)).toBe(file("docs", "a.md"));
  expect(linkTarget("./src/x.ts:42", cwd)).toBe(file("src", "x.ts"));
  expect(linkTarget("src/x.ts:42:7", cwd)).toBe(file("src", "x.ts"));
  expect(linkTarget("src/x.ts#L3-L9", cwd)).toBe(file("src", "x.ts"));
  expect(linkTarget("my%20notes.md", cwd)).toBe(file("my notes.md"));
  expect(linkTarget("~/n.md", cwd)).toBe(pathToFileURL(join(homedir(), "n.md")).href);
  const abs = join(homedir(), "other", "b.md");
  expect(linkTarget(abs, cwd)).toBe(pathToFileURL(abs).href);
});

test("URLs and anchors are left alone", () => {
  for (const href of ["https://example.com/a.md", "mailto:a@b.c", "file:///x.md", "#usage", "vscode://file/x"]) {
    expect(linkTarget(href, cwd)).toBe(href);
  }
});

test.if(process.platform === "win32")("a Windows path is a path, not a scheme", () => {
  expect(linkTarget("C:\\work\\a.md", cwd)).toBe("file:///C:/work/a.md");
});

test("only link targets change: link text, code spans and fenced code stay as written", () => {
  const md = [
    "see [docs/a.md](docs/a.md) and [web](https://x.y)",
    "`[t](raw.md)` stays, [b](<b c.md>) too",
    "```md",
    "[in](fence.md)",
    "```",
  ].join("\n");
  expect(localLinks(md, cwd).split("\n")).toEqual([
    `see [docs/a.md](${file("docs", "a.md")}) and [web](https://x.y)`,
    `\`[t](raw.md)\` stays, [b](<${file("b c.md")}>) too`,
    "```md",
    "[in](fence.md)",
    "```",
  ]);
});

test("the rendered terminal link points at the file", () => {
  setCapabilityOverrides({ hyperlinks: true });
  const out = new Markdown("[a](docs/a.md)", 0, 0, markdownTheme, undefined, { transform: (m) => localLinks(m, cwd) }).render(200).join("");
  expect(out).toContain(`\x1b]8;;${file("docs", "a.md")}\x1b\\`);
});
