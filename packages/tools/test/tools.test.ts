import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { bashTool, editTool, readTool, writeTool } from "../src/index.ts";

let dir: string;
const ctx = () => ({ cwd: dir, signal: new AbortController().signal });
const out = (r: { content: { type: string; text?: string }[] }) => r.content.map((c) => c.text ?? "").join("");

beforeAll(() => {
  dir = mkdtempSync(join(tmpdir(), "sasacode-tools-"));
});
afterAll(() => rmSync(dir, { recursive: true, force: true }));

test("write creates parents, read numbers lines and pages", async () => {
  await writeTool.execute({ path: "a/b/c.txt", content: "one\ntwo\nthree\n" }, ctx());
  expect(readFileSync(join(dir, "a/b/c.txt"), "utf8")).toBe("one\ntwo\nthree\n");
  const all = out(await readTool.execute({ path: "a/b/c.txt" }, ctx()));
  expect(all).toBe("     1\tone\n     2\ttwo\n     3\tthree");
  const page = out(await readTool.execute({ path: "a/b/c.txt", offset: 2, limit: 1 }, ctx()));
  expect(page).toContain("     2\ttwo");
  expect(page).toContain("Showing lines 2-2 of 3. Use offset=3 to continue.");
});

test("read returns images as images and reports missing files", async () => {
  writeFileSync(join(dir, "x.png"), Buffer.from([0x89, 0x50, 0x4e, 0x47]));
  const r = await readTool.execute({ path: "x.png" }, ctx());
  expect(r.content[1]).toMatchObject({ type: "image", mediaType: "image/png" });
  const missing = await readTool.execute({ path: "nope.txt" }, ctx());
  expect(missing.isError).toBe(true);
});

test("edit requires a unique exact match and returns a diff", async () => {
  writeFileSync(join(dir, "e.txt"), "foo\nbar\nfoo\n");
  expect((await editTool.execute({ path: "e.txt", old_string: "baz", new_string: "x" }, ctx())).isError).toBe(true);
  const dup = await editTool.execute({ path: "e.txt", old_string: "foo", new_string: "x" }, ctx());
  expect(dup.isError).toBe(true);
  expect(out(dup)).toContain("appears 2 times");
  const ok = await editTool.execute({ path: "e.txt", old_string: "bar", new_string: "BAR" }, ctx());
  expect(ok.isError).toBeFalsy();
  expect(ok.details?.diff).toContain("-bar\n+BAR");
  await editTool.execute({ path: "e.txt", old_string: "foo", new_string: "$&", replace_all: true }, ctx());
  expect(readFileSync(join(dir, "e.txt"), "utf8")).toBe("$&\nBAR\n$&\n");
});

test("bash reports exit codes, truncates to a file, and times out", async () => {
  const r = await bashTool.execute({ command: "echo hi; exit 3" }, ctx());
  expect(out(r)).toBe("hi\n[exit code 3]");
  expect(r.isError).toBe(true);
  const big = await bashTool.execute({ command: "yes x | head -c 40000" }, ctx());
  const m = /saved to (\S+)\]/.exec(out(big));
  expect(m).not.toBeNull();
  expect(readFileSync(m![1]!, "utf8").length).toBe(40000);
  const slow = await bashTool.execute({ command: "sleep 5", timeout: 0.3 }, ctx());
  expect(out(slow)).toContain("timed out after 0.3s");
});

test("bash abort kills the whole process group", async () => {
  const ac = new AbortController();
  const p = bashTool.execute({ command: "sleep 30 & sleep 30; echo done" }, { cwd: dir, signal: ac.signal });
  setTimeout(() => ac.abort(), 200);
  const t0 = Date.now();
  const r = await p;
  expect(Date.now() - t0).toBeLessThan(3000);
  expect(out(r)).toContain("interrupted by the user");
});
