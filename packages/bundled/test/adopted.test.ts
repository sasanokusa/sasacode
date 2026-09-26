// goal, background-sessions and auto-reconnect: user plugins that became bundled ones.
import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Plugin } from "@sasacode/plugin-api";
import autoReconnect from "../src/auto-reconnect.ts";
import backgroundSessions from "../src/background-sessions.ts";
import goal from "../src/goal.ts";

const root = mkdtempSync(join(tmpdir(), "sasacode-adopted-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

/** Just enough of the plugin API to drive one plugin: records what it registers and injects. */
function fake(plugin: Plugin, settings: Record<string, unknown> = {}, entries: Record<string, unknown[]> = {}) {
  const tools: Record<string, any> = {};
  const commands: Record<string, any> = {};
  const hooks: Record<string, ((e: any) => any)[]> = {};
  const injected: string[] = [];
  const notices: string[] = [];
  const status = new Map<string, string>();
  const api: any = {
    cwd: root,
    settings,
    session: {
      id: "s1",
      append: (k: string, d: unknown) => (entries[k] ??= []).push(d),
      entries: (k: string) => entries[k] ?? [],
      inject: (c: string) => injected.push(c),
    },
    ui: { interactive: true, notify: (m: string) => notices.push(m), setStatus: (k: string, t?: string) => (t ? status.set(k, t) : status.delete(k)) },
    agent: { model: () => ({ baseUrl: undefined }) },
    permissions: { addRules() {} },
    registerTool: (t: any) => (tools[t.name] = t),
    registerCommand: (c: any) => (commands[c.name] = c),
    on: (n: string, h: any) => (hooks[n] ??= []).push(h),
    log() {},
  };
  plugin(api);
  const fire = async (n: string, e: any = {}) => {
    let r: any;
    for (const h of hooks[n] ?? []) r = (await h(e)) ?? r;
    return r;
  };
  return { tools, commands, injected, notices, status, entries, fire };
}

test("goal: set by /goal or set_goal, shown in the prompt and footer, and a clear survives /resume", async () => {
  const g = fake(goal);
  await g.fire("session_start");
  await g.commands.goal.run({ args: "ログイン画面を完成させる" });
  expect(g.status.get("goal")).toContain("ログイン画面");
  expect(g.injected.at(-1)).toContain("ログイン画面を完成させる");
  expect((await g.fire("system_prompt", { prompt: "base" })).prompt).toContain("現在のゴール: ログイン画面を完成させる");
  const r = await g.tools.set_goal.execute({ status: "done" });
  expect(r.isError).toBeFalsy();
  expect(g.status.has("goal")).toBe(false); // only an active goal is shown
  await g.commands.goal.run({ args: "clear" });
  const resumed = fake(goal, {}, g.entries);
  await resumed.fire("session_start");
  expect((await resumed.tools.goal_status.execute({})).content[0].text).toContain("未設定");
});

test("background-sessions: a job runs detached and its exit code and output come back as a notice", async () => {
  const home = join(root, "home");
  process.env.SASACODE_HOME = home;
  try {
    const b = fake(backgroundSessions, { pollMs: 50 });
    const started = await b.tools.bg_start.execute({ command: "echo from-the-job; exit 3" }, { cwd: root });
    expect(started.details.logPath.startsWith(home)).toBe(true);
    for (let i = 0; i < 100 && !b.injected.length; i++) await Bun.sleep(20);
    expect(b.injected[0]).toContain("exit 3");
    expect(b.injected[0]).toContain("from-the-job");
    expect(b.tools.bg_start.permissionsAs).toBe("bash");

    const long = await b.tools.bg_start.execute({ command: "sleep 30" }, { cwd: root });
    await b.commands.bg.run({ args: `kill ${long.details.id}` });
    for (let i = 0; i < 100 && b.injected.length < 2; i++) await Bun.sleep(20);
    expect(b.injected[1]).toContain("強制終了");
  } finally {
    delete process.env.SASACODE_HOME;
  }
});

test("auto-reconnect: after an error while the endpoint is down, it waits and resumes the work once it answers", async () => {
  const server = Bun.serve({ port: 0, fetch: () => new Response("ok") });
  const port = server.port;
  const url = `http://127.0.0.1:${port}/`;
  server.stop(true);
  const a = fake(autoReconnect, { checkUrl: url, checkIntervalMs: 30, checkTimeoutMs: 200 });
  await a.fire("session_start");
  await a.fire("agent_end", { cause: "error" });
  for (let i = 0; i < 50 && !a.status.size; i++) await Bun.sleep(10);
  expect(a.status.get("auto-reconnect")).toContain("オフライン");
  const back = Bun.serve({ port, fetch: () => new Response("ok") });
  try {
    for (let i = 0; i < 100 && !a.injected.length; i++) await Bun.sleep(20);
    expect(a.injected[0]).toContain("再開");
    expect(a.status.size).toBe(0);
  } finally {
    back.stop(true);
  }
  // Online and failing for another reason (a bad key): nothing to wait for.
  const b = fake(autoReconnect, { checkUrl: url, checkIntervalMs: 30 });
  const up = Bun.serve({ port, fetch: () => new Response("ok") });
  try {
    await b.fire("agent_end", { cause: "error" });
    await Bun.sleep(100);
    expect(b.injected).toEqual([]);
  } finally {
    up.stop(true);
  }
});
