import { afterAll, expect, test } from "bun:test";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Agent, PermissionPolicy, PluginHost, SessionFile } from "@sasacode/agent";
import { setLang } from "@sasacode/host";
import { bundledPlugins, guiArgs, openGui, ownProgram } from "../src/index.ts";

// The messages checked below are the Japanese ones, whatever the machine's locale.
setLang("ja");
const root = realpathSync(mkdtempSync(join(tmpdir(), "sasacode-gui-")));
const env = { PATH: process.env.PATH, SASACODE_BIN: process.env.SASACODE_BIN };
afterAll(() => {
  for (const [k, v] of Object.entries(env)) v === undefined ? delete process.env[k] : (process.env[k] = v);
  rmSync(root, { recursive: true, force: true });
});

/**
 * A stand-in for sasacode-gui: writes where it was started and what it was given, then `then`.
 * `given` waits for it: a program started for the first time can take longer than the launcher waits.
 */
function fakeGui(name: string, then = "exit 0"): { command: string; given: () => Promise<string[]> } {
  const dir = join(root, name);
  mkdirSync(dir, { recursive: true });
  const command = join(dir, "sasacode-gui");
  const out = join(dir, "given");
  writeFileSync(command, `#!/bin/sh\n{ pwd; for a in "$@"; do printf '%s\\n' "$a"; done; } > "${out}.part"\nmv "${out}.part" "${out}"\n${then}\n`);
  chmodSync(command, 0o755);
  const given = async () => {
    for (let i = 0; i < 200 && !existsSync(out); i++) await Bun.sleep(25);
    return readFileSync(out, "utf8").trimEnd().split("\n");
  };
  return { command, given };
}

/** The plugin in a host, with what it told the user. */
async function withGui(settings: Record<string, unknown>, session?: SessionFile) {
  const agent = new Agent({ model: { id: "m", provider: "t", api: "none", contextWindow: 1e5, maxOutput: 1e3 }, cwd: root, systemPrompt: "", permissions: new PermissionPolicy("edits"), session });
  const host = new PluginHost({ agent, cwd: root, settings: (n) => (n === "gui" ? settings : {}) });
  const notes: { text: string; level?: string }[] = [];
  host.setUI({ interactive: false, notify: (text, level) => notes.push({ text, level }), confirm: async () => false, select: async () => undefined });
  await host.load("gui", bundledPlugins.gui!);
  const run = () => Promise.resolve(host.commands.find((c) => c.name === "gui")!.run({ args: "" }));
  return { agent, notes, run };
}

test("the window's arguments: the sasacode to run, the conversation, then the folder", () => {
  expect(guiArgs({ cwd: "/p" })).toEqual(["/p"]);
  expect(guiArgs({ cwd: "/p", session: "17cae3ae", program: "/usr/local/bin/sasacode" })).toEqual(["--sasacode", "/usr/local/bin/sasacode", "--session", "17cae3ae", "/p"]);
});

test("a compiled sasacode names itself to the window; from source, and when SASACODE_BIN chooses, it does not", () => {
  expect(ownProgram("/usr/local/bin/sasacode", {})).toBe("/usr/local/bin/sasacode");
  expect(ownProgram("C:\\Users\\me\\.sasacode\\bin\\sasacode.exe", {})).toBe("C:\\Users\\me\\.sasacode\\bin\\sasacode.exe");
  expect(ownProgram("/Users/me/.bun/bin/bun", {})).toBeUndefined();
  expect(ownProgram("/usr/local/bin/sasacode", { SASACODE_BIN: "/opt/sasacode" })).toBeUndefined();
});

// The stand-in is a shell script: there is no such thing on Windows.
const unix = test.skipIf(process.platform === "win32");

unix("/gui before anything is said opens the window on the folder alone", async () => {
  const fake = fakeGui("fresh");
  // A session that has an id and, with nothing said in it, no log.
  const session = SessionFile.create(join(root, "sessions"), root);
  const { notes, run } = await withGui({ command: fake.command }, session);
  await run();
  expect(existsSync(session.path)).toBe(false);
  expect(await fake.given()).toEqual([root, root]);
  expect(notes.map((n) => n.text)).toEqual(["GUIを開きました。"]);
});

unix("/gui in a conversation opens the window on it, and says the two do not follow each other", async () => {
  const fake = fakeGui("talking");
  const session = SessionFile.create(join(root, "sessions"), root);
  const { agent, notes, run } = await withGui({ command: fake.command }, session);
  agent.replaceMessages([{ role: "user", content: [{ type: "text", text: "こんにちは" }], timestamp: Date.now() }]);
  await run();
  expect(await fake.given()).toEqual([root, "--session", session.id, root]);
  expect(notes.map((n) => n.text).join("\n")).toContain("互いの発言を読み込みません");
});

unix("the window is found on PATH when no command is set, at the time it is asked for", async () => {
  process.env.PATH = "";
  const { notes, run } = await withGui({});
  await run();
  expect(notes[0]).toEqual({ text: expect.stringContaining("sasacode-guiが見つかりません"), level: "error" });
  expect(notes[0]!.text).toContain("\n  cargo install --locked --path crates/sasacode-gui\n");

  // Installed meanwhile: the same session finds it.
  const fake = fakeGui("on-path");
  process.env.PATH = `${join(root, "on-path")}:${env.PATH}`;
  await run();
  expect(await fake.given()).toEqual([root, root]);
  process.env.PATH = env.PATH;
});

unix("a window that leaves at once with an error is reported, not announced as opened", async () => {
  // Waited for longer than the launcher does by itself: what is checked is what an exit is taken for.
  const graceMs = 10_000;
  const old = fakeGui("old", "echo 'sasacode-gui: unknown option --session' >&2; exit 2");
  await expect(openGui({ command: old.command, cwd: root, session: "17cae3ae", graceMs })).rejects.toThrow(/すぐに終了しました.*終了コード2/);
  await expect(openGui({ command: join(root, "nowhere", "sasacode-gui"), cwd: root, graceMs })).rejects.toThrow("sasacode-guiを起動できませんでした");
  // A launcher that starts the window and leaves has not failed.
  const launcher = fakeGui("launcher");
  expect(await openGui({ command: launcher.command, cwd: root, graceMs })).toBe(launcher.command);
});
