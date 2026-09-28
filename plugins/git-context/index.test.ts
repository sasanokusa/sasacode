import { afterAll, expect, test } from "bun:test";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import plugin, { collectContext } from "./index.ts";
import { loadPlugin } from "../test-support.ts";
const root = mkdtempSync(join(tmpdir(), "git-context-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));
function fixture(commit = true) {
  const cwd = mkdtempSync(join(root, "repo-"));
  const git = (...args: string[]) => execFileSync("git", args, { cwd, encoding: "utf8", stdio: "pipe" }).trim();
  git("init", "-b", "main"); git("config", "user.name", "Test"); git("config", "user.email", "test@example.invalid");
  writeFileSync(join(cwd, "file.txt"), "original\n"); git("add", ".");
  if (commit) git("-c", "commit.gpgsign=false", "commit", "-m", "initial");
  return { cwd, git, ctx: { cwd, signal: new AbortController().signal } };
}
const body = (r: Awaited<ReturnType<typeof collectContext>>) => JSON.stringify(r.content);

test("working combines staged and unstaged; staged stays at the index", async () => {
  const { cwd, git, ctx } = fixture();
  writeFileSync(join(cwd, "file.txt"), "staged\n"); git("add", ".");
  writeFileSync(join(cwd, "file.txt"), "unstaged\n");
  writeFileSync(join(cwd, "untracked.txt"), "UNTRACKED_SECRET_CONTENT\n");
  const working = body(await collectContext({}, ctx));
  expect(working).toContain("+unstaged"); expect(working).toContain("untracked.txt");
  expect(working).not.toContain("UNTRACKED_SECRET_CONTENT");
  expect(body(await collectContext({ scope: "staged" }, ctx))).toContain("+staged");
  expect(body(await collectContext({ patch: false }, ctx))).not.toContain("+unstaged");
});

test("branch uses merge-base and excludes uncommitted edits", async () => {
  const { cwd, git, ctx } = fixture();
  git("checkout", "-b", "feature"); writeFileSync(join(cwd, "file.txt"), "feature\n"); git("add", "."); git("-c", "commit.gpgsign=false", "commit", "-m", "feature");
  writeFileSync(join(cwd, "file.txt"), "local-only\n");
  const output = body(await collectContext({ scope: "branch", base: "main" }, ctx));
  expect(output).toContain("+feature"); expect(output).not.toContain("+local-only");
  await expect(collectContext({ scope: "branch", base: "--output=evil" }, ctx)).rejects.toThrow();
  await expect(collectContext({ scope: "branch" }, ctx)).rejects.toThrow();
});

test("works before first commit and limits scope to cwd descendants", async () => {
  const f = fixture(false);
  expect(body(await collectContext({}, f.ctx))).toContain("+original");
  const { cwd, git } = fixture(); mkdirSync(join(cwd, "sub"));
  writeFileSync(join(cwd, "sub", "inside.txt"), "old\n"); git("add", "."); git("-c", "commit.gpgsign=false", "commit", "-m", "sub");
  writeFileSync(join(cwd, "file.txt"), "OUTSIDE_CONTENT\n"); writeFileSync(join(cwd, "sub", "inside.txt"), "INSIDE_CONTENT\n");
  const output = body(await collectContext({}, { cwd: join(cwd, "sub"), signal: new AbortController().signal }));
  expect(output).toContain("INSIDE_CONTENT"); expect(output).not.toContain("OUTSIDE_CONTENT");
});

test("truncates explicitly, disables external diff, and respects cancellation", async () => {
  const { cwd, git, ctx } = fixture();
  git("config", "diff.external", "false");
  writeFileSync(join(cwd, "file.txt"), "long change\n".repeat(1000));
  const result = await collectContext({ maxChars: 1000 }, ctx);
  expect(result.details.truncated).toBe(true);
  expect(result.content[0].type === "text" && result.content[0].text.length).toBeLessThanOrEqual(1000);
  expect(body(result)).toContain("Truncated");
  await expect(collectContext({}, { cwd, signal: AbortSignal.abort() })).rejects.toThrow();
});

test("loads through the host as a read tool with cwd permission scope", async () => {
  const { cwd } = fixture(); const h = await loadPlugin("git-context", plugin, cwd);
  const tool = h.tool("git_context");
  expect((await h.agent.permissions.check({ tool, args: {}, cwd })).decision).toBe("allow");
  expect(body(await tool.execute({}, h.context) as Awaited<ReturnType<typeof collectContext>>)).toContain("Git context");
});
