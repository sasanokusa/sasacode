import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { BUILTIN_PROVIDERS, defaultModelFor } from "@sasacode/ai";
import { dropBlankKeys, ensureEnvFile, keyVariables, loadEnvFile } from "../src/env.ts";

const dir = mkdtempSync(join(tmpdir(), "sasacode-env-"));
afterAll(() => rmSync(dir, { recursive: true, force: true }));

test("the generated .env lists every provider key, blank, readable only by the owner", () => {
  const path = join(dir, "home", ".env");
  expect(ensureEnvFile(path, BUILTIN_PROVIDERS)).toBe(true);
  const text = readFileSync(path, "utf8");
  for (const name of ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "OPENROUTER_API_KEY", "CMD_API_KEY"]) expect(text).toContain(`\n${name}=\n`);
  expect(text.match(/OPENAI_API_KEY=/g)).toHaveLength(1); // openai and openai-chat share one key
  expect(statSync(path).mode & 0o777).toBe(0o600);
  writeFileSync(path, "CMD_API_KEY=mine\n");
  expect(ensureEnvFile(path, BUILTIN_PROVIDERS)).toBe(false); // never overwritten
  expect(keyVariables(BUILTIN_PROVIDERS).map((k) => k.name)).toEqual(["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "OPENROUTER_API_KEY", "CMD_API_KEY"]);
});

test("blank entries mean not configured", () => {
  const path = join(dir, "blank.env");
  writeFileSync(path, 'ANTHROPIC_API_KEY=\nOPENAI_API_KEY=""\nCMD_API_KEY=abc\n# comment\n');
  const saved = { ...process.env };
  delete process.env.ANTHROPIC_API_KEY;
  delete process.env.OPENAI_API_KEY;
  delete process.env.CMD_API_KEY;
  process.env.OPENROUTER_API_KEY = "   "; // e.g. from a project .env Bun loaded
  try {
    dropBlankKeys(BUILTIN_PROVIDERS);
    loadEnvFile(path);
    expect(process.env.ANTHROPIC_API_KEY).toBeUndefined();
    expect(process.env.OPENAI_API_KEY).toBeUndefined();
    expect(process.env.OPENROUTER_API_KEY).toBeUndefined();
    expect(process.env.CMD_API_KEY as string | undefined).toBe("abc");
    expect(defaultModelFor(BUILTIN_PROVIDERS)).toBe("commandcode/deepseek/deepseek-v4-flash");
  } finally {
    process.env = saved;
  }
});

test("the default model follows the first provider with a key", () => {
  expect(defaultModelFor(BUILTIN_PROVIDERS, {})).toBe("anthropic/claude-opus-5");
  expect(defaultModelFor(BUILTIN_PROVIDERS, { OPENAI_API_KEY: "x", CMD_API_KEY: "y" })).toBe("openai/gpt-5.5");
  expect(defaultModelFor(BUILTIN_PROVIDERS, { ANTHROPIC_API_KEY: "", OPENROUTER_API_KEY: "k" })).toBe("openrouter/openrouter/auto");
});
