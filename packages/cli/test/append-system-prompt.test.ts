import { afterAll, beforeAll, beforeEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setLang } from "@sasacode/host";
import { readPromptFile, setup } from "../src/setup.ts";

beforeAll(() => setLang("en")); // these assertions quote the English strings

const root = mkdtempSync(join(tmpdir(), "sasacode-append-"));
const home = join(root, "home");
const proj = join(root, "proj");
afterAll(() => rmSync(root, { recursive: true, force: true }));
beforeEach(() => {
  process.env.SASACODE_HOME = home;
  mkdirSync(home, { recursive: true });
  mkdirSync(proj, { recursive: true });
  writeFileSync(join(home, "config.json"), JSON.stringify({ providers: { test: { api: "replay" } }, model: "test/m", instructions: "FROM-CONFIG" }));
});

test("the appended text follows config.instructions and precedes the environment facts", async () => {
  const h = await setup({ cwd: proj, noSession: true, appendSystemPrompt: "FROM-FILE\nsecond line" });
  const prompt = h.agent.systemPrompt;
  expect(prompt.indexOf("FROM-CONFIG")).toBeGreaterThan(0);
  expect(prompt.indexOf("FROM-FILE")).toBeGreaterThan(prompt.indexOf("FROM-CONFIG"));
  expect(prompt.indexOf("Environment:")).toBeGreaterThan(prompt.indexOf("second line"));
});

test("without the option, or with an empty one, the system prompt is unchanged", async () => {
  const plain = (await setup({ cwd: proj, noSession: true })).agent.systemPrompt;
  expect(plain).toContain("FROM-CONFIG");
  expect((await setup({ cwd: proj, noSession: true, appendSystemPrompt: "" })).agent.systemPrompt).toBe(plain);
  expect((await setup({ cwd: proj, noSession: true, appendSystemPrompt: " \n" })).agent.systemPrompt).toBe(plain);
});

test("it works without config.instructions too", async () => {
  writeFileSync(join(home, "config.json"), JSON.stringify({ providers: { test: { api: "replay" } }, model: "test/m" }));
  const prompt = (await setup({ cwd: proj, noSession: true, appendSystemPrompt: "ONLY-FILE" })).agent.systemPrompt;
  expect(prompt).toContain("ONLY-FILE");
  expect(prompt).not.toContain("FROM-CONFIG");
});

test("readPromptFile reads UTF-8, and a file it cannot read is an error naming the path", () => {
  const file = join(root, "prompt.txt");
  writeFileSync(file, "人格: ていねいに\n");
  expect(readPromptFile(file)).toBe("人格: ていねいに\n");
  writeFileSync(file, "");
  expect(readPromptFile(file)).toBe("");
  expect(() => readPromptFile(join(root, "missing.txt"))).toThrow(`failed to read ${join(root, "missing.txt")}`);
  expect(() => readPromptFile(root)).toThrow(`failed to read ${root}`); // a directory
});
