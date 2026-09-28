// checkpoints: write / edit が変えたファイルの直前の内容を退避し、/undo で戻す。
import { afterAll, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Plugin, ToolDefinition } from "@sasacode/plugin-api";
import { editTool, writeTool } from "@sasacode/tools";
import checkpoints from "../src/checkpoints.ts";

const root = mkdtempSync(join(tmpdir(), "sasacode-checkpoints-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

/** Just enough of the plugin API to drive the plugin: its hooks, its command, its session log. */
function fake(settings: Record<string, unknown> = {}, entries: Record<string, unknown[]> = {}) {
  const hooks: Record<string, ((e: any) => any)[]> = {};
  const notices: string[] = [];
  const commands: Record<string, any> = {};
  const api: any = {
    cwd: root,
    settings,
    session: {
      id: "s1",
      append: (k: string, d: unknown) => (entries[k] ??= []).push(d),
      entries: (k: string) => entries[k] ?? [],
      inject: () => {},
    },
    ui: { interactive: true, notify: (m: string) => notices.push(m), setStatus: () => {} },
    registerCommand: (c: any) => (commands[c.name] = c),
    on: (n: string, h: any) => (hooks[n] ??= []).push(h),
    log() {},
  };
  (checkpoints as Plugin)(api);
  const fire = async (n: string, e: any) => {
    for (const h of hooks[n] ?? []) await h(e);
  };
  /** One call as the harness runs it: tool_call, then the tool, then tool_result. */
  let n = 0;
  const call = async (tool: ToolDefinition<any>, args: Record<string, unknown>, denied = false) => {
    const c = { id: `c${++n}`, name: tool.name, input: args };
    await fire("tool_call", { call: c, tool, args });
    if (denied) return void (await fire("tool_result", { call: c, result: { content: [], isError: true }, ran: false }));
    const result = await tool.execute(args, { cwd: root, signal: new AbortController().signal });
    await fire("tool_result", { call: c, result, ran: true });
    return result;
  };
  return { fire, call, notices, commands, entries };
}

test("write over a file: /undo puts the old content back", async () => {
  const f = join(root, "a.txt");
  writeFileSync(f, "before");
  const g = fake();
  await g.call(writeTool, { path: "a.txt", content: "after" });
  expect(readFileSync(f, "utf8")).toBe("after");
  await g.commands.undo.run({});
  expect(readFileSync(f, "utf8")).toBe("before");
  expect(g.notices.join()).toContain("戻しました");
});

test("write that creates a file: /undo deletes it again", async () => {
  const f = join(root, "new.txt");
  const g = fake();
  await g.call(writeTool, { path: "new.txt", content: "x" });
  expect(existsSync(f)).toBe(true);
  await g.commands.undo.run({});
  expect(existsSync(f)).toBe(false);
  expect(g.notices.join()).toContain("削除しました");
});

test("edit: /undo by path restores the edited file", async () => {
  const f = join(root, "e.txt");
  writeFileSync(f, "a b c");
  const g = fake();
  await g.call(editTool, { path: "e.txt", old_string: "b", new_string: "X" });
  expect(readFileSync(f, "utf8")).toBe("a X c");
  await g.commands.undo.run({ args: "e.txt" });
  expect(readFileSync(f, "utf8")).toBe("a b c");
});

test("a denied, failing or no-op call leaves no checkpoint", async () => {
  const f = join(root, "n.txt");
  writeFileSync(f, "same");
  const g = fake();
  await g.call(writeTool, { path: "n.txt", content: "y" }, true); // denied: never ran
  await g.call(editTool, { path: "n.txt", old_string: "nope", new_string: "z" }); // old_string not found
  await g.call(writeTool, { path: "n.txt", content: "same" }); // writes the same content back
  expect(readFileSync(f, "utf8")).toBe("same");
  await g.commands.undo.run({ args: "list" });
  expect(g.notices.join()).toContain("戻せる変更はありません");
});

test("two changes to one file undo one at a time", async () => {
  const f = join(root, "m.txt");
  writeFileSync(f, "1");
  const g = fake();
  await g.call(writeTool, { path: "m.txt", content: "2" });
  await g.call(writeTool, { path: "m.txt", content: "3" });
  await g.commands.undo.run({});
  expect(readFileSync(f, "utf8")).toBe("2");
  await g.commands.undo.run({});
  expect(readFileSync(f, "utf8")).toBe("1");
  await g.commands.undo.run({});
  expect(g.notices.join()).toContain("戻せる変更はありません");
});

test("checkpoints survive a resume: they live in the session log", async () => {
  const f = join(root, "r.txt");
  writeFileSync(f, "r1");
  const g = fake();
  await g.call(writeTool, { path: "r.txt", content: "r2" });
  const resumed = fake({}, g.entries);
  await resumed.commands.undo.run({});
  expect(readFileSync(f, "utf8")).toBe("r1");
});

test("a file over the limit is not kept: /undo says so instead of guessing", async () => {
  const f = join(root, "big.txt");
  writeFileSync(f, "old");
  const g = fake({ maxFileKB: 0 });
  await g.call(writeTool, { path: "big.txt", content: "new" });
  expect(readFileSync(f, "utf8")).toBe("new");
  await g.commands.undo.run({});
  expect(g.notices.join()).toContain("戻せません");
  expect(readFileSync(f, "utf8")).toBe("new");
});
