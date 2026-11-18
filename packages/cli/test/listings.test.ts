// What the GUI asks before a run: models, commands, skills, plugins and the folder's trust.
import { afterAll, beforeAll, beforeEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { setLang } from "@sasacode/host";
import { commandListing, modelsOf, pluginListing, skillListing, TUI_COMMANDS, trustCommand, trustStatus } from "../src/listings.ts";

beforeAll(() => setLang("en"));

const root = mkdtempSync(join(tmpdir(), "sasacode-listings-"));
const home = join(root, "home");
afterAll(() => rmSync(root, { recursive: true, force: true }));
let n = 0;
let proj = "";

const write = (path: string, body: string) => {
  mkdirSync(resolve(path, ".."), { recursive: true });
  writeFileSync(path, body);
};
const skill = (dir: string, name: string, description: string) => write(join(dir, name, "SKILL.md"), `---\nname: ${name}\ndescription: ${description}\n---\nDo it.\n`);

beforeEach(() => {
  rmSync(home, { recursive: true, force: true });
  process.env.SASACODE_HOME = home;
  proj = join(root, `proj${++n}`);
  mkdirSync(proj, { recursive: true });
  write(join(home, "config.json"), "{}");
});

test("the TUI's commands listed are the TUI's own, in the same order", () => {
  const app = readFileSync(resolve(import.meta.dir, "../../tui/src/app.ts"), "utf8");
  const body = app.slice(app.indexOf("private coreCommands()"), app.indexOf("private async runCommand("));
  expect(TUI_COMMANDS.map((c) => c.name)).toEqual([...body.matchAll(/^\s+(?:\{ )?name: "([a-z]+)"/gm)].map((m) => m[1]));
  for (const c of TUI_COMMANDS) expect(body).toContain(`t("${c.description}")`);
});

test("models: the current one first, then the config's, then what the providers list, without repeats", async () => {
  const h = {
    agent: { model: { provider: "local", id: "a" } },
    config: { models: ["local/b", "other/c"] },
    listModels: async () => [
      { spec: "local/a", contextWindow: 32_000 },
      { spec: "local/d", maxContext: 128_000, server: "ollama" as const },
    ],
  };
  expect(await modelsOf(h as any)).toEqual([
    { id: "local/a", contextWindow: 32_000, current: true },
    { id: "local/b" },
    { id: "other/c" },
    { id: "local/d", maxContext: 128_000, server: "ollama" },
  ]);
  // A provider that cannot be asked leaves the rest.
  const down = { ...h, listModels: async () => Promise.reject(new Error("offline")) };
  const orig = console.error;
  console.error = () => {};
  try {
    expect((await modelsOf(down as any)).map((m) => m.id)).toEqual(["local/a", "local/b", "other/c"]);
  } finally {
    console.error = orig;
  }
});

test("skills and commands: the built-in skill, the user's and the project's (trusted or not); the TUI's commands do not run headless", () => {
  skill(join(home, "skills"), "mine", "the user's skill");
  skill(join(proj, ".sasacode", "skills"), "ours", "the project's skill");
  const skills = skillListing(proj);
  expect(skills.map((s) => s.name).sort()).toEqual(["mine", "ours", "tool-authoring"]);
  expect(skills.find((s) => s.name === "mine")).toEqual({ name: "mine", description: "the user's skill", path: join(home, "skills", "mine", "SKILL.md") });

  const commands = commandListing(proj);
  expect(commands.find((c) => c.name === "help")).toEqual({ name: "help", description: "show commands and keys", owner: "core-commands", headless: false });
  expect(commands.find((c) => c.name === "model")?.argumentHint).toBe("<provider/model> | --refresh");
  expect(commands.find((c) => c.name === "skill:mine")).toEqual({ name: "skill:mine", description: "the user's skill", argumentHint: "[task]", owner: "skills", headless: true });
  expect(commands.find((c) => c.name === "skills")).toEqual({ name: "skills", description: "list the available skills", owner: "skills", headless: true });

  // Turned off: no skills, and no skill commands.
  write(join(home, "config.json"), JSON.stringify({ plugins: { disabled: ["skills"] } }));
  expect(skillListing(proj)).toEqual([]);
  expect(commandListing(proj).every((c) => c.owner === "core-commands")).toBe(true);
});

test("plugins: bundled ones and installed ones, what is turned off, what cannot load, and what is no plugin", () => {
  write(join(home, "config.json"), JSON.stringify({ plugins: { disabled: ["todo", "off"] } }));
  const plugins = join(home, "plugins");
  write(join(plugins, "good", "plugin.json"), JSON.stringify({ version: "1.2.0", extensions: ["index.ts"] }));
  write(join(plugins, "good", "index.ts"), "export default () => {};\n");
  write(join(plugins, "newer", "plugin.json"), JSON.stringify({ apiVersion: "^99.0.0", extensions: ["index.ts"] }));
  write(join(plugins, "newer", "index.ts"), "export default () => {};\n");
  write(join(plugins, "gone", "plugin.json"), JSON.stringify({ extensions: ["missing.ts"] }));
  write(join(plugins, "off.ts"), "export default () => {};\n");
  write(join(plugins, "package.json"), JSON.stringify({ dependencies: { "not-a-plugin": "1.0.0" } }));
  write(join(plugins, "node_modules", "not-a-plugin", "package.json"), JSON.stringify({ name: "not-a-plugin" }));
  write(join(proj, ".sasacode", "plugins", "local.ts"), "export default () => {};\n");

  const listed = pluginListing(proj);
  const byName = (name: string) => listed.find((p) => p.name === name);
  expect(byName("goal")).toEqual({ name: "goal", source: "bundled", enabled: true });
  expect(byName("todo")).toEqual({ name: "todo", source: "bundled", enabled: false });
  expect(byName("mcp")).toEqual({ name: "mcp", source: "bundled", enabled: true });
  expect(byName("good")).toEqual({ name: "good", version: "1.2.0", source: "global", dir: join(plugins, "good"), enabled: true });
  expect(byName("newer")?.error).toContain("needs plugin API ^99.0.0");
  expect(byName("gone")?.error).toContain("missing.ts is not a file inside");
  expect(byName("off")).toEqual({ name: "off", source: "global", dir: plugins, enabled: false });
  expect(byName("not-a-plugin")).toEqual({ name: "not-a-plugin", source: "global", dir: join(plugins, "node_modules", "not-a-plugin"), enabled: false, error: "not a sasacode plugin" });
  // The project is not trusted: its plugin is found, not loaded.
  expect(byName("local")).toEqual({ name: "local", source: "project", dir: join(proj, ".sasacode", "plugins"), enabled: false });
});

test("trust: what the project would add, and --yes trusts it as answering y does", () => {
  write(join(proj, ".sasacode", "config.json"), JSON.stringify({
    mcpServers: { files: { command: "files-mcp" } },
    providers: { lan: { baseUrl: "http://192.168.1.2:8080/v1" } },
    permissions: { allow: ["bash(make *)"], mode: "auto" },
  }));
  write(join(proj, ".sasacode", "plugins", "local.ts"), "export default () => {};\n");
  const before = trustStatus(proj);
  expect(before).toEqual({
    cwd: proj,
    trusted: false,
    needed: true,
    complete: true,
    items: expect.arrayContaining(["plugin local", "MCP server files"]),
    plugins: ["local"],
    mcpServers: ["files"],
    endpoints: [{ name: "lan", url: "http://192.168.1.2:8080/v1" }],
    allow: ["bash(make *)"],
    mode: "auto",
  });
  const out: string[] = [];
  const orig = console.log;
  console.log = (s: string) => void out.push(s);
  try {
    expect(trustCommand(["--yes", "--json"], proj)).toBe(0);
  } finally {
    console.log = orig;
  }
  expect(JSON.parse(out[0]!).trusted).toBe(true);
  expect(trustStatus(proj).trusted).toBe(true);
  // A plugin changed: asked again.
  write(join(proj, ".sasacode", "plugins", "local.ts"), "export default () => { /* new */ };\n");
  expect(trustStatus(proj).trusted).toBe(false);

  const plain = join(root, "plain");
  mkdirSync(plain, { recursive: true });
  expect(trustStatus(plain)).toMatchObject({ trusted: true, needed: false, items: [] });
});

// ── through the command line ──

const MAIN = resolve(import.meta.dir, "../src/main.ts");
const NO_BUNFIG = resolve(import.meta.dir, "../../../scripts/no-bunfig.toml");

async function cli(...args: string[]) {
  const p = Bun.spawn(["bun", "--no-env-file", `--config=${NO_BUNFIG}`, MAIN, ...args], {
    cwd: proj,
    env: { ...process.env, SASACODE_HOME: home, SASACODE_LANG: "en" },
    stdout: "pipe",
    stderr: "pipe",
  });
  const [stdout, stderr, code] = await Promise.all([new Response(p.stdout).text(), new Response(p.stderr).text(), p.exited]);
  return { stdout, stderr, code };
}

test("sasacode list … --json, plugin list --json and trust --json write one JSON value; a wrong word is the usage", async () => {
  skill(join(home, "skills"), "mine", "the user's skill");
  const skills = await cli("list", "skills", "--json");
  expect(skills.code).toBe(0);
  expect(JSON.parse(skills.stdout).map((s: { name: string }) => s.name)).toContain("mine");
  const commands = await cli("list", "commands", "--json");
  expect(JSON.parse(commands.stdout).find((c: { name: string }) => c.name === "skill:mine").headless).toBe(true);
  const plugins = await cli("plugin", "list", "--json");
  expect(JSON.parse(plugins.stdout).find((p: { name: string }) => p.name === "agent-judge")).toEqual({ name: "agent-judge", source: "bundled", enabled: true });
  const trust = await cli("trust", "--json");
  expect(JSON.parse(trust.stdout)).toMatchObject({ cwd: expect.any(String), trusted: true, needed: false });
  const wrong = await cli("list", "everything");
  expect(wrong.code).toBe(1);
  expect(wrong.stderr).toContain("usage: sasacode list models|commands|skills [--json]");
});
