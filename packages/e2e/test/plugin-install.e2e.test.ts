// `sasacode plugin install` with the real CLI: a package that is not a sasacode plugin is refused
// before anything is installed, however it is named (npm, a folder, a git URL). Typing a plugin's
// short name (`jev-guard`) used to install an unrelated npm package of that name.
import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { sandbox, type Sandbox } from "./harness.ts";

let registry: ReturnType<typeof Bun.serve>;
let box: Sandbox;
const env = () => ({ SASACODE_NPM_REGISTRY: `http://127.0.0.1:${registry.port}` });
const doc = (manifest: Record<string, unknown>) => ({ "dist-tags": { latest: "1.0.0" }, versions: { "1.0.0": { description: "x", ...manifest } } });

beforeAll(() => {
  const docs: Record<string, unknown> = {
    "/jev-guard": doc({}), // someone else's package with the plugin's short name
    "/sasacode-plugin-jev-guard": doc({ sasacode: { name: "jev-guard", extensions: ["index.ts"] } }),
    "/left-pad": doc({}),
  };
  registry = Bun.serve({ port: 0, fetch: (req) => (docs[new URL(req.url).pathname] ? Response.json(docs[new URL(req.url).pathname]) : new Response("{}", { status: 404 })) });
  box = sandbox({});
});
afterAll(() => {
  registry.stop(true);
  box.cleanup();
});

const plugins = () => join(box.home, "plugins");

test("an npm package without a sasacode field is refused, and the plugin of that short name is suggested", async () => {
  const r = await box.run(["plugin", "install", "jev-guard", "--yes"], { env: env() });
  expect(r.code).toBe(1);
  expect(r.stderr).toContain("jev-guard is not a sasacode plugin");
  expect(r.stderr).toContain("sasacode plugin install sasacode-plugin-jev-guard");
  expect(existsSync(join(plugins(), "package.json"))).toBe(false); // nothing was added

  const other = await box.run(["plugin", "install", "left-pad", "--yes"], { env: env() });
  expect(other.code).toBe(1);
  expect(other.stderr).toContain("left-pad is not a sasacode plugin");
  expect(other.stderr).not.toContain("Did you mean"); // no sasacode-plugin-left-pad exists
});

test("a local folder that is not a plugin is refused before it is packed", async () => {
  const dir = join(box.project, "not-a-plugin");
  mkdirSync(dir);
  writeFileSync(join(dir, "package.json"), JSON.stringify({ name: "not-a-plugin", version: "1.0.0" }));
  const r = await box.run(["plugin", "install", "./not-a-plugin", "--yes"]);
  expect(r.code).toBe(1);
  expect(r.stderr).toContain("not-a-plugin is not a sasacode plugin");
  expect(existsSync(join(plugins(), "local-packages"))).toBe(false);
});

test("a git repository that is not a plugin is refused and its clone removed", async () => {
  const repo = join(box.project, "some-tool");
  mkdirSync(repo);
  writeFileSync(join(repo, "README.md"), "a tool\n");
  const git = (...args: string[]) => Bun.spawnSync(["git", "-c", "user.name=t", "-c", "user.email=t@t", ...args], { cwd: repo });
  git("init", "-q");
  git("add", ".");
  git("commit", "-qm", "x");
  const r = await box.run(["plugin", "install", `git+file://${repo}`, "--yes"]);
  expect(r.code).toBe(1);
  expect(r.stderr).toContain("not a sasacode plugin");
  expect(existsSync(join(plugins(), "some-tool"))).toBe(false);
});

test("plugin list points out a non-plugin package installed before install refused them", async () => {
  mkdirSync(join(plugins(), "node_modules", "git-context"), { recursive: true });
  writeFileSync(join(plugins(), "package.json"), JSON.stringify({ private: true, dependencies: { "git-context": "^1.1.0" } }));
  writeFileSync(join(plugins(), "node_modules", "git-context", "package.json"), JSON.stringify({ name: "git-context", version: "1.1.0" }));
  const r = await box.run(["plugin", "list"]);
  expect(r.code).toBe(0);
  expect(r.stderr).toContain("git-context  [global]  not a sasacode plugin, never loaded: sasacode plugin remove git-context");
});
