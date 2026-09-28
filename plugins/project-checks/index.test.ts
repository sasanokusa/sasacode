import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, writeFileSync, readFileSync, rmSync, existsSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { PermissionPolicy } from "../../packages/agent/src/index.ts";
import { hideFromChildren } from "@sasacode/plugin-api";
import plugin, { loadChecks } from "./index.ts";
import { bashTool } from "../../packages/tools/src/bash.ts";
import { loadPlugin } from "../test-support.ts";
const root = mkdtempSync(join(tmpdir(), "project-checks-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));
const context = (signal = new AbortController().signal) => ({ cwd: root, signal });

test("detects script names and package manager; rejects ambiguous lockfiles", () => {
  const cwd = mkdtempSync(join(root, "auto-"));
  writeFileSync(join(cwd, "package.json"), JSON.stringify({ scripts: { test: "bun test", lint: "lint", deploy: "publish" } }));
  writeFileSync(join(cwd, "bun.lock"), "");
  expect([...loadChecks(cwd, {})]).toEqual([["test", { command: "bun run test", timeout: 120 }], ["lint", { command: "bun run lint", timeout: 120 }]]);
  writeFileSync(join(cwd, "pnpm-lock.yaml"), "");
  expect(() => loadChecks(cwd, {})).toThrow("multiple");
  expect(loadChecks(cwd, { checks: {} }).size).toBe(0);
});

test("validates explicit commands and timeouts", () => {
  expect(loadChecks(root, { checks: { types: "bun run typecheck" } }).get("types")?.timeout).toBe(120);
  for (const checks of [{ bad: { command: "true", timeout: -1 } }, { bad: "" }, { "bad name": "true" }, []]) {
    expect(() => loadChecks(root, { checks })).toThrow();
  }
});

test("host uses real shell command for bash permission rules", async () => {
  const h = await loadPlugin("project-checks", plugin, root, { checks: { test: "printf tested" } });
  const tool = h.tool("project_check");
  const policy = new PermissionPolicy("auto", { deny: ["bash(printf *)"] });
  expect((await policy.check({ tool, args: { name: "test" }, cwd: root })).decision).toBe("deny");
  expect((await h.agent.permissions.check({ tool, args: { name: "test" }, cwd: root })).decision).toBe("ask");
  expect(tool.matchTarget?.({ name: "test" })).toBe("printf tested");
  await h.host.commands.find((c) => c.name === "checks")!.run({ args: "" });
  expect(h.notices.join()).toContain("printf tested");
  h.agent.setTool(bashTool);
  h.agent.permissions.mode = "auto";
  const result = await h.invoke("project_check", { name: "test" });
  expect(result.isError).toBe(false); expect(JSON.stringify(result.content)).toContain("tested");
});

test("nonzero and zero exits report accurate results", async () => {
  const failed = await bashTool.execute({ command: "printf failure >&2; exit 7", timeout: 2 }, context());
  expect(failed.isError).toBe(true); expect(failed.details?.exitCode).toBe(7);
  expect(JSON.stringify(failed.content)).toContain("failure");
  expect((await bashTool.execute({ command: "true", timeout: 2 }, context())).isError).toBe(false);
});

test("cancellation before execution does not spawn and in-flight cancellation fails", async () => {
  const marker = join(root, "should-not-exist");
  expect((await bashTool.execute({ command: `touch '${marker}'`, timeout: 2 }, context(AbortSignal.abort()))).isError).toBe(true);
  expect(existsSync(marker)).toBe(false);
  const controller = new AbortController();
  const pending = bashTool.execute({ command: "sleep 10", timeout: 5 }, context(controller.signal));
  setTimeout(() => controller.abort(), 30);
  const result = await pending;
  expect(result.isError).toBe(true); expect(result.details?.killedBy).toBe("abort");
});

test("timeout kills descendants even when the shell catches TERM and exits zero", async () => {
  const marker = join(root, "descendant-survived");
  const result = await bashTool.execute({ command: `trap 'exit 0' TERM; (trap '' TERM; sleep 1; touch '${marker}') & wait`, timeout: 0.05 }, context());
  expect(result.isError).toBe(true); expect(result.details?.killedBy).toBe("timeout");
  await Bun.sleep(1100);
  expect(existsSync(marker)).toBe(false);
});

test("long output is bounded and saved; host-only credentials are removed", async () => {
  const previous = process.env.SASACODE_HOME;
  process.env.SASACODE_HOME = join(root, "home");
  const key = "SASACODE_CHECKS_TEST_HIDDEN";
  process.env[key] = "private-test-value"; hideFromChildren([key]);
  try {
    const hidden = await bashTool.execute({ command: `printf '%s' "${"$"}${key}"`, timeout: 2 }, context());
    expect(JSON.stringify(hidden.content)).not.toContain("private-test-value");
    const result = await bashTool.execute({ command: "printf '%040000d' 0", timeout: 2 }, context());
    expect(JSON.stringify(result.content)).toContain("truncated");
    expect(JSON.stringify(result.content).length).toBeLessThan(31000);
    expect(readFileSync(/saved to (\S+)\]/.exec(result.content[0].type === "text" ? result.content[0].text : "")![1], "utf8").length).toBe(40000);
  } finally {
    if (previous === undefined) delete process.env.SASACODE_HOME; else process.env.SASACODE_HOME = previous;
    delete process.env[key];
  }
});
