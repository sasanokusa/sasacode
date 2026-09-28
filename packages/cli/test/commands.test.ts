// Subcommands and input handling: flags in any position, piped input that arrives late.
import { afterAll, beforeAll, beforeEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setLang } from "@sasacode/host";
import { hideFromChildren, childEnv } from "@sasacode/plugin-api";
import { endpointCommand } from "../src/endpoints.ts";
import { loadEnvFile } from "../src/env.ts";
import { discoverPlugins, pluginCommand } from "../src/loader.ts";
import { readPipedStdin } from "../src/stdin.ts";

beforeAll(() => setLang("en")); // these assertions quote the English strings

const base = mkdtempSync(join(tmpdir(), "sasacode-commands-"));
afterAll(() => rmSync(base, { recursive: true, force: true }));
let home = "";
let proj = "";
beforeEach(() => {
  const root = mkdtempSync(join(base, "t-"));
  home = join(root, "home");
  proj = join(root, "proj");
  mkdirSync(home, { recursive: true });
  mkdirSync(join(proj, ".sasacode", "plugins"), { recursive: true });
  process.env.SASACODE_HOME = home;
});

const quiet = async <T>(fn: () => Promise<T>) => {
  const [o, e] = [console.log, console.error];
  const out: string[] = [];
  console.log = console.error = (...a: unknown[]) => void out.push(a.join(" "));
  try {
    return { r: await fn(), out: out.join("\n") };
  } finally {
    [console.log, console.error] = [o, e];
  }
};

test("plugin flags may come before the name", async () => {
  mkdirSync(join(proj, ".sasacode", "plugins", "mine"));
  writeFileSync(join(proj, ".sasacode", "plugins", "mine", "plugin.json"), "{}");
  const { r } = await quiet(() => pluginCommand(["remove", "--project", "--yes", "mine"], proj));
  expect(r).toBe(0);
  const upd = await quiet(() => pluginCommand(["update", "--project"], proj));
  expect(upd.out).toContain("no npm or git plugins to update"); // not "--project is not an installed plugin"
  const bad = await quiet(() => pluginCommand(["list", "--nope"], proj));
  expect(bad.r).toBe(1);
});

test("a broken link in a plugins folder is skipped with a warning, not a crash", () => {
  symlinkSync(join(base, "missing"), join(proj, ".sasacode", "plugins", "dangling"));
  const warnings: string[] = [];
  expect(discoverPlugins(proj, warnings)).toEqual([]);
  expect(warnings[0]).toContain("dangling");
});

test("endpoint flags may come before the arguments; --key-env takes its value", async () => {
  const server = Bun.serve({ port: 0, fetch: () => Response.json({ object: "list", data: [{ id: "m", object: "model", max_model_len: 32768 }] }) });
  try {
    const { r, out } = await quiet(() => endpointCommand(["add", "--key-env", "LAN_KEY", "lan", `http://localhost:${server.port}`], proj));
    expect(r).toBe(0);
    expect(out).toContain("(32768 ctx)"); // vLLM's max_model_len
    expect(JSON.parse(readFileSync(join(home, "config.json"), "utf8")).providers.lan).toMatchObject({ apiKeyEnv: "LAN_KEY", api: "openai-chat" });
  } finally {
    server.stop(true);
  }
});

test("piped input that arrives after a second is still read; a file is read whole", async () => {
  const file = join(base, "in.txt");
  writeFileSync(file, "from a file");
  const { openSync, closeSync } = await import("node:fs");
  const fd = openSync(file, "r");
  expect(await readPipedStdin(false, fd)).toBe("from a file");
  closeSync(fd);
  const child = Bun.spawn(
    ["bash", "-c", `(sleep 1; echo LATE) | ${process.execPath} -e 'import { readPipedStdin } from "${join(import.meta.dir, "../src/stdin.ts")}"; console.log(JSON.stringify(await readPipedStdin()))'`],
    { stdout: "pipe" },
  );
  expect((await new Response(child.stdout).text()).trim()).toBe(JSON.stringify("LATE\n"));
});

test("keys read from ~/.sasacode/.env stay out of commands the agent runs", () => {
  const env = join(base, ".env");
  writeFileSync(env, "SASACODE_TEST_SECRET=abc\n");
  delete process.env.SASACODE_TEST_SECRET;
  hideFromChildren(loadEnvFile(env));
  expect(String(process.env.SASACODE_TEST_SECRET)).toBe("abc");
  expect(childEnv().SASACODE_TEST_SECRET).toBeUndefined();
  expect(childEnv({ X: "1" }).X).toBe("1");
  expect(childEnv().PATH).toBe(process.env.PATH!);
});
