#!/usr/bin/env bun
// Markdown summary of a bench/run.ts results file.   bun bench/summarize.ts <results.jsonl>
import { readFileSync } from "node:fs";

interface Req {
  input: number;
  cacheRead: number;
  cacheWrite: number;
  output: number;
  ttftMs?: number;
  durationMs: number;
}
interface Run {
  prompt: number;
  requests: Req[];
  tools: Record<string, number>;
  toolErrors: number;
  repairs: number;
  errors: string[];
  cause?: string;
}
interface TaskRecord {
  task: string;
  kind: string;
  pass: number;
  ok: boolean | null;
  wallMs: number;
  tokens: number;
  runs: Run[];
}

const file = process.argv[2];
if (!file) throw new Error("usage: bun bench/summarize.ts <results.jsonl>");
const records: TaskRecord[] = readFileSync(file, "utf8").trim().split("\n").map((l) => JSON.parse(l));

const promptTokens = (r: Req) => r.input + r.cacheRead + r.cacheWrite;
const sum = (xs: number[]) => xs.reduce((a, b) => a + b, 0);
const pct = (a: number, b: number) => (b ? `${((a / b) * 100).toFixed(1)}%` : "-");
const fmt = (n: number) => n.toLocaleString("en-US");
const quantile = (xs: number[], q: number) => {
  if (!xs.length) return 0;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor(q * s.length))]!;
};
const hit = (rs: Req[]) => pct(sum(rs.map((r) => r.cacheRead)), sum(rs.map(promptTokens)));

const allReqs = records.flatMap((r) => r.runs.flatMap((x) => x.requests));
const graded = records.filter((r) => r.ok !== null);
const ttft = allReqs.flatMap((r) => (r.ttftMs !== undefined ? [r.ttftMs] : []));
const genMs = sum(allReqs.map((r) => r.durationMs - (r.ttftMs ?? 0)));
const out: string[] = [];

out.push("## Totals", "", "| | |", "| --- | --- |");
out.push(`| Tasks run | ${records.length} (${Math.max(...records.map((r) => r.pass))} passes over ${new Set(records.map((r) => r.task)).size} tasks) |`);
out.push(`| Success (graded tasks) | ${graded.filter((r) => r.ok).length} / ${graded.length} |`);
out.push(`| Model requests | ${fmt(allReqs.length)} |`);
out.push(`| Total tokens | ${fmt(sum(allReqs.map((r) => promptTokens(r) + r.output)))} |`);
out.push(`| Prompt tokens | ${fmt(sum(allReqs.map(promptTokens)))} (uncached ${fmt(sum(allReqs.map((r) => r.input)))}, cache read ${fmt(sum(allReqs.map((r) => r.cacheRead)))}) |`);
out.push(`| **Cache hit rate** (cache read ÷ prompt tokens) | **${hit(allReqs)}** |`);
out.push(`| Output tokens | ${fmt(sum(allReqs.map((r) => r.output)))} |`);
out.push(`| Time to first token | median ${quantile(ttft, 0.5)} ms, p90 ${quantile(ttft, 0.9)} ms |`);
out.push(`| Output speed (after first token) | ${(sum(allReqs.map((r) => r.output)) / (genMs / 1000)).toFixed(0)} tok/s |`);
out.push(`| Wall time | ${(sum(records.map((r) => r.wallMs)) / 60000).toFixed(1)} min |`);
out.push(`| Tool calls / failed / repaired | ${sum(records.flatMap((r) => r.runs.map((x) => sum(Object.values(x.tools)))))} / ${sum(records.flatMap((r) => r.runs.map((x) => x.toolErrors)))} / ${sum(records.flatMap((r) => r.runs.map((x) => x.repairs)))} |`);
out.push("");

out.push("## Per task", "", "| pass | task | result | requests | tokens | cache hit | tools | wall |", "| --- | --- | --- | --- | --- | --- | --- | --- |");
for (const r of records) {
  const reqs = r.runs.flatMap((x) => x.requests);
  const tools = Object.entries(r.runs.reduce<Record<string, number>>((acc, x) => {
    for (const [k, v] of Object.entries(x.tools)) acc[k] = (acc[k] ?? 0) + v;
    return acc;
  }, {}))
    .map(([k, v]) => `${k}×${v}`)
    .join(" ");
  const result = r.ok === null ? "-" : r.ok ? "ok" : "**fail**";
  const causes = r.runs.map((x) => x.cause).filter((c) => c && c !== "done");
  out.push(`| ${r.pass} | ${r.task} | ${result}${causes.length ? ` (${causes.join(",")})` : ""} | ${reqs.length} | ${fmt(r.tokens)} | ${hit(reqs)} | ${tools} | ${Math.round(r.wallMs / 1000)}s |`);
}
out.push("");

// How caching builds up within a session: the n-th request of each prompt.
const buckets: [string, (i: number) => boolean][] = [
  ["1st request", (i) => i === 0],
  ["2nd–5th", (i) => i >= 1 && i <= 4],
  ["6th–10th", (i) => i >= 5 && i <= 9],
  ["11th+", (i) => i >= 10],
];
out.push("## Cache hit rate by position in the run", "", "| requests | count | cache hit |", "| --- | --- | --- |");
for (const [label, test] of buckets) {
  const rs = records.flatMap((r) => r.runs.flatMap((x) => x.requests.filter((_, i) => test(i))));
  out.push(`| ${label} | ${rs.length} | ${hit(rs)} |`);
}
const followUps = records.flatMap((r) => r.runs.filter((x) => x.prompt > 0).map((x) => x.requests[0]).filter(Boolean)) as Req[];
if (followUps.length) out.push(`| 1st request of a resumed follow-up (\`-r <id> -p\`) | ${followUps.length} | ${hit(followUps)} |`);
out.push("");
console.log(out.join("\n"));
