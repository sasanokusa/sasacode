import { afterAll, beforeEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { emptyUsage, registerApi, replayProvider } from "@sasacode/ai";
import { discoverPlugins, isTrusted } from "../src/loader.ts";
import { setup } from "../src/setup.ts";

// A fresh directory per test: Bun caches module resolution per path within a process.
const base = mkdtempSync(join(tmpdir(), "sasacode-cli-plugins-"));
let home = "";
let proj = "";
afterAll(() => rmSync(base, { recursive: true, force: true }));

const PLUGIN = (tool: string) => `
import { text } from "@sasacode/plugin-api";
export default (api) => {
  api.registerTool({
    name: "${tool}", description: "says hi", kind: "read",
    parameters: { type: "object", properties: {} },
    execute: async () => ({ content: [text("hi from ${tool}")] }),
  });
  api.registerCommand({ name: "${tool}-cmd", description: "x", run() {} });
};`;

function write(path: string, body: string) {
  mkdirSync(join(path, ".."), { recursive: true });
  writeFileSync(path, body);
}

beforeEach(() => {
  const root = mkdtempSync(join(base, "t-"));
  home = join(root, "home");
  proj = join(root, "proj");
  process.env.SASACODE_HOME = home;
  write(join(home, "config.json"), JSON.stringify({ providers: { test: { api: "replay" } }, model: "test/m" }));
  write(join(home, "plugins", "global-hi.ts"), PLUGIN("global_hi"));
  write(join(proj, ".sasacode", "plugins", "proj-hi", "plugin.json"), JSON.stringify({ apiVersion: "^1.0.0", extensions: ["main.ts"], skills: "skills" }));
  write(join(proj, ".sasacode", "plugins", "proj-hi", "main.ts"), PLUGIN("proj_hi"));
  write(join(proj, ".sasacode", "plugins", "proj-hi", "skills", "s1", "SKILL.md"), "---\nname: s1\ndescription: plugin skill\n---\nbody");
  write(join(proj, ".sasacode", "plugins", "future", "plugin.json"), JSON.stringify({ apiVersion: "^2.0.0", extensions: ["x.ts"] }));
});

test("discovery finds single files, manifest dirs and scopes", () => {
  const found = discoverPlugins(proj).map((p) => `${p.manifest.name}:${p.scope}`);
  expect(found.sort()).toEqual(["future:project", "global-hi:global", "proj-hi:project"]);
});

test("global plugins load; project plugins wait for trust in headless runs", async () => {
  const h = await setup({ cwd: proj, noSession: true });
  await h.loadPlugins();
  const tools = h.agent.getTools().map((t) => t.name);
  expect(tools).toContain("global_hi");
  expect(tools).not.toContain("proj_hi");
  expect(tools).toEqual(expect.arrayContaining(["read", "write", "edit", "bash", "task", "todo_write", "web_fetch"]));
});

test("trusted project: plugin tools, commands and skills load; incompatible API versions are skipped", async () => {
  const h = await setup({ cwd: proj, noSession: true, trustProject: true, permission: "auto" });
  await h.loadPlugins();
  expect(h.agent.getTools().map((t) => t.name)).toContain("proj_hi");
  expect(h.host.commands.map((c) => c.name)).toEqual(expect.arrayContaining(["proj_hi-cmd", "skill:s1", "compact", "mcp", "presets"]));
  expect(h.host.plugins.find((p) => p.name === "future")?.error).toContain("needs plugin API ^2.0.0");

  registerApi("replay", replayProvider([{ role: "assistant", content: [{ type: "tool_call", id: "1", name: "proj_hi", input: {} }], api: "replay", provider: "test", model: "m", usage: emptyUsage(), stopReason: "tool_use", timestamp: 0 }, { role: "assistant", content: [{ type: "text", text: "ok" }], api: "replay", provider: "test", model: "m", usage: emptyUsage(), stopReason: "stop", timestamp: 0 }]));
  await h.agent.prompt("hi");
  expect(JSON.stringify(h.agent.messages)).toContain("hi from proj_hi");
});

test("interactive trust prompt: accepting saves trust for this fingerprint", async () => {
  const h = await setup({ cwd: proj, noSession: true });
  const asked: string[] = [];
  await h.loadPlugins({ interactive: true, notify() {}, confirm: async (t, m) => (asked.push(`${t} ${m}`), true), select: async () => undefined });
  expect(asked[0]).toContain("plugin proj-hi");
  expect(h.agent.getTools().map((t) => t.name)).toContain("proj_hi");
  const again = await setup({ cwd: proj, noSession: true });
  await again.loadPlugins({ interactive: true, notify() {}, confirm: async () => false, select: async () => undefined });
  expect(again.agent.getTools().map((t) => t.name)).toContain("proj_hi"); // no second prompt needed
  // A new project plugin changes the fingerprint.
  write(join(proj, ".sasacode", "plugins", "new.ts"), PLUGIN("new_one"));
  const third = await setup({ cwd: proj, noSession: true });
  await third.loadPlugins();
  expect(third.agent.getTools().map((t) => t.name)).not.toContain("proj_hi");
});

test("plugins.disabled and tools.disabled switch things off (P3)", async () => {
  write(join(proj, ".sasacode", "config.json"), JSON.stringify({ plugins: { disabled: ["todo", "global-hi"] }, tools: { disabled: ["bash", "web_fetch"] } }));
  const h = await setup({ cwd: proj, noSession: true });
  await h.loadPlugins();
  const tools = h.agent.getTools().map((t) => t.name);
  expect(tools).not.toContain("todo_write");
  expect(tools).not.toContain("global_hi");
  expect(tools).not.toContain("bash");
  expect(tools).not.toContain("web_fetch");
  expect(tools).toContain("read");
});

test("a plugin can replace a built-in tool by name", async () => {
  write(join(home, "plugins", "my-read.ts"), `export default (api) => api.registerTool({ name: "read", description: "custom read", kind: "read", parameters: { type: "object", properties: {} }, execute: async () => ({ content: [] }) });`);
  const h = await setup({ cwd: proj, noSession: true });
  await h.loadPlugins();
  expect(h.agent.getTools().find((t) => t.name === "read")?.description).toBe("custom read");
  expect(isTrusted(proj, "nope")).toBe(false);
});
