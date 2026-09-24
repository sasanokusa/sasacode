import { expect, test } from "bun:test";
import { bashTool, editTool, readTool } from "@sasacode/tools";
import { PermissionPolicy } from "../src/permission.ts";

const cwd = "/work/proj";
const bash = (command: string) => ({ tool: bashTool, args: { command }, cwd });

test("default edits mode: in-cwd read/edit allowed, bash and outside paths ask", async () => {
  const p = new PermissionPolicy();
  expect((await p.check({ tool: readTool, args: { path: "src/a.ts" }, cwd })).decision).toBe("allow");
  expect((await p.check({ tool: editTool, args: { path: "/work/proj/x" }, cwd })).decision).toBe("allow");
  expect((await p.check({ tool: editTool, args: { path: "../other/x" }, cwd })).decision).toBe("ask");
  expect((await p.check({ tool: readTool, args: { path: "/etc/passwd" }, cwd })).decision).toBe("ask");
  expect((await p.check(bash("ls"))).decision).toBe("ask");
});

test("modes auto and ask", async () => {
  expect((await new PermissionPolicy("auto").check(bash("rm -rf x"))).decision).toBe("allow");
  expect((await new PermissionPolicy("ask").check({ tool: readTool, args: { path: "a" }, cwd })).decision).toBe("ask");
});

test("allow rules must cover every chained command; substitutions and redirects never match", async () => {
  const p = new PermissionPolicy("edits", { allow: ["bash(git status*)", "bash(npm test)"] });
  expect((await p.check(bash("git status --short"))).decision).toBe("allow");
  expect((await p.check(bash("git status && npm test"))).decision).toBe("allow");
  expect((await p.check(bash("git status && rm -rf ~"))).decision).toBe("ask");
  expect((await p.check(bash("git status $(rm -rf ~)"))).decision).toBe("ask");
  expect((await p.check(bash("git status > ~/.bashrc"))).decision).toBe("ask");
});

test("deny beats allow and auto; path globs; tool wildcards", async () => {
  const p = new PermissionPolicy("auto", { deny: ["bash(rm *)", "edit(secrets/**)"], allow: ["bash"] });
  expect((await p.check(bash("ls && rm -rf /"))).decision).toBe("deny");
  expect((await p.check({ tool: editTool, args: { path: "secrets/k.txt" }, cwd })).decision).toBe("deny");
  const w = new PermissionPolicy("ask", { allow: ["re*"] });
  expect((await w.check({ tool: readTool, args: { path: "a" }, cwd })).decision).toBe("allow");
});

test("agent mode uses the judge for non-read calls", async () => {
  const p = new PermissionPolicy("agent");
  const safe = async () => ({ safe: true, reason: "ls is read-only" });
  const risky = async () => ({ safe: false, reason: "deletes" });
  expect((await p.check(bash("ls"), safe)).decision).toBe("allow");
  expect((await p.check(bash("rm -rf x"), risky)).decision).toBe("ask");
  expect((await p.check({ tool: readTool, args: { path: "a" }, cwd }, risky)).decision).toBe("allow");
});
