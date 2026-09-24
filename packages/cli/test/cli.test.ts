import { afterAll, beforeEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { emptyUsage, registerApi, replayProvider } from "@sasacode/ai";
import { loadConfig, mergeConfig } from "../src/config.ts";
import { runHeadless } from "../src/headless.ts";
import { setup } from "../src/setup.ts";

const root = mkdtempSync(join(tmpdir(), "sasacode-cli-"));
const home = join(root, "home");
const proj = join(root, "proj");
afterAll(() => rmSync(root, { recursive: true, force: true }));

function writeConfigs(global: object, project: object) {
  mkdirSync(home, { recursive: true });
  mkdirSync(join(proj, ".sasacode"), { recursive: true });
  writeFileSync(join(home, "config.json"), JSON.stringify(global));
  writeFileSync(join(proj, ".sasacode", "config.json"), JSON.stringify(project));
}

beforeEach(() => {
  process.env.SASACODE_HOME = home;
});

test("project config wins; permission rules from both levels are kept", () => {
  const merged = mergeConfig(
    { model: "a/x", thinking: "high", permissions: { deny: ["bash(rm *)"] }, providers: { a: { baseUrl: "g" } } },
    { model: "b/y", permissions: { deny: ["edit(.env)"] }, providers: { a: { headers: { h: "1" } } } },
  );
  expect(merged.model).toBe("b/y");
  expect(merged.thinking).toBe("high");
  expect(merged.permissions?.deny).toEqual(["bash(rm *)", "edit(.env)"]);
  expect(merged.providers?.a).toEqual({ baseUrl: "g", headers: { h: "1" } });
});

test("an untrusted project cannot loosen permissions", () => {
  writeConfigs({}, { permissions: { mode: "auto", allow: ["bash"], deny: ["edit(.env)"] } });
  const { config, warnings } = loadConfig(proj);
  expect(config.permissions?.mode).toBeUndefined();
  expect(config.permissions?.allow).toBeUndefined();
  expect(config.permissions?.deny).toEqual(["edit(.env)"]);
  expect(warnings[0]).toContain("trustedProjects");

  writeConfigs({ trustedProjects: [proj] }, { permissions: { mode: "auto" } });
  expect(loadConfig(proj).config.permissions?.mode).toBe("auto");
});

test("headless jsonl emits the whole run and exits 0", async () => {
  writeConfigs({ providers: { test: { api: "replay" } }, model: "test/m" }, {});
  const msg = (text: string) => ({
    role: "assistant" as const,
    content: [{ type: "text" as const, text }],
    api: "replay",
    provider: "test",
    model: "m",
    usage: emptyUsage(),
    stopReason: "stop" as const,
    timestamp: 0,
  });
  registerApi("replay", replayProvider([msg("hello from replay")]));
  const h = await setup({ cwd: proj, noSession: true });
  const lines: string[] = [];
  const orig = process.stdout.write.bind(process.stdout);
  process.stdout.write = ((s: string) => (lines.push(s), true)) as typeof process.stdout.write;
  try {
    expect(await runHeadless(h, "hi", "jsonl")).toBe(0);
  } finally {
    process.stdout.write = orig;
  }
  const events = lines.join("").trim().split("\n").map((l) => JSON.parse(l));
  expect(events[0].type).toBe("message_end"); // the user message
  expect(events.map((e) => e.type)).toContain("message_update");
  expect(events.at(-1)).toEqual({ type: "agent_end", cause: "done" });
  expect(JSON.stringify(events)).toContain("hello from replay");
});
