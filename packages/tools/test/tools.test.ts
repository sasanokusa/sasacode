import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync, statSync, utimesSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { bashTool, editTool, readTool, writeTool } from "../src/index.ts";

let dir: string;
const ctx = () => ({ cwd: dir, signal: new AbortController().signal });
const out = (r: { content: { type: string; text?: string }[] }) => r.content.map((c) => c.text ?? "").join("");

beforeAll(() => {
  dir = mkdtempSync(join(tmpdir(), "sasacode-tools-"));
  process.env.SASACODE_HOME = join(dir, ".home"); // where long outputs are saved
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

test("bash: a character split across two writes stays whole", async () => {
  // "あ" is e3 81 82: the first two bytes, a pause, then the last one.
  const r = await bashTool.execute({ command: "printf '\\343\\201'; sleep 0.2; printf '\\202\\n'" }, ctx());
  expect(out(r)).toStartWith("あ\n");
});

test("bash: a flood of output is kept to its tail in memory and saved whole, privately", async () => {
  const r = await bashTool.execute({ command: "for i in $(seq 1 20000); do echo line-$i; done" }, ctx());
  const text = out(r);
  const file = /saved to (\S+)\]/.exec(text)![1]!;
  expect(file).toContain(join(process.env.SASACODE_HOME ?? "", "tmp"));
  expect(readFileSync(file, "utf8").split("\n")[0]).toBe("line-1");
  expect(text).toContain("line-20000");
  const { statSync } = await import("node:fs");
  // Unix permission bits; on Windows the file is private through the ACL of the user profile it is in.
  if (process.platform !== "win32") expect(statSync(file).mode & 0o777).toBe(0o600);
});

test("read: pipes and devices are refused, big images too, big files read only as far as shown", async () => {
  if (process.platform !== "win32") {
    // Windows has no named pipes in the file system.
    Bun.spawnSync(["mkfifo", join(dir, "fifo")]);
    expect(out(await readTool.execute({ path: "fifo" }, ctx()))).toContain("not a regular file");
  }
  writeFileSync(join(dir, "huge.png"), Buffer.alloc(5_000_001));
  expect(out(await readTool.execute({ path: "huge.png" }, ctx()))).toContain("over the 5000000 bytes");
  writeFileSync(join(dir, "big.log"), `${"x".repeat(99)}\n`.repeat(120_000)); // 12 MB
  const r = out(await readTool.execute({ path: "big.log", limit: 3 }, ctx()));
  expect(r).toContain("Showing lines 1-3 of a 12 MB file. Use offset=4");
  // 100,000 characters come before 2,000 lines here, and the note says so in its own words (#66).
  expect(out(await readTool.execute({ path: "big.log" }, ctx()))).toContain("Showing lines 1-934 of a 12 MB file. Use offset=935");
});


test("an identical write is a no-op: contents, mtime and inode stay unchanged", async () => {
  const path = join(dir, "same.txt");
  expect((await writeTool.execute({ path, content: "same" }, ctx())).details?.changed).toBe(true);
  utimesSync(path, new Date(1000), new Date(1000));
  const before = statSync(path);
  const result = await writeTool.execute({ path, content: "same" }, ctx());
  expect(result.details?.changed).toBe(false);
  expect(result.details?.diff).toBeUndefined();
  expect(statSync(path).mtimeMs).toBe(before.mtimeMs);
  expect(statSync(path).ino).toBe(before.ino);
  expect(readFileSync(path, "utf8")).toBe("same");
  expect((await writeTool.execute({ path, content: "changed" }, ctx())).details?.changed).toBe(true);
});

test("write counts lines as read does: a final newline is not one more, empty is none (#47)", async () => {
  const cases: [string, number][] = [["hello\n", 1], ["a\nb\n", 2], ["", 0], ["no-newline", 1]];
  for (const [content, n] of cases) {
    const out = await writeTool.execute({ path: `count-${n}-${content.length}.txt`, content }, ctx());
    expect(JSON.stringify(out.content)).toContain(`(${n} lines)`);
  }
});

test("edit: an exact match is replaced as given, with no note", async () => {
  writeFileSync(join(dir, "plain.txt"), "\tkeep\r\nme\r\n");
  const r = await editTool.execute({ path: "plain.txt", old_string: "me", new_string: "you\nthere" }, ctx());
  expect(out(r)).toBe(`Edited ${join(dir, "plain.txt")} (1 replacement).`);
  expect(readFileSync(join(dir, "plain.txt"), "utf8")).toBe("\tkeep\r\nyou\nthere\r\n");
});

test("edit: read's line numbers are ignored when every line of old_string has one (#65)", async () => {
  const path = join(dir, "numbered.ts");
  writeFileSync(path, "function f() {\n\n  return 1;\n}\n");
  const r = await editTool.execute({ path, old_string: "     1\tfunction f() {\n     2\t\n     3\t  return 1;", new_string: "     1\tfunction f() {\n     3\t  return 2;" }, ctx());
  expect(out(r)).toBe(`[harness matched old_string ignoring line-number prefixes]\nEdited ${path} (1 replacement).`);
  expect(readFileSync(path, "utf8")).toBe("function f() {\n  return 2;\n}\n");
  // One line without a number: old_string is taken as it is, and is not found.
  const mixed = await editTool.execute({ path, old_string: "     1\tfunction f() {\n  return 2;", new_string: "x" }, ctx());
  expect(out(mixed)).toContain("not found");
  expect(readFileSync(path, "utf8")).toBe("function f() {\n  return 2;\n}\n");
});

test("edit: a TSV row's \"123\\t\" is the file's text, not read's 6-column line number (#84)", async () => {
  const path = join(dir, "rows.tsv");
  writeFileSync(path, "123\tfoo\n456\tbar\nfoo\nbar\n");
  // A space too many at the end: matched ignoring it, on the TSV rows, not on the plain lines below.
  const r = await editTool.execute({ path, old_string: "123\tfoo \n456\tbar", new_string: "123\tFOO\n456\tBAR" }, ctx());
  expect(out(r)).toStartWith("[harness matched old_string ignoring trailing whitespace and line endings]\n");
  expect(readFileSync(path, "utf8")).toBe("123\tFOO\n456\tBAR\nfoo\nbar\n");
  // read's own numbers on such a row are still taken off.
  const read = await editTool.execute({ path, old_string: "     1\t123\tFOO", new_string: "     1\t123\tF" }, ctx());
  expect(out(read)).toStartWith("[harness matched old_string ignoring line-number prefixes]\n");
  expect(readFileSync(path, "utf8")).toBe("123\tF\n456\tBAR\nfoo\nbar\n");
});

test("edit: trailing whitespace and CRLF are ignored both ways, and the file keeps its line endings (#65)", async () => {
  const crlf = join(dir, "crlf.txt");
  writeFileSync(crlf, "a\r\nb  \r\nc\r\n");
  const r = await editTool.execute({ path: crlf, old_string: "a\nb\nc", new_string: "a\nB\nc" }, ctx());
  expect(out(r)).toStartWith("[harness matched old_string ignoring trailing whitespace and line endings]\n");
  expect(readFileSync(crlf, "utf8")).toBe("a\r\nB\r\nc\r\n");
  const lf = join(dir, "lf.txt");
  writeFileSync(lf, "x\ny\nz\n");
  expect((await editTool.execute({ path: lf, old_string: "x\r\ny \r\n", new_string: "x\r\nY\r\n" }, ctx())).isError).toBeFalsy();
  expect(readFileSync(lf, "utf8")).toBe("x\nY\nz\n");
  // Found once only the whitespace at the end is dropped: the note still names what was ignored.
  const end = await editTool.execute({ path: lf, old_string: "Y\nz  ", new_string: "Y\nZ" }, ctx());
  expect(out(end)).toStartWith("[harness matched old_string ignoring trailing whitespace and line endings]\n");
  expect(readFileSync(lf, "utf8")).toBe("x\nY\nZ\n");
  // The same past line numbers: both are named.
  const numbered = await editTool.execute({ path: lf, old_string: "     1\tx  ", new_string: "     1\tX" }, ctx());
  expect(out(numbered)).toStartWith("[harness matched old_string ignoring line-number prefixes, trailing whitespace and line endings]\n");
  expect(readFileSync(lf, "utf8")).toBe("X\nY\nZ\n");
});

test("edit: other indentation is ignored, and new_string takes the file's (#65)", async () => {
  const path = join(dir, "indent.ts");
  writeFileSync(path, "function f() {\n\tif (x) {\n\t\treturn 1;\n\t}\n}\n");
  // Dedented by a level: every line of new_string gets the file's indentation in front.
  const r = await editTool.execute({ path, old_string: "if (x) {\n\treturn 1;\n}", new_string: "if (x) {\n\treturn 2;\n}" }, ctx());
  expect(out(r)).toStartWith("[harness matched old_string ignoring trailing whitespace and line endings, indentation]\n");
  expect(readFileSync(path, "utf8")).toBe("function f() {\n\tif (x) {\n\t\treturn 2;\n\t}\n}\n");
  // Spaces for tabs: each indentation old_string has becomes the file's for it, at every depth.
  const spaces = { path, old_string: "    if (x) {\n        return 2;\n    }\n", new_string: "    if (y) {\n        return 3;\n    }\n" };
  expect((await editTool.execute(spaces, ctx())).isError).toBeFalsy();
  expect(readFileSync(path, "utf8")).toBe("function f() {\n\tif (y) {\n\t\treturn 3;\n\t}\n}\n");
});

test("edit: lines shallower than the first, and other indentation units, land at the file's depths (#65)", async () => {
  const cases = [
    // A whole snippet dedented by one level that runs past the end of a block.
    ["def f(a):\n    if a:\n        x = 1\n    y = 2\n    return x + y\n", "    x = 1\ny = 2", "    x = 3\ny = 2", "        x = 3\n    y = 2\n    return x + y\n"],
    ["function f() {\n  if (a) {\n    return 1;\n  }\n}\n", "  return 1;\n}", "  return 2;\n}", "    return 2;\n  }\n}\n"],
    // A depth old_string does not have takes the one shift its lines share.
    ["function f() {\n  if (a) {\n    return 1;\n  }\n}\n", "  return 1;\n}", "  if (b)\n    return 2;\n}", "    if (b)\n      return 2;\n  }\n}\n"],
    // Tabs in a file of spaces, four spaces in a file of tabs, two spaces in a file of four.
    ["def f(x):\n    if x:\n        a()\n        b()\n", "\tif x:\n\t\ta()", "\tif x:\n\t\ta2()", "    if x:\n        a2()\n        b()\n"],
    ["def f(x):\n\tif x:\n\t\ta()\n\t\tb()\n", "    if x:\n        a()", "    if x:\n        a2()", "\tif x:\n\t\ta2()\n\t\tb()\n"],
    ["def f(x):\n    if x:\n        a()\n        b()\n", "  if x:\n    a()", "  if x:\n    a2()", "    if x:\n        a2()\n        b()\n"],
    ["class A:\n    def f(self):\n        return x\n\n    def g(self):\n", "    return x\n\n  def g(self):", "    return y\n\n  def g(self):", "        return y\n\n    def g(self):\n"],
  ];
  for (const [file, old_string, new_string, tail] of cases) {
    const path = join(dir, "depths.txt");
    writeFileSync(path, file!);
    expect(out(await editTool.execute({ path, old_string: old_string!, new_string: new_string! }, ctx()))).toContain("ignoring trailing whitespace and line endings, indentation]");
    expect(readFileSync(path, "utf8")).toEndWith(tail!);
  }
  // A depth old_string does not have, with no one shift for all its lines: refused rather than guessed.
  const path = join(dir, "depths.txt"), file = "f() {\n\tif (x) {\n\t\treturn 1;\n\t}\n}\n";
  writeFileSync(path, file);
  const deeper = { path, old_string: "    if (x) {\n        return 1;\n    }", new_string: "    if (x) {\n        if (y)\n            log();\n        return 1;\n    }" };
  const r = await editTool.execute(deeper, ctx());
  expect(r.isError).toBe(true);
  expect(out(r)).toContain("indentation does not map onto the file's");
  expect(readFileSync(path, "utf8")).toBe(file);
});

test("edit: a depth old_string lacks takes the shared shift only when two depths or unmoved lines show it (#65)", async () => {
  const path = join(dir, "onedepth.py");
  // One depth shows no unit: "    \tz()", "      z()" and "\t    z()" would be TabError or IndentationError.
  for (const [file, old_string, new_string] of [
    ["def f(x):\n    if x:\n        a()\n    return 1\n", "\tif x:", "\tif x:\n\t\tz()"],
    ["def f(x):\n  if x:\n    a()\n  return 1\n", "    if x:", "    if x:\n        z()"],
    ["def f(x):\n\tif x:\n\t\ta()\n\treturn 1\n", "    if x:", "    if x:\n        z()"],
  ]) {
    writeFileSync(path, file!);
    const r = await editTool.execute({ path, old_string: old_string!, new_string: new_string! }, ctx());
    expect(r.isError).toBe(true);
    expect(out(r)).toContain("indentation does not map onto the file's");
    expect(readFileSync(path, "utf8")).toBe(file!);
  }
  // Matched past line numbers or trailing whitespace, the lines did not move, so a new depth is kept as written.
  writeFileSync(path, "foo()\n");
  await editTool.execute({ path, old_string: "     1\tfoo()", new_string: "     1\tfoo()\n     2\t  bar()" }, ctx());
  expect(readFileSync(path, "utf8")).toBe("foo()\n  bar()\n");
});

test("edit: a loose match must be unique, replace_all or not; nothing is changed otherwise (#65)", async () => {
  const path = join(dir, "twice.ts");
  writeFileSync(path, "\tfoo();\n  foo();\n");
  for (const replace_all of [false, true]) {
    const r = await editTool.execute({ path, old_string: "    foo();", new_string: "bar();", replace_all }, ctx());
    expect(r.isError).toBe(true);
    expect(out(r)).toContain("appears more than once");
  }
  const none = await editTool.execute({ path, old_string: "    baz();", new_string: "bar();" }, ctx());
  expect(out(none)).toContain("even ignoring");
  // Blank once trailing whitespace is ignored: that is everywhere, so it is refused as ambiguous.
  expect(out(await editTool.execute({ path, old_string: " \t ", new_string: "x" }, ctx()))).toContain("appears more than once");
  expect(readFileSync(path, "utf8")).toBe("\tfoo();\n  foo();\n");
  // Overlapping places count too: with indentation ignored, nested closers fit at lines 3-4 and at 4-5.
  for (const [file, old_string] of [["func f() {\n\tif x {\n\t\t}\n\t}\n}\n", "    }\n}"], ["a \na \na \n", "a\na"]]) {
    writeFileSync(path, file!);
    expect(out(await editTool.execute({ path, old_string: old_string!, new_string: "z" }, ctx()))).toContain("appears more than once");
    expect(readFileSync(path, "utf8")).toBe(file!);
  }
});

test("edit: read's output is taken as copied, a CRLF file's CRs too (#65)", async () => {
  const path = join(dir, "copied.ts");
  writeFileSync(path, "function f() {\r\n  return 1;\r\n}\r\n");
  await editTool.execute({ path, old_string: "     1\tfunction f() {\r\n     2\t  return 1;\r", new_string: "     1\tfunction f() {\n     2\t  return 2;" }, ctx());
  expect(readFileSync(path, "utf8")).toBe("function f() {\r\n  return 2;\r\n}\r\n");
  // A last line of only indentation is where the next line starts, so that line keeps its own.
  writeFileSync(path, "foo \n  next\n");
  await editTool.execute({ path, old_string: "foo\n  ", new_string: "bar\n  " }, ctx());
  expect(readFileSync(path, "utf8")).toBe("bar\n  next\n");
});

test("edit: a line of only digits among read's line numbers is refused as ambiguous, and the file is left as it was (#65)", async () => {
  const path = join(dir, "digits.txt"), imports = `${"\n".repeat(9)}import a\nimport b\n\nfn main() {}\n`;
  const cases = [
    // A blank line's number without its tab: deleting the line before it, the same in Python, inserting without renumbering.
    [imports, "    10\timport a\n    11\timport b\n    12\n    13\tfn main() {}", "    10\timport a\n    12\n    13\tfn main() {}"],
    ["def f():\n    x = 1\n    y = 2\n\n    return x\n", "     2\t    x = 1\n     3\t    y = 2\n     4\n     5\t    return x", "     2\t    x = 1\n     4\n     5\t    return x"],
    ["a\n\nb\n", "     1\ta\n     2\n     3\tb", "     1\ta\n     2\tins\n     2\n     3\tb"],
    ["function f() {\n\n  return 1;\n}\n", "     1\tfunction f() {\n     2\n     3\t  return 1;", "     1\tfunction f() {\n     2\n     3\t  return 2;"],
    ["a\n\n\nb\n", "     1\ta\n     2\n     3\n     4\tb", "     1\ta\n     2\n     3\tc\n     4\tb"],
    // Text of only digits: a line added without a number, at any width, a new_string written without numbers, and old_string's own.
    ["let v = [\n  1,\n  2\n];\n", "     2\t  1,\n     3\t  2", "     2\t  1,\n     3\t  2,\n  3"],
    ["let v = [\n  1,\n  2\n];\n", "     2\t  1,\n     3\t  2", "     2\t  1,\n     3\t  2,\n  1000"],
    ["fn f() -> i32 {\n    1\n}\n", "     1\tfn f() -> i32 {\n     2\t    1\n     3\t}", "     1\tfn f() -> i32 {\n    0\n     3\t}"],
    ["fn f() -> i32 {\n    1\n}\n", "     1\tfn f() -> i32 {\n     2\t    1\n     3\t}", "     1\tfn f() -> i32 {\n    42\n     3\t}"],
    ["a\nb\n", "     1\ta\n     2\tb", "     1\ta\n     2\tb\n100000"],
    ["a\nb\n", "     1\ta", "0\na"],
    ["foo\n42\n", "     1\tfoo\n42", "     1\tbar\n43"],
    ["foo\n    42\n", "     1\tfoo\n    42", "     1\tbar\n    43"],
  ];
  for (const [file, old_string, new_string] of cases) {
    writeFileSync(path, file!);
    const r = await editTool.execute({ path, old_string: old_string!, new_string: new_string! }, ctx());
    expect(r.isError).toBe(true);
    expect(out(r)).toContain("line-number form is ambiguous there");
    expect(readFileSync(path, "utf8")).toBe(file!);
  }
  // With the tab read gives a blank line, the same edits are made as meant.
  writeFileSync(path, imports);
  await editTool.execute({ path, old_string: "    10\timport a\n    11\timport b\n    12\t\n    13\tfn main() {}", new_string: "    10\timport a\n    12\t\n    13\tfn main() {}" }, ctx());
  expect(readFileSync(path, "utf8")).toBe(`${"\n".repeat(9)}import a\n\nfn main() {}\n`);
  writeFileSync(path, "a\n\nb\n");
  await editTool.execute({ path, old_string: "     1\ta\n     2\t\n     3\tb", new_string: "     1\ta\n     2\tins\n     2\t\n     3\tb" }, ctx());
  expect(readFileSync(path, "utf8")).toBe("a\nins\n\nb\n");
});

test("edit: a loose match that would change nothing is an error, so loop-guard sees no change (#65)", async () => {
  const path = join(dir, "same.txt");
  writeFileSync(path, "a\nb\n");
  const r = await editTool.execute({ path, old_string: "     1\ta", new_string: "a" }, ctx());
  expect(r.isError).toBe(true);
  expect(out(r)).toContain("identical");
  expect(r.details?.changed).toBeUndefined();
});

test("edit: an old_string that starts with a blank line is found in time next to a long run of spaces (#65)", async () => {
  const path = join(dir, "padded.txt");
  writeFileSync(path, `x${" ".repeat(100_000)}y\nfoo();\n  needle();\n`);
  const started = performance.now();
  const r = await editTool.execute({ path, old_string: "\n    needle();", new_string: "\n    pin();" }, ctx());
  expect(performance.now() - started).toBeLessThan(2000);
  expect(r.isError).toBeFalsy();
  expect(readFileSync(path, "utf8")).toEndWith("y\nfoo();\n  pin();\n");
});

test("edit: a long loose edit that adds a depth old_string lacks is mapped in time (#65)", async () => {
  const path = join(dir, "long.ts"), file: string[] = [], old: string[] = [], neu: string[] = [];
  // Dedented by one tab, with a line two tabs deeper after each: the shift comes from old_string's two depths.
  for (let k = 0; k < 300; k++) file.push(`\ta${k}();`, `\t\tb${k}();`), old.push(`a${k}();`, `\tb${k}();`), neu.push(`a${k}();`, `\tb${k}();`, `\t\tc${k}();`);
  writeFileSync(path, `{\n${file.join("\n")}\n}\n`);
  const started = performance.now();
  const r = await editTool.execute({ path, old_string: old.join("\n"), new_string: neu.join("\n") }, ctx());
  expect(performance.now() - started).toBeLessThan(2000);
  expect(out(r)).toContain("indentation]");
  expect(readFileSync(path, "utf8")).toStartWith("{\n\ta0();\n\t\tb0();\n\t\t\tc0();\n\ta1();\n");
});

test("read stops on a whole line within 100,000 characters and says where to go on (#66)", async () => {
  const lines = Array.from({ length: 2000 }, (_, i) => `line ${i + 1} `.padEnd(200, "x"));
  writeFileSync(join(dir, "wide.txt"), `${lines.join("\n")}\n`);
  const first = out(await readTool.execute({ path: "wide.txt" }, ctx()));
  expect(first).toEndWith("\n[Showing lines 1-480 of 2000. Use offset=481 to continue.]");
  const seen: string[] = [];
  for (let offset = 1; offset; ) {
    const r = out(await readTool.execute({ path: "wide.txt", offset }, ctx()));
    const body = r.slice(0, r.indexOf("\n[Showing"));
    expect(body.length).toBeLessThanOrEqual(100_000);
    seen.push(...body.split("\n").map((l) => l.replace(/^ *\d+\t/, "")));
    offset = Number(/Use offset=(\d+)/.exec(r)?.[1] ?? 0);
  }
  expect(seen).toEqual(lines);
  // Long lines are cut first, and only the ones shown are counted.
  writeFileSync(join(dir, "long.txt"), `${"y".repeat(3000)}\n`.repeat(60));
  const long = out(await readTool.execute({ path: "long.txt" }, ctx()));
  expect(long).toContain("[Showing lines 1-49 of 60. Use offset=50 to continue.]\n[49 long line(s) truncated");
});

test("edit refuses a file that is not UTF-8 and leaves its bytes as they were; read says so (#89)", async () => {
  // Shift_JIS for "// 設定\nconst x = 1;\n// 終わり\n"
  const sjis = Buffer.from("2f2f2090dd92e80a636f6e73742078203d20313b0a2f2f208f4982ed82e80a", "hex");
  writeFileSync(join(dir, "sjis.js"), sjis);
  const r = await editTool.execute({ path: "sjis.js", old_string: "const x = 1;", new_string: "const x = 2;" }, ctx());
  expect(r.isError).toBe(true);
  expect(out(r)).toContain("not valid UTF-8");
  expect(readFileSync(join(dir, "sjis.js")).equals(sjis)).toBe(true);
  expect(out(await readTool.execute({ path: "sjis.js" }, ctx()))).toContain("is not valid UTF-8");
  writeFileSync(join(dir, "utf8.js"), "// 設定\nconst x = 1;\n");
  expect(out(await readTool.execute({ path: "utf8.js" }, ctx()))).not.toContain("UTF-8");
  expect((await editTool.execute({ path: "utf8.js", old_string: "const x = 1;", new_string: "const x = 2;" }, ctx())).isError).toBeFalsy();
});

test("read and bash never cut a surrogate pair in half (#88)", async () => {
  writeFileSync(join(dir, "emoji.txt"), `${"x".repeat(1999)}${"😀".repeat(50)}\n`);
  const r = out(await readTool.execute({ path: "emoji.txt" }, ctx()));
  expect(r.isWellFormed()).toBe(true);
  expect(r).toContain("x… [line truncated");
  const b = out(await bashTool.execute({ command: "for i in $(seq 1 20000); do printf '😀'; done; echo; echo o" }, ctx()));
  expect(b.isWellFormed()).toBe(true);
});
