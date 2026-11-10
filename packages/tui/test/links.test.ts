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

test("a file in the folder with a line is a file, a #heading is not part of the name, and $ in a path stays as written (#115)", () => {
  expect(linkTarget("x.ts:42", cwd)).toBe(file("x.ts"));
  expect(linkTarget("README.md:10", cwd)).toBe(file("README.md"));
  expect(linkTarget("package.json:3:5", cwd)).toBe(file("package.json"));
  expect(linkTarget("Makefile:12", cwd)).toBe(file("Makefile"));
  expect(linkTarget("README.md#setup", cwd)).toBe(file("README.md"));
  expect(linkTarget("docs/guide.md#install", cwd)).toBe(file("docs", "guide.md"));
  for (const href of ["tel:5551234", "localhost:3000/x", "vscode:extension/a.b"]) expect(linkTarget(href, cwd)).toBe(href);
  expect(localLinks("see [a](a$$b.md) and [r](x$&y.md)", cwd)).toBe(`see [a](${file("a$$b.md")}) and [r](${file("x$&y.md")})`);
});

test("the rendered terminal link points at the file", () => {
  setCapabilityOverrides({ hyperlinks: true });
  const out = new Markdown("[a](docs/a.md)", 0, 0, markdownTheme, undefined, { transform: (m) => localLinks(m, cwd) }).render(200).join("");
  expect(out).toContain(`\x1b]8;;${file("docs", "a.md")}\x1b\\`);
});
