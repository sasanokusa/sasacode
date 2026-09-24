#!/usr/bin/env bun
// Token / cache benchmark: run TASKS against fresh copies of this repository with a real model,
// through the headless CLI (the released binary or the source), until a token budget is spent.
//
//   bun bench/run.ts --model commandcode/deepseek/deepseek-v4.1-flash --budget 1000000 \
//     --bin ~/.local/bin/sasacode --env ~/.sasacode/.env --context 1000000
import { appendFileSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { parseArgs } from "node:util";
import { $ } from "bun";
import { TASKS, type Task } from "./tasks.ts";

const { values: opt } = parseArgs({
  options: {
    model: { type: "string" },
    budget: { type: "string", default: "1000000" },
    bin: { type: "string", default: "sasacode" },
    env: { type: "string" },
    context: { type: "string" },
    "max-turns": { type: "string", default: "60" },
    out: { type: "string", default: "bench/results" },
  },
});
if (!opt.model) throw new Error("--model <provider/model> is required");
const repo = resolve(import.meta.dir, "..");
const budget = Number(opt.budget);
const stamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
const outDir = resolve(repo, opt.out!);
mkdirSync(outDir, { recursive: true });
const resultsFile = join(outDir, `${stamp}_${opt.model.replace(/\//g, "_")}.jsonl`);

// An isolated SASACODE_HOME: the API key, and the model's real context window so compaction
// does not kick in at the 128k default.
const home = mkdtempSync(join(tmpdir(), "sasacode-bench-home-"));
if (opt.env) copyFileSync(opt.env, join(home, ".env"));
const overrides = opt.context ? { [opt.model]: { contextWindow: Number(opt.context) } } : {};
writeFileSync(join(home, "config.json"), JSON.stringify({ modelOverrides: overrides }));

interface RequestStat {
  input: number;
  cacheRead: number;
  cacheWrite: number;
  output: number;
  ttftMs?: number;
  durationMs: number;
  stopReason: string;
}

interface Run {
  task: string;
  kind: Task["kind"];
  pass: number;
  prompt: number;
  session?: string;
  requests: RequestStat[];
  tools: Record<string, number>;
  toolErrors: number;
  repairs: number;
  pluginErrors: number;
  errors: string[];
  cause?: string;
  wallMs: number;
}

async function runPrompt(ws: string, prompt: string, session: string | undefined): Promise<Omit<Run, "task" | "kind" | "pass" | "prompt">> {
  const args = [opt.bin!, "-m", opt.model!, "--permission", "auto", "--output", "jsonl", "--max-turns", opt["max-turns"]!];
  if (session) args.push("-r", session);
  args.push("-p", prompt);
  const run: Omit<Run, "task" | "kind" | "pass" | "prompt"> = { requests: [], tools: {}, toolErrors: 0, repairs: 0, pluginErrors: 0, errors: [], wallMs: 0 };
  const started = performance.now();
  const proc = Bun.spawn(args, { cwd: ws, env: { ...process.env, SASACODE_HOME: home }, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
  let msgStart = 0;
  let firstDelta: number | undefined;
  let buf = "";
  const decoder = new TextDecoder();
  for await (const chunk of proc.stdout) {
    buf += decoder.decode(chunk, { stream: true });
    let nl: number;
    while ((nl = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, nl);
      buf = buf.slice(nl + 1);
      if (!line.trim()) continue;
      const now = performance.now();
      let e: any;
      try {
        e = JSON.parse(line);
      } catch {
        continue;
      }
      if (e.type === "session") run.session = e.id ?? undefined;
      else if (e.type === "message_start") {
        msgStart = now;
        firstDelta = undefined;
      } else if (e.type === "message_update") firstDelta ??= now;
      else if (e.type === "message_end" && e.message.role === "assistant") {
        const u = e.message.usage;
        run.requests.push({
          input: u.input,
          cacheRead: u.cacheRead,
          cacheWrite: u.cacheWrite,
          output: u.output,
          ttftMs: firstDelta !== undefined ? Math.round(firstDelta - msgStart) : undefined,
          durationMs: Math.round(now - msgStart),
          stopReason: e.message.stopReason,
        });
      } else if (e.type === "message_end" && e.message.role === "tool" && e.message.isError) run.toolErrors++;
      else if (e.type === "tool_start") run.tools[e.call.name] = (run.tools[e.call.name] ?? 0) + 1;
      else if (e.type === "tool_repaired") run.repairs++;
      else if (e.type === "plugin_error") run.pluginErrors++;
      else if (e.type === "error") run.errors.push(e.error);
      else if (e.type === "agent_end") run.cause = e.cause;
    }
  }
  const stderr = await new Response(proc.stderr).text();
  await proc.exited;
  if (proc.exitCode && !run.cause) run.errors.push(stderr.trim().split("\n").slice(-3).join(" | "));
  run.wallMs = Math.round(performance.now() - started);
  return run;
}

async function prepare(task: Task): Promise<string> {
  const ws = mkdtempSync(join(tmpdir(), `sasacode-bench-${task.id}-`));
  await $`git clone --quiet ${repo} ${ws}`.quiet();
  await $`bun install --frozen-lockfile`.cwd(ws).quiet();
  if (task.setup) {
    const path = join(ws, task.setup.file);
    const text = readFileSync(path, "utf8");
    if (!text.includes(task.setup.from)) throw new Error(`${task.id}: setup text not found in ${task.setup.file}`);
    writeFileSync(path, text.replace(task.setup.from, task.setup.to));
  }
  return ws;
}

async function check(task: Task, ws: string): Promise<boolean | null> {
  if (!task.check) return null;
  const p = Bun.spawn(["sh", "-c", task.check], { cwd: ws, stdout: "ignore", stderr: "ignore", env: { ...process.env, SASACODE_HOME: home } });
  const timer = setTimeout(() => p.kill(), 300_000);
  await p.exited;
  clearTimeout(timer);
  return p.exitCode === 0;
}

const tokens = (r: RequestStat) => r.input + r.cacheRead + r.cacheWrite + r.output;
let spent = 0;
console.log(`model ${opt.model}, budget ${budget.toLocaleString()} tokens → ${resultsFile}`);
for (let pass = 1; spent < budget; pass++) {
  for (const task of TASKS) {
    if (spent >= budget) break;
    const ws = await prepare(task);
    const taskStart = performance.now();
    let session: string | undefined;
    const runs: Run[] = [];
    for (const [i, prompt] of task.prompts.entries()) {
      const r = await runPrompt(ws, prompt, session);
      session = r.session ?? session;
      runs.push({ task: task.id, kind: task.kind, pass, prompt: i, ...r });
    }
    const ok = await check(task, ws);
    const used = runs.flatMap((r) => r.requests).reduce((n, r) => n + tokens(r), 0);
    spent += used;
    const record = { task: task.id, kind: task.kind, pass, ok, wallMs: Math.round(performance.now() - taskStart), tokens: used, runs };
    appendFileSync(resultsFile, `${JSON.stringify(record)}\n`);
    const reqs = runs.flatMap((r) => r.requests);
    const read = reqs.reduce((n, r) => n + r.cacheRead, 0);
    const prompt = reqs.reduce((n, r) => n + r.input + r.cacheRead + r.cacheWrite, 0);
    console.log(
      `[pass ${pass}] ${task.id.padEnd(24)} ${ok === null ? "  -  " : ok ? " ok  " : "FAIL "} ${String(reqs.length).padStart(3)} req  ${used.toLocaleString().padStart(9)} tok  cache ${prompt ? ((read / prompt) * 100).toFixed(1) : "0"}%  ${Math.round(record.wallMs / 1000)}s  total ${spent.toLocaleString()}`,
    );
    rmSync(ws, { recursive: true, force: true });
  }
}
rmSync(home, { recursive: true, force: true });
console.log(`done: ${spent.toLocaleString()} tokens → ${resultsFile}`);
