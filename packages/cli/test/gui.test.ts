// `sasacode gui [folder]`: starts sasacode-gui on the folder and leaves it running on its own.
import { afterAll, expect, test } from "bun:test";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const MAIN = resolve(import.meta.dir, "../src/main.ts");
const NO_BUNFIG = resolve(import.meta.dir, "../../../scripts/no-bunfig.toml");
const root = realpathSync(mkdtempSync(join(tmpdir(), "sasacode-gui-cli-")));
afterAll(() => rmSync(root, { recursive: true, force: true }));

/** A place to run from: a home, a project, and a folder for stand-in programs. */
function place(name: string) {
  const [home, proj, bin] = ["home", "proj", "bin"].map((d) => join(root, name, d)) as [string, string, string];
  for (const d of [home, join(proj, ".sasacode"), bin]) mkdirSync(d, { recursive: true });
  writeFileSync(join(home, ".env"), "");
  return { home, proj, bin };
}

/**
 * A stand-in for sasacode-gui that stays for a while, as a window does: it writes where it was
 * started and what it was given, and `<out>.stayed` when it leaves.
 */
function fakeGui(dir: string, name: string, out: string): string {
  const command = join(dir, name);
  const script = `#!/bin/sh\n{ pwd; for a in "$@"; do printf '%s\\n' "$a"; done; } > "${out}.part"\nmv "${out}.part" "${out}"\nsleep 2\n: > "${out}.stayed"\n`;
  writeFileSync(command, script);
  chmodSync(command, 0o755);
  return command;
}

const appears = async (path: string) => {
  for (let i = 0; i < 400 && !existsSync(path); i++) await Bun.sleep(25);
  return existsSync(path);
};

async function sasacode(args: string[], at: { home: string; proj: string; bin: string }) {
  const p = Bun.spawn([process.execPath, "--no-env-file", `--config=${NO_BUNFIG}`, MAIN, ...args], {
    cwd: at.proj,
    // Only the stand-ins and the system's own tools: a sasacode-gui installed on this machine is not found.
    env: { PATH: `${at.bin}:/usr/bin:/bin`, HOME: join(at.home, ".."), USERPROFILE: join(at.home, ".."), SASACODE_HOME: at.home, SASACODE_LANG: "en", NO_COLOR: "1" },
    stdout: "pipe",
    stderr: "pipe",
  });
  const [out, err, code] = await Promise.all([new Response(p.stdout).text(), new Response(p.stderr).text(), p.exited]);
  return { out, err, code };
}

// The stand-ins are shell scripts: there is no such thing on Windows.
const unix = test.skipIf(process.platform === "win32");

unix("gui: the window is started on the folder and is still there after sasacode has left", async () => {
  const at = place("starts");
  const given = join(at.bin, "given");
  fakeGui(at.bin, "sasacode-gui", given);
  const other = join(at.proj, "pkg");
  mkdirSync(other);

  const here = await sasacode(["gui"], at);
  expect(here).toEqual({ out: "", err: "", code: 0 });
  expect(await appears(given)).toBe(true);
  // From source there is no sasacode program to name: the window finds its own.
  expect(readFileSync(given, "utf8").trimEnd().split("\n")).toEqual([at.proj, at.proj]);
  // sasacode is gone, and the window had not left yet: it leaves later, by itself.
  expect(existsSync(`${given}.stayed`)).toBe(false);
  expect(await appears(`${given}.stayed`)).toBe(true);

  rmSync(given);
  expect((await sasacode(["gui", "pkg"], at)).code).toBe(0);
  expect(await appears(given)).toBe(true);
  expect(readFileSync(given, "utf8").trimEnd().split("\n")).toEqual([other, other]);
}, 30_000);

unix("gui: the program is the one the user's config names, never one a project's config names", async () => {
  const at = place("config");
  const mine = fakeGui(at.bin, "my-gui", join(at.bin, "mine"));
  const theirs = fakeGui(at.bin, "their-gui", join(at.bin, "theirs"));
  writeFileSync(join(at.proj, ".sasacode", "config.json"), JSON.stringify({ plugins: { settings: { gui: { command: theirs } } } }));

  // Nothing of the user's says where it is, and none is on PATH: the project's is not started.
  const none = await sasacode(["gui"], at);
  expect(none.code).toBe(1);
  expect(none.err).toContain("sasacode-gui not found");
  expect(none.err).toContain("plugins.settings.gui.command");

  writeFileSync(join(at.home, "config.json"), JSON.stringify({ plugins: { settings: { gui: { command: mine } } } }));
  expect((await sasacode(["gui"], at)).code).toBe(0);
  expect(await appears(join(at.bin, "mine"))).toBe(true);
  expect(existsSync(join(at.bin, "theirs"))).toBe(false);
}, 30_000);

test("gui: what is not a folder, and more than a folder, are turned down before anything starts", async () => {
  const at = place("refused");
  const missing = await sasacode(["gui", "nowhere"], at);
  expect(missing.code).toBe(1);
  expect(missing.err).toMatch(/nowhere is not a folder/);
  const many = await sasacode(["gui", ".", "--session"], at);
  expect(many.code).toBe(1);
  expect(many.err).toContain("usage: sasacode gui [folder]");
}, 30_000);
