import { afterAll, beforeAll, beforeEach, expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setLang } from "@sasacode/host";
import { emptyUsage, registerApi } from "@sasacode/ai";
import { replayProvider } from "@sasacode/testing";
import { discoverPlugins, pluginCommand } from "../src/loader.ts";
import { assessProject, isTrusted, saveTrust } from "../src/trust.ts";
import { setup } from "../src/setup.ts";

beforeAll(() => setLang("en")); // these assertions quote the English strings

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

test("one trust decision covers project plugins and elevated config; it is remembered until they change", async () => {
  write(join(proj, ".sasacode", "config.json"), JSON.stringify({ mcpServers: { fx: { command: "true" } } }));
  const t = assessProject(proj);
  expect(t.items).toEqual(expect.arrayContaining(["plugin proj-hi", "plugin future", "MCP server fx"]));
  expect(isTrusted(proj, t)).toBe(false);
  saveTrust(proj, t);
  expect(isTrusted(proj, assessProject(proj))).toBe(true);
  // A new project plugin changes the fingerprint: asked again.
  write(join(proj, ".sasacode", "plugins", "new.ts"), PLUGIN("new_one"));
  expect(isTrusted(proj, assessProject(proj))).toBe(false);
  saveTrust(proj, assessProject(proj));
  expect(isTrusted(proj, assessProject(proj))).toBe(true);
  // So does a change to a trusted plugin's code alone, same name and manifest.
  write(join(proj, ".sasacode", "plugins", "new.ts"), PLUGIN("new_one").replace("new_one", "new_one_changed"));
  expect(isTrusted(proj, assessProject(proj))).toBe(false);
  // The legacy trustedProjects list in the global config still counts.
  write(join(home, "config.json"), JSON.stringify({ trustedProjects: [proj] }));
  expect(isTrusted(proj, assessProject(proj))).toBe(true);
  // A project with nothing risky needs no trust.
  const plain = mkdtempSync(join(tmpdir(), "plain-"));
  expect(assessProject(plain).items).toEqual([]);
});

test("trust follows a plugin folder that is a link, and never vouches for code it could not read", () => {
  const elsewhere = join(proj, "..", "linked-plugin");
  write(join(elsewhere, "plugin.json"), JSON.stringify({ apiVersion: "^1.0.0", extensions: ["main.ts"] }));
  write(join(elsewhere, "main.ts"), PLUGIN("linked"));
  symlinkSync(elsewhere, join(proj, ".sasacode", "plugins", "linked"), "junction"); // junction: no admin needed on Windows
  saveTrust(proj, assessProject(proj));
  expect(isTrusted(proj, assessProject(proj))).toBe(true);
  write(join(elsewhere, "main.ts"), PLUGIN("linked_changed")); // same name, same manifest
  expect(isTrusted(proj, assessProject(proj))).toBe(false);
  // Too many files to read: asked every time, even right after saying yes.
  for (let i = 0; i < 5001; i++) write(join(proj, ".sasacode", "plugins", "aaa-bulk", `f${i}.txt`), "x");
  const t = assessProject(proj);
  expect(t.complete).toBe(false);
  saveTrust(proj, t);
  expect(isTrusted(proj, assessProject(proj))).toBe(false);
}, 30_000); // 5001 files: slow to write on Windows (and under a virus scanner)

test("plugins.disabled and tools.disabled switch things off (P3)", async () => {
  write(join(proj, ".sasacode", "config.json"), JSON.stringify({ plugins: { disabled: ["todo", "global-hi"] }, tools: { disabled: ["bash", "web_fetch"] } }));
  const h = await setup({ cwd: proj, noSession: true, trustProject: true }); // disabling plugins is a trusted setting
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
});

test("a user plugin named like a bundled one replaces it instead of running beside it", async () => {
  write(join(home, "plugins", "todo.ts"), PLUGIN("my_todo"));
  const h = await setup({ cwd: proj, noSession: true });
  await h.loadPlugins();
  const tools = h.agent.getTools().map((t) => t.name);
  expect(tools).toContain("my_todo");
  expect(tools).not.toContain("todo_write");
  expect(h.host.plugins.filter((p) => p.name === "todo")).toHaveLength(1);
});

