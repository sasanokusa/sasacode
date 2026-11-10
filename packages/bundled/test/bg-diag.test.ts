// TEMPORARY (#128): what a background job leaves in its log on each OS. Removed before merge.
import { expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import backgroundSessions from "../src/background-sessions.ts";

const root = mkdtempSync(join(tmpdir(), "bg-diag-"));
process.env.SASACODE_HOME = root;

test("bg diag", async () => {
  const tools: Record<string, any> = {};
  const injected: string[] = [];
  const api: any = {
    cwd: root, settings: { pollMs: 20 },
    session: { id: "d", append() {}, entries: () => [], inject: (c: string) => injected.push(c) },
    ui: { interactive: true, notify() {}, setStatus() {} },
    agent: { model: () => ({ api: "anthropic" }) },
    permissions: { addRules() {} }, registerTool: (t: any) => (tools[t.name] = t), registerCommand() {}, on() {}, log() {},
  };
  backgroundSessions(api);
  const report: string[] = [];
  for (const command of ["seq 1 250", "printf '%s\\n' {1..250}", "echo one; echo two", "for i in 1 2 3; do echo $i; done", "echo x; sleep 1; echo y", "seq 1 3; exit 0", "/usr/bin/seq 1 3", "printf 'a\\nb\\n'; sleep 1"]) {
    const before = injected.length;
    const r = await tools.bg_start.execute({ command }, { cwd: root });
    for (let i = 0; i < 500 && injected.length === before; i++) await Bun.sleep(20);
    const dir = join(r.details.logPath, "..");
    const log = existsSync(r.details.logPath) ? readFileSync(r.details.logPath, "utf8") : "<no log>";
    const code = existsSync(join(dir, "exit-code")) ? readFileSync(join(dir, "exit-code"), "utf8").trim() : "<no exit-code>";
    report.push(`[${command}] exit=${code} logBytes=${log.length} head=${JSON.stringify(log.slice(0, 40))} notice=${JSON.stringify((injected[before] ?? "").slice(0, 200))}`);
  }
  console.log(report.join("\n"));
  expect(report.join("\n")).toBe("show");
}, 60_000);
