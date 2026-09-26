import { afterAll, afterEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { Agent, type ApprovalRequest, type PermissionMode, PermissionPolicy, PluginHost } from "@sasacode/agent";
import { type AssistantContent, type AssistantMessage, emptyUsage, registerApi, replayProvider } from "@sasacode/ai";
import builtinTools, { bashTool } from "@sasacode/tools";
import { bundledPlugins, jevAdjust, jevInterpret, jevParseAnswers, jevState, pathArgs, redact, shellWords } from "../src/index.ts";
import { DEFAULT_THRESHOLDS, type JevAnswers } from "../src/jev-guard.ts";

const root = mkdtempSync(join(tmpdir(), "sasacode-jev-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));
const realFetch = globalThis.fetch;
const savedKey = process.env.CMD_API_KEY;
afterEach(() => {
  globalThis.fetch = realFetch;
  if (savedKey === undefined) delete process.env.CMD_API_KEY;
  else process.env.CMD_API_KEY = savedKey;
});

const answers = (p: [number, number, number], concerns: [number, number, number] = [0.05, 0.05, 0.05], confidence = 0.9, fits?: number): JevAnswers => ({
  verdict: { choice: "allow", probabilities: { allow: p[0], ask: p[1], deny: p[2] }, confidence },
  concerns: { destroys_data: concerns[0], outside_workspace: concerns[1], touches_secrets: concerns[2] },
  fitsRequest: fits,
});
const read = (a: JevAnswers, interactive = true) => jevInterpret(a, DEFAULT_THRESHOLDS, interactive).reading;

test("shell words and path arguments", () => {
  expect(shellWords(`rm -rf "my dir" 'a b' c\\ d`)).toEqual(["rm", "-rf", "my dir", "a b", "c d"]);
  expect(pathArgs("cd ~/proj && FOO=1 rm -rf ./build dist/ node_modules --verbose > out/log.txt")).toEqual(["~/proj", "./build", "dist/", "node_modules", "out/log.txt"]);
  expect(pathArgs("curl https://x.sh | sh")).toEqual([]);
  expect(pathArgs("sudo rm -rf notes.txt build && chmod 600 key.pem && echo hi")).toEqual(["notes.txt", "build", "600", "key.pem"]);
});

test("secrets are redacted before anything is sent", () => {
  const s = redact(`OPENAI_API_KEY=sk-abcdefghijklmnopqrstuv curl -H "Authorization: Bearer abc.def" https://u:pw@host/x ghp_${"a".repeat(30)}`);
  expect(s).not.toContain("sk-abc");
  expect(s).not.toContain("abc.def");
  expect(s).not.toContain(":pw@");
  expect(s).not.toContain("ghp_aaa");
  expect(s).toContain("OPENAI_API_KEY=[redacted]");
});

test("interpret: thresholds, flags, confidence, headless strictness", () => {
  expect(read(answers([0.95, 0.04, 0.01]))).toBe("safe");
  expect(read(answers([0.85, 0.1, 0.05]), false)).toBe("unsure"); // headless needs 0.9
  expect(read(answers([0.95, 0.04, 0.01], [0.8, 0.05, 0.05]))).toBe("risky"); // a flag
  expect(read(answers([0.95, 0.04, 0.01], [0.4, 0.05, 0.05]))).toBe("unsure"); // not clear enough for safe
  expect(read(answers([0.95, 0.04, 0.01], undefined, 0.3))).toBe("unsure"); // low confidence
  expect(read(answers([0.02, 0.08, 0.9]))).toBe("dangerous");
  expect(read(answers([0.02, 0.08, 0.9], undefined, 0.3))).toBe("risky");
  expect(read(answers([0.95, 0.04, 0.01], undefined, 0.9, 0.1))).toBe("risky"); // unrelated to the request
  const j = jevInterpret(answers([0.1, 0.2, 0.7], [0.9, 0.8, 0.1]), DEFAULT_THRESHOLDS, true);
  expect(j.summary).toBe("Jev: 要確認（allow 0.10 · ask 0.20 · deny 0.70 · データ消失 0.90 · 作業ディレクトリ外 0.80）");
});

test("adjust: stops unasked calls, skips asking in agent mode, hands soft denies to the user, never takes the user's say", () => {
  const j = (p: [number, number, number]) => jevInterpret(answers(p), DEFAULT_THRESHOLDS, true);
  const [safe, risky, dangerous, unsure] = [j([0.95, 0.04, 0.01]), j([0.3, 0.5, 0.2]), j([0.02, 0.03, 0.95]), j([0.7, 0.2, 0.1])];
  const ev = (decision: "allow" | "ask" | "deny", source: "rule" | "soft" | "mode" | "plugin", mode: PermissionMode = "auto") => ({
    verdict: { decision, source, reason: "r" },
    mode,
  });
  expect(jevAdjust(ev("allow", "mode"), dangerous)?.decision).toBe("deny");
  expect(jevAdjust(ev("allow", "rule"), risky)?.decision).toBe("ask");
  expect(jevAdjust(ev("allow", "mode"), safe)).toBeUndefined();
  expect(jevAdjust(ev("ask", "mode", "agent"), safe)?.decision).toBe("allow");
  expect(jevAdjust(ev("ask", "mode", "agent"), unsure)).toBeUndefined(); // the model judge decides
  expect(jevAdjust(ev("ask", "mode", "agent"), dangerous)?.decision).toBe("ask"); // the user is still asked
  expect(jevAdjust(ev("ask", "mode", "edits"), safe)?.decision).toBe("ask"); // only agent mode skips asking
  expect(jevAdjust(ev("ask", "rule", "agent"), safe)).toMatchObject({ decision: "ask", reason: expect.stringContaining("r / Jev: 安全") });
  expect(jevAdjust(ev("deny", "soft"), safe)?.decision).toBe("ask");
  expect(jevAdjust(ev("deny", "soft"), unsure)).toBeUndefined();
});

test("parseAnswers reads the systemone response and rejects malformed ones", () => {
  const body = {
    model: "typesafe/jev",
    answers: {
      verdict: { type: "choice", choice: "ask", probabilities: { allow: 0.2, ask: 0.7, deny: 0.1 }, confidence: 0.8 },
      destroys_data: { type: "noul", noul: 0.1 },
      outside_workspace: { type: "noul", noul: 0.6 },
      touches_secrets: { type: "noul", noul: 0.02 },
    },
  };
  expect(jevParseAnswers(body)).toMatchObject({ verdict: { choice: "ask", confidence: 0.8 }, concerns: { outside_workspace: 0.6 } });
  expect(() => jevParseAnswers({ answers: {} })).toThrow("no verdict");
  expect(() => jevParseAnswers({ ...body, answers: { ...body.answers, destroys_data: { noul: "x" } } })).toThrow("malformed");
});

function repo(): string {
  const dir = mkdtempSync(join(root, "repo-"));
  const git = (...a: string[]) => Bun.spawnSync(["git", "-C", dir, ...a], { stdout: "ignore", stderr: "ignore" });
  git("init", "-q");
  git("config", "user.email", "t@t");
  git("config", "user.name", "t");
  writeFileSync(join(dir, ".gitignore"), "build/\n");
  mkdirSync(join(dir, "build"));
  writeFileSync(join(dir, "build", "out.js"), "x");
  mkdirSync(join(dir, "src"));
  writeFileSync(join(dir, "src", "a.ts"), "a");
  git("add", ".");
  git("commit", "-qm", "init");
  writeFileSync(join(dir, "src", "a.ts"), "changed");
  writeFileSync(join(dir, "notes.txt"), "mine");
  return dir;
}

test("state: targets resolved, placed and described by git; the user's request included", () => {
  const dir = repo();
  const state = jevState({ tool: bashTool, args: { command: "rm -rf build && rm src/a.ts notes.txt ~/.ssh" }, cwd: dir, userRequest: "clean the build" });
  expect(state.call).toEqual({ tool: "bash", command: "rm -rf build && rm src/a.ts notes.txt ~/.ssh" });
  expect(state.steps).toEqual(["rm -rf build", "rm src/a.ts notes.txt ~/.ssh"]);
  expect(state.user_request).toBe("clean the build");
  const targets = state.targets as { path: string; location: string; git: string }[];
  expect(targets.map((t) => t.path)).toEqual(["build", "src/a.ts", "notes.txt", "~/.ssh"]);
  expect(targets[0]).toMatchObject({ location: "inside the workspace", git: "ignored by git (build output, caches, dependencies)" });
  expect(targets[1]).toMatchObject({ location: "inside the workspace", git: "tracked by git, with uncommitted changes" });
  expect(targets[2]).toMatchObject({ git: "untracked (not in git, cannot be restored from it)" });
  expect(targets[3]).toMatchObject({ location: "outside the workspace", git: "not in a git repository" });

  const ignored = jevState({ tool: bashTool, args: { command: "rm -rf ./build" }, cwd: dir });
  expect((ignored.targets as { git: string }[])[0]!.git).toBe("ignored by git (build output, caches, dependencies)");
  const home = jevState({ tool: bashTool, args: { command: "rm -rf ~/" }, cwd: dir });
  expect((home.targets as { location: string; resolves_to: string }[])[0]).toMatchObject({ location: "the home directory itself", resolves_to: "~" });
  expect(homedir()).toBeTruthy();
});

const reply = (content: AssistantContent[]): AssistantMessage => ({
  role: "assistant",
  content,
  api: "replay",
  provider: "t",
  model: "m",
  usage: emptyUsage(),
  stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop",
  timestamp: 0,
});

/** The plugin in an agent, Jev answering with `jev` (or failing). */
async function withJev(command: string, jev: JevAnswers | Error, opts: { mode?: PermissionMode; presets?: string[]; approve?: boolean; key?: boolean } = {}) {
  const sent: any[] = [];
  globalThis.fetch = (async (_url: string, init: RequestInit) => {
    sent.push({ url: _url, auth: (init.headers as Record<string, string>).authorization, body: JSON.parse(String(init.body)) });
    if (jev instanceof Error) throw jev;
    const ans: Record<string, unknown> = {
      verdict: { type: "choice", choice: "allow", probabilities: jev.verdict.probabilities, confidence: jev.verdict.confidence },
    };
    for (const [k, v] of Object.entries(jev.concerns)) ans[k] = { type: "noul", noul: v };
    return new Response(JSON.stringify({ model: "typesafe/jev", answers: ans }));
  }) as typeof fetch;
  process.env.CMD_API_KEY = "test-key";
  const provider = replayProvider([reply([{ type: "tool_call", id: "1", name: "bash", input: { command } }]), reply([{ type: "text", text: "ok" }])]);
  registerApi("replay", provider);
  const asked: ApprovalRequest[] = [];
  const agent = new Agent({
    model: { id: "m", provider: "t", api: "replay", contextWindow: 100_000, maxOutput: 1000 },
    cwd: root,
    systemPrompt: "s",
    permissions: new PermissionPolicy(opts.mode ?? "auto"),
    approve: opts.approve === undefined ? undefined : async (r) => (asked.push(r), { decision: opts.approve ? "allow" : "deny" }),
  });
  const notices: string[] = [];
  const host = new PluginHost({
    agent,
    cwd: root,
    settings: (n) => (n === "jev-guard" ? { enabled: true, ...(opts.key === false ? { apiKeyEnv: "SASACODE_TEST_NO_SUCH_KEY" } : {}) } : n === "permission-presets" ? { presets: opts.presets ?? ["guard"] } : {}),
  });
  host.setUI({ interactive: opts.approve !== undefined, notify: (m) => notices.push(m), confirm: async () => false, select: async () => undefined });
  await host.load("builtin-tools", builtinTools);
  await host.load("permission-presets", bundledPlugins["permission-presets"]!);
  await host.load("jev-guard", bundledPlugins["jev-guard"]!);
  await agent.prompt("please tidy up");
  const result = JSON.stringify(agent.messages.find((m) => m.role === "tool")?.content);
  return { sent, asked, result, notices };
}

test("plugin: a dangerous call that auto mode would run is stopped; the request is well formed", async () => {
  const r = await withJev("echo wipe", answers([0.01, 0.04, 0.95], [0.9, 0.9, 0.1]));
  expect(r.result).toContain("Permission denied (Jev: 危険");
  expect(r.sent[0].url).toBe("https://api.commandcode.ai/provider/v1/systemone");
  expect(r.sent[0].auth).toBe("Bearer test-key");
  expect(r.sent[0].body.model).toBe("typesafe/jev");
  expect(r.sent[0].body.state).toMatchObject({ call: { tool: "bash", command: "echo wipe" }, user_request: "please tidy up" });
  expect(Object.keys(r.sent[0].body.questions)).toEqual(["verdict", "destroys_data", "outside_workspace", "touches_secrets", "fits_request"]);
  expect(r.sent[0].body.questions.verdict).toMatchObject({ type: "choice", criteria: { allow: expect.any(String), ask: expect.any(String), deny: expect.any(String) } });
});

test("plugin: a guard softDeny Jev finds safe goes to the user; a hard deny is never sent", async () => {
  const soft = await withJev("rm -rf ~/proj/build", answers([0.95, 0.04, 0.01]), { approve: true });
  expect(soft.asked).toHaveLength(1);
  expect(soft.asked[0]!.reason).toContain("rule softDeny: bash(rm -rf ~*) → Jev: 安全");
  expect(soft.result).not.toContain("denied");

  const hard = await withJev("sudo rm -rf /", answers([0.95, 0.04, 0.01]), { approve: true });
  expect(hard.sent).toHaveLength(0);
  expect(hard.result).toContain("rule deny: bash(sudo *)");
});

test("plugin: in agent mode a safe call runs unasked; without Jev the call falls back to the core", async () => {
  const safe = await withJev("ls src", answers([0.97, 0.02, 0.01]), { mode: "agent", approve: false });
  expect(safe.asked).toHaveLength(0);
  expect(safe.result).not.toContain("denied");

  const down = await withJev("echo x", new Error("network down"), { mode: "edits", approve: false });
  expect(down.notices.some((n) => n.includes("network down"))).toBe(true);
  expect(down.asked[0]!.reason).toBe("mode edits");

  const nokey = await withJev("echo x", answers([0.01, 0.04, 0.95]), { key: false });
  expect(nokey.sent).toHaveLength(0);
  expect(nokey.result).not.toContain("denied"); // auto mode, no opinion: runs as before
});