test("a config that turns jev-guard on, now that it is not bundled, is told how to get it back", async () => {
  write(join(home, "config.json"), JSON.stringify({ providers: { test: { api: "replay" } }, model: "test/m", plugins: { settings: { "jev-guard": { enabled: true } } } }));
  const notices: string[] = [];
  const h = await setup({ cwd: proj, noSession: true });
  await h.loadPlugins({ interactive: false, notify: (m) => notices.push(m), confirm: async () => false, select: async () => undefined });
  expect(notices.some((n) => n.includes("sasacode plugin install sasacode-plugin-jev-guard"))).toBe(true);

  write(join(home, "plugins", "jev-guard.ts"), PLUGIN("jev_stub")); // installed: nothing to say
  const quiet: string[] = [];
  const h2 = await setup({ cwd: proj, noSession: true });
  await h2.loadPlugins({ interactive: false, notify: (m) => quiet.push(m), confirm: async () => false, select: async () => undefined });
  expect(quiet.some((n) => n.includes("sasacode-plugin-jev-guard"))).toBe(false);
});

test("plugin remove takes a plugin's name, never a path out of the plugins folder", async () => {
  const outside = join(home, "..", "keep-me");
  mkdirSync(outside, { recursive: true });
  expect(await pluginCommand(["remove", "../../keep-me"], proj)).toBe(1);
  expect(await pluginCommand(["remove", ".."], proj)).toBe(1);
  expect(existsSync(outside)).toBe(true);
  expect(existsSync(join(home, "plugins"))).toBe(true);
  mkdirSync(join(home, "plugins", "gone"));
  expect(await pluginCommand(["remove", "gone"], proj)).toBe(1); // no terminal to ask on
  expect(existsSync(join(home, "plugins", "gone"))).toBe(true);
  expect(await pluginCommand(["remove", "--yes", "gone"], proj)).toBe(0);
  expect(existsSync(join(home, "plugins", "gone"))).toBe(false);
});

test("plugin remove finds the plugin by the name list shows: a single file, a folder named otherwise; nothing found is an error (#107)", async () => {
  const plugins = join(home, "plugins");
  write(join(plugins, "repo-dir", "package.json"), JSON.stringify({ name: "sasacode-plugin-demo", version: "1.0.0", sasacode: { extensions: ["index.ts"] } }));
  write(join(plugins, "repo-dir", "index.ts"), PLUGIN("demo"));
  const names = () => discoverPlugins(proj).filter((p) => p.scope === "global").map((p) => p.manifest.name);
  expect(names().sort()).toEqual(["global-hi", "sasacode-plugin-demo"]);

  expect(await pluginCommand(["remove", "--yes", "global-hi"], proj)).toBe(0);
  expect(existsSync(join(plugins, "global-hi.ts"))).toBe(false);
  expect(existsSync(plugins)).toBe(true); // the file, never the folder it is listed with
  expect(await pluginCommand(["remove", "--yes", "sasacode-plugin-demo"], proj)).toBe(0);
  expect(existsSync(join(plugins, "repo-dir"))).toBe(false);
  expect(names()).toEqual([]);

  expect(await pluginCommand(["remove", "--yes", "nothing-here"], proj)).toBe(1);
  expect(await pluginCommand(["remove", "--yes", "proj-hi"], proj)).toBe(1); // a project plugin, without --project
  expect(existsSync(join(proj, ".sasacode", "plugins", "proj-hi"))).toBe(true);
});

test("trust covers what the loader imports: no dependency outside node_modules, no extension under node_modules (#104)", async () => {
  const plugins = join(proj, ".sasacode", "plugins");
  // A: a dependency "name" that leaves node_modules.
  write(join(proj, "..", "tools", "plug", "plugin.json"), JSON.stringify({ apiVersion: "^1.0.0", extensions: ["main.ts"] }));
  write(join(proj, "..", "tools", "plug", "main.ts"), PLUGIN("dotdot_v1"));
  write(join(plugins, "package.json"), JSON.stringify({ dependencies: { "../../../tools/plug": "*" } }));
  const warnings: string[] = [];
  expect(discoverPlugins(proj, warnings).map((p) => p.manifest.name)).not.toContain("plug");
  expect(warnings.join()).toContain("is not a package name");
  // B: an extension under the plugin's node_modules: changing it is asked about, and the loader refuses it.
  write(join(plugins, "nm", "plugin.json"), JSON.stringify({ apiVersion: "^1.0.0", extensions: ["node_modules/impl/index.ts"] }));
  write(join(plugins, "nm", "node_modules", "impl", "index.ts"), PLUGIN("nm_v1"));
  saveTrust(proj, assessProject(proj));
  expect(isTrusted(proj, assessProject(proj))).toBe(true);
  write(join(plugins, "nm", "node_modules", "impl", "index.ts"), PLUGIN("nm_v2"));
  expect(isTrusted(proj, assessProject(proj))).toBe(false);
  const h = await setup({ cwd: proj, noSession: true, trustProject: true });
  await h.loadPlugins();
  expect(h.agent.getTools().map((t) => t.name)).not.toContain("nm_v2");
  expect(h.host.plugins.find((p) => p.name === "nm")?.error).toContain("node_modules or .git");
});
