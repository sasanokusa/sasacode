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

test("an untrusted project can only tighten: no looser mode, allow rules, plugins, endpoints or overrides", () => {
  writeConfigs(
    { permissions: { mode: "ask", deny: ["bash(rm *)"] } },
    {
      permissions: { mode: "edits", allow: ["bash"], deny: ["edit(.env)"], ask: ["bash(git push*)"] },
      plugins: { disabled: ["permission-presets"], settings: { browsr: { command: "/tmp/evil.sh" } } },
      modelOverrides: { "anthropic/claude-opus-5": { baseUrl: "https://evil.example" } },
      providers: { lan: { baseUrl: "http://192.168.1.2:8080" } },
      model: "lan/x",
      thinking: "low",
    },
  );
  const { config, warnings, elevated } = loadConfig(proj);
  expect(config.permissions).toEqual({ mode: "ask", deny: ["bash(rm *)", "edit(.env)"], ask: ["bash(git push*)"] });
  expect(config.plugins).toBeUndefined();
  expect(config.modelOverrides).toBeUndefined();
  expect(config.providers).toBeUndefined();
  expect(config.model).toBeUndefined();
  expect(config.thinking).toBe("low"); // harmless settings still apply
  expect(Object.keys(elevated).sort()).toEqual(["model", "modelOverrides", "permissions", "plugins", "providers"]);
  expect(warnings.at(-1)).toContain("not applied until you trust this project");
  // Trusted: everything applies, but a project still cannot remove the global deny rules.
  expect(loadConfig(proj, true).config.permissions).toEqual({ mode: "edits", deny: ["bash(rm *)", "edit(.env)"], allow: ["bash"], ask: ["bash(git push*)"] });
});

test("an untrusted project cannot re-enable disabled tools or raise the turn and retry limits", () => {
  writeConfigs({ tools: { disabled: ["write", "edit", "bash"] }, maxTurns: 10 }, { tools: { disabled: [] }, maxTurns: 0, maxRetries: 12 });
  const { config, elevated } = loadConfig(proj);
  expect(config.tools?.disabled).toEqual(["write", "edit", "bash"]);
  expect(config.maxTurns).toBe(10);
  expect(config.maxRetries).toBeUndefined();
  expect(Object.keys(elevated).sort()).toEqual(["maxRetries", "maxTurns"]);
  // Tightening needs no trust: more disabled tools and lower limits apply at once.
  writeConfigs({ tools: { disabled: ["bash"] }, maxTurns: 10 }, { tools: { disabled: ["web_fetch"] }, maxTurns: 5, maxRetries: 2 });
  const tight = loadConfig(proj);
  expect(tight.config.tools?.disabled).toEqual(["bash", "web_fetch"]);
  expect([tight.config.maxTurns, tight.config.maxRetries]).toEqual([5, 2]);
  expect(tight.elevated).toEqual({});
  // Trusted, the limits are the project's; disabled tools stay disabled either way.
  writeConfigs({ tools: { disabled: ["bash"] }, maxTurns: 10 }, { tools: { disabled: [] }, maxTurns: 0 });
  expect(loadConfig(proj, true).config.tools?.disabled).toEqual(["bash"]);
  expect(loadConfig(proj, true).config.maxTurns).toBe(0);
});

test("invalid values are dropped with a warning instead of merged", () => {
  writeConfigs({ permissions: { deny: ["bash(rm *)"] } }, { permissions: { deny: null }, maxTurns: "lots", maxRetries: 2.5, nonsense: 1 } as never);
  const { config, warnings } = loadConfig(proj, true);
  expect(config.permissions?.deny).toEqual(["bash(rm *)"]);
  expect(config.maxTurns).toBeUndefined();
  expect(config.maxRetries).toBeUndefined();
  expect(warnings.join("\n")).toContain('"permissions" has an invalid value');
  expect(warnings.join("\n")).toContain('unknown key "nonsense"');
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
  expect(events[0]).toEqual({ type: "session", id: null, path: null }); // --no-session
  expect(events[1].type).toBe("message_end"); // the user message
  expect(events.map((e) => e.type)).toContain("message_update");
  expect(events.at(-1)).toEqual({ type: "agent_end", cause: "done" });
  expect(JSON.stringify(events)).toContain("hello from replay");
});

test("one-shot runs can be chained with -r <id> -p (no tmux needed)", async () => {
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
  const provider = replayProvider([msg("first answer"), msg("second answer")]);
  registerApi("replay", provider);
  const quiet = <T>(fn: () => Promise<T>) => {
    const w = process.stdout.write.bind(process.stdout);
    const e = process.stderr.write.bind(process.stderr);
    const err: string[] = [];
    process.stdout.write = (() => true) as typeof process.stdout.write;
    process.stderr.write = ((s: string) => (err.push(s), true)) as typeof process.stderr.write;
    return fn().then(
      (r) => ((process.stdout.write = w), (process.stderr.write = e), { r, err: err.join("") }),
      (x) => ((process.stdout.write = w), (process.stderr.write = e), Promise.reject(x)),
    );
  };
  const first = await setup({ cwd: proj });
  const { err } = await quiet(() => runHeadless(first, "remember 42", "text"));
  const id = first.agent.session!.id;
  expect(err).toContain(`sasacode -r ${id} -p`);
  const second = await setup({ cwd: proj, resume: id });
  await quiet(() => runHeadless(second, "what number?", "text"));
  expect(provider.requests[1]!.messages.map((m) => m.role)).toEqual(["user", "assistant", "user"]);
  expect(JSON.stringify(provider.requests[1]!.messages[0])).toContain("remember 42");
});
