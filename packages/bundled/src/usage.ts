// /usage: what this run used, what the ChatGPT plan has left, and a record of every session on
// this machine (read back from the session logs), with a contributions graph by day.
import { readdirSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import type { AssistantMessage, Plugin } from "@sasacode/plugin-api";
import { codexTokens, readCodexAuth } from "./openai-codex/auth.ts";

export interface UsageRecord {
  /** Epoch ms. */
  t: number;
  model: string;
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  cost: number;
}

const tokens = (r: Omit<UsageRecord, "t" | "model">) => r.input + r.output + r.cacheRead + r.cacheWrite;

function record(m: AssistantMessage): UsageRecord {
  const u = m.usage;
  return { t: m.timestamp, model: `${m.provider}/${m.model}`, input: u.input, output: u.output, cacheRead: u.cacheRead, cacheWrite: u.cacheWrite, cost: u.cost };
}

/**
 * Every model response recorded in the session logs under `home`. Summaries written by compaction
 * and subagent runs are not in the logs, so they are not counted.
 */
export function readRecords(home = process.env.SASACODE_HOME ?? join(homedir(), ".sasacode")): UsageRecord[] {
  const root = join(home, "sessions");
  const out: UsageRecord[] = [];
  let dirs: string[] = [];
  try {
    dirs = readdirSync(root);
  } catch {
    return out;
  }
  for (const d of dirs) {
    let files: string[] = [];
    try {
      files = readdirSync(join(root, d)).filter((f) => f.endsWith(".jsonl"));
    } catch {
      continue;
    }
    for (const f of files) {
      let text = "";
      try {
        text = readFileSync(join(root, d, f), "utf8");
      } catch {
        continue;
      }
      for (const line of text.split("\n")) {
        // Only new messages: a "replace" entry (compaction) lists earlier ones again.
        if (!line.startsWith('{"type":"message"') || !line.includes('"role":"assistant"')) continue;
        try {
          const m = (JSON.parse(line) as { message: AssistantMessage }).message;
          if (m.usage && m.timestamp) out.push(record(m));
        } catch {}
      }
    }
  }
  return out.sort((a, b) => a.t - b.t);
}

// ── aggregation ─────────────────────────────────────────────────────

interface Sum {
  tokens: number;
  output: number;
  requests: number;
  cost: number;
}
const empty = (): Sum => ({ tokens: 0, output: 0, requests: 0, cost: 0 });
function add(s: Sum, r: UsageRecord): Sum {
  s.tokens += tokens(r);
  s.output += r.output;
  s.requests++;
  s.cost += r.cost;
  return s;
}

export const dayKey = (t: number) => {
  const d = new Date(t);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
};

export function byDay(records: UsageRecord[]): Map<string, Sum> {
  const days = new Map<string, Sum>();
  for (const r of records) {
    const k = dayKey(r.t);
    days.set(k, add(days.get(k) ?? empty(), r));
  }
  return days;
}

const startOfDay = (t: number) => new Date(new Date(t).setHours(0, 0, 0, 0)).getTime();
const since = (records: UsageRecord[], t: number) => records.filter((r) => r.t >= t).reduce(add, empty());

// ── formatting ──────────────────────────────────────────────────────

const esc = (code: string) => (s: string) => `\x1b[${code}m${s}\x1b[0m`;
const gray = esc("90");
const bold = esc("1");
const cyan = esc("36");
/**
 * A day is two colored spaces, about square in a terminal cell grid. Block characters (■, █) would
 * be simpler but are ambiguous-width: two columns in CJK terminals, which breaks the grid.
 */
const cell = (color: number) => `\x1b[48;5;${color}m  \x1b[0m`;
/** Columns per week: a two-column day and a gap. */
const WEEK = 3;
const LEVELS = [237, 22, 28, 34, 40];

export function fmtCount(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${+(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
  if (n < 1_000_000_000) return `${+(n / 1_000_000).toFixed(n < 10_000_000 ? 2 : 1)}M`;
  return `${+(n / 1_000_000_000).toFixed(2)}B`;
}
const fmtCost = (c: number) => (c > 0 ? ` · $${c.toFixed(c < 1 ? 3 : 2)}` : "");
const sumLine = (s: Sum) => `${fmtCount(s.tokens)} トークン（出力 ${fmtCount(s.output)}）· ${s.requests} 回${fmtCost(s.cost)}`;

/** Display width without escape sequences (Japanese is two columns a character). */
function vis(s: string): number {
  let w = 0;
  for (const ch of s.replace(/\x1b\[[0-9;]*m/g, "")) w += /[\u3000-\u9fff\uff00-\uffef]/.test(ch) ? 2 : 1;
  return w;
}
const pad = (s: string, width: number) => s + " ".repeat(Math.max(1, width - vis(s)));

function bar(fraction: number, width: number, color = 34): string {
  const n = Math.round(Math.max(0, Math.min(1, fraction)) * width);
  return `\x1b[48;5;${color}m${" ".repeat(n)}\x1b[48;5;237m${" ".repeat(width - n)}\x1b[0m`;
}

function fmtWait(ms: number): string {
  const m = Math.max(0, Math.round(ms / 60_000));
  if (m < 60) return `${m}分`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}時間${m % 60 ? `${m % 60}分` : ""}`;
  return `${Math.floor(h / 24)}日${h % 24 ? `${h % 24}時間` : ""}`;
}

// ── contributions graph ─────────────────────────────────────────────

/** Level 0–4 for each day: quartiles of the days with any use, as GitHub does. */
function levels(values: number[]): (v: number) => number {
  const used = values.filter((v) => v > 0).sort((a, b) => a - b);
  const q = [0.25, 0.5, 0.75].map((p) => used[Math.floor(p * (used.length - 1))] ?? 0);
  return (v) => (v <= 0 ? 0 : v <= q[0]! ? 1 : v <= q[1]! ? 2 : v <= q[2]! ? 3 : 4);
}

/** Weeks as columns (Sunday on top), the last one holding today; month names above. */
export function graph(days: Map<string, Sum>, weeks: number, now = Date.now()): string[] {
  const today = new Date(startOfDay(now));
  const start = new Date(today);
  start.setDate(start.getDate() - today.getDay() - 7 * (weeks - 1));
  const grid: { key: string; date: Date; future: boolean }[][] = [];
  for (let w = 0; w < weeks; w++) {
    const col = [];
    for (let d = 0; d < 7; d++) {
      const date = new Date(start);
      date.setDate(start.getDate() + w * 7 + d);
      col.push({ key: dayKey(date.getTime()), date, future: date > today });
    }
    grid.push(col);
  }
  const level = levels(grid.flat().map((c) => days.get(c.key)?.tokens ?? 0));

  // Month labels over the week in which the month begins, where there is room.
  let months = "";
  for (let w = 0; w < weeks; w++) {
    const first = grid[w]!.find((c) => c.date.getDate() === 1);
    const label = w === 0 ? `${grid[0]![0]!.date.getMonth() + 1}月` : first ? `${first.date.getMonth() + 1}月` : "";
    const at = 3 + w * WEEK;
    const width = vis(months);
    if (label && at >= width + (width ? 1 : 0)) months += " ".repeat(at - width) + label;
  }
  const rows = [gray(months)];
  const names = ["", "月", "", "水", "", "金", ""];
  for (let d = 0; d < 7; d++) {
    let row = gray(names[d] ? `${names[d]} ` : "   ");
    for (let w = 0; w < weeks; w++) {
      const c = grid[w]![d]!;
      row += c.future ? " ".repeat(WEEK) : `${cell(LEVELS[level(days.get(c.key)?.tokens ?? 0)]!)} `;
    }
    rows.push(row.trimEnd());
  }
  rows.push(`${gray("   少 ")}${LEVELS.map((l) => `${cell(l)} `).join("")}${gray("多")}`);
  return rows;
}

/** Longest and current runs of days with any use. */
function streaks(days: Map<string, Sum>, now = Date.now()): { current: number; longest: number } {
  const keys = [...days.keys()].sort();
  let longest = 0;
  let run = 0;
  let prev: number | undefined;
  for (const k of keys) {
    const t = new Date(`${k}T00:00:00`).getTime();
    run = prev !== undefined && Math.round((t - prev) / 86_400_000) === 1 ? run + 1 : 1;
    longest = Math.max(longest, run);
    prev = t;
  }
  let current = 0;
  const d = new Date(startOfDay(now));
  if (!days.has(dayKey(d.getTime()))) d.setDate(d.getDate() - 1); // today not used yet: count up to yesterday
  while (days.has(dayKey(d.getTime()))) {
    current++;
    d.setDate(d.getDate() - 1);
  }
  return { current, longest };
}

// ── the ChatGPT plan ────────────────────────────────────────────────

interface Window {
  used_percent?: number;
  limit_window_seconds?: number;
  reset_at?: number;
}
export interface Plan {
  plan_type?: string;
  rate_limit?: { limit_reached?: boolean; primary_window?: Window | null; secondary_window?: Window | null };
  credits?: { has_credits?: boolean; unlimited?: boolean; balance?: string };
}

/** What the plan has left, from the endpoint Codex's /status reads. Undefined when not signed in. */
export async function codexPlan(doFetch: typeof fetch = fetch): Promise<Plan | undefined> {
  if (!readCodexAuth()) return undefined;
  const t = await codexTokens();
  if (!t?.accountId) return undefined;
  const res = await doFetch("https://chatgpt.com/backend-api/wham/usage", {
    headers: { Authorization: `Bearer ${t.access}`, "chatgpt-account-id": t.accountId, originator: "sasacode" },
    signal: AbortSignal.timeout(10_000),
  });
  if (!res.ok) throw new Error(`ChatGPT returned ${res.status}`);
  return (await res.json()) as Plan;
}

const windowName = (s?: number) => (!s ? "枠" : s === 604_800 ? "週の枠" : s % 86_400 === 0 ? `${s / 86_400}日の枠` : `${Math.round(s / 3600)}時間の枠`);

export function planLines(plan: Plan, width = 80, now = Date.now()): string[] {
  const lines = [bold(`ChatGPT ${plan.plan_type ?? ""}`.trim())];
  const windows = [plan.rate_limit?.primary_window, plan.rate_limit?.secondary_window].filter((w): w is Window => !!w);
  for (const w of windows) {
    const used = w.used_percent ?? 0;
    const color = used >= 90 ? 160 : used >= 70 ? 178 : 34;
    const reset = w.reset_at ? `あと ${fmtWait(w.reset_at * 1000 - now)}でリセット` : "";
    const head = `  ${pad(windowName(w.limit_window_seconds), 12)}${bar(used / 100, Math.max(8, Math.min(20, width - 50)), color)} ${used}% 使用`;
    lines.push(!reset ? head : vis(head) + reset.length * 2 + 3 <= width ? `${head}${gray(` · ${reset}`)}` : `${head}\n${" ".repeat(14)}${gray(reset)}`);
  }
  if (!windows.length) lines.push(gray("  上限の情報がありません"));
  if (plan.rate_limit?.limit_reached) lines.push("  \x1b[31m上限に達しています\x1b[0m");
  if (plan.credits?.has_credits) lines.push(`  ${pad("クレジット", 12)}${plan.credits.unlimited ? "無制限" : (plan.credits.balance ?? "?")}`);
  return lines;
}

// ── plugin ──────────────────────────────────────────────────────────

const SUBCOMMANDS = [
  { value: "graph", label: "graph", description: "日ごとの使用量（Contributions Graph 風）" },
  { value: "history", label: "history", description: "日別の記録（既定: 直近14日）" },
  { value: "models", label: "models", description: "モデル別の合計" },
  { value: "plan", label: "plan", description: "ChatGPT プランの残り" },
];

const usage: Plugin = (api) => {
  const run = { started: Date.now(), sum: empty(), models: new Set<string>() };
  api.on("turn_end", ({ message }) => {
    add(run.sum, record(message));
    run.models.add(`${message.provider}/${message.model}`);
  });
  const width = () => Math.max(40, (process.stdout.columns || 80) - 5);
  const show = (lines: string[]) => api.ui.notify(lines.join("\n"));

  async function overview(): Promise<void> {
    const records = readRecords();
    const today = startOfDay(Date.now());
    const lines = [bold(`この起動（${fmtWait(Date.now() - run.started)}）`), `  ${sumLine(run.sum)}`];
    if (run.models.size) lines.push(gray(`  ${[...run.models].join(", ")}`));
    lines.push("", bold("これまで"));
    for (const [label, t] of [["今日", today], ["直近7日", today - 6 * 86_400_000], ["直近30日", today - 29 * 86_400_000], ["全期間", 0]] as const)
      lines.push(`  ${pad(label, 10)}${sumLine(since(records, t))}`);
    const plan = await codexPlan().catch(() => undefined);
    if (plan) lines.push("", ...planLines(plan, width()));
    lines.push("", gray("/usage graph · /usage history [日数] · /usage models · /usage plan"));
    show(lines);
  }

  function graphView(): void {
    const days = byDay(readRecords());
    const weeks = Math.max(4, Math.min(53, Math.floor((width() - 3) / WEEK)));
    const from = startOfDay(Date.now()) - (weeks * 7 - 1) * 86_400_000;
    const inRange = [...days].filter(([k]) => new Date(`${k}T00:00:00`).getTime() >= from);
    const total = inRange.reduce((s, [, v]) => s + v.tokens, 0);
    const top = inRange.sort((a, b) => b[1].tokens - a[1].tokens)[0];
    const { current, longest } = streaks(days);
    show([
      bold(`過去 ${weeks} 週の使用量`),
      ...graph(days, weeks),
      "",
      `  合計 ${fmtCount(total)} トークン · 使った日 ${inRange.length} 日${top ? ` · 最多 ${top[0]}（${fmtCount(top[1].tokens)}）` : ""}`,
      gray(`  連続 ${current} 日（最長 ${longest} 日）`),
    ]);
  }

  function historyView(arg: string): void {
    const n = Math.max(1, Math.min(365, Number(arg) || 14));
    const days = byDay(readRecords());
    const today = startOfDay(Date.now());
    const rows: [string, Sum][] = [];
    for (let i = n - 1; i >= 0; i--) {
      const t = today - i * 86_400_000;
      rows.push([dayKey(t), days.get(dayKey(t)) ?? empty()]);
    }
    const max = Math.max(1, ...rows.map(([, s]) => s.tokens));
    const barWidth = Math.max(8, Math.min(30, width() - 60));
    const wd = "日月火水木金土";
    show([
      bold(`直近 ${n} 日`),
      ...rows.map(([k, s]) => {
        const day = `${k.slice(5)} ${wd[new Date(`${k}T00:00:00`).getDay()]}`;
        if (!s.requests) return gray(`  ${day}  ${" ".repeat(barWidth)} -`);
        return `  ${day}  ${bar(s.tokens / max, barWidth)} ${fmtCount(s.tokens).padStart(6)} ${gray(`出力 ${fmtCount(s.output)} · ${s.requests} 回${fmtCost(s.cost)}`)}`;
      }),
      "",
      `  合計 ${sumLine(rows.reduce((a, [, s]) => ({ tokens: a.tokens + s.tokens, output: a.output + s.output, requests: a.requests + s.requests, cost: a.cost + s.cost }), empty()))}`,
    ]);
  }

  function modelsView(): void {
    const records = readRecords();
    const models = new Map<string, Sum & { last: number }>();
    for (const r of records) {
      const m = models.get(r.model) ?? { ...empty(), last: 0 };
      add(m, r);
      m.last = r.t;
      models.set(r.model, m);
    }
    const list = [...models].sort((a, b) => b[1].tokens - a[1].tokens);
    const total = Math.max(1, list.reduce((s, [, v]) => s + v.tokens, 0));
    const nameWidth = Math.min(40, Math.max(10, ...list.map(([k]) => k.length)) + 2);
    const row = ([k, s]: [string, Sum & { last: number }]) => {
      const figures = `${bar(s.tokens / total, 12)} ${fmtCount(s.tokens).padStart(6)} ${gray(`${Math.round((s.tokens / total) * 100)}% · ${s.requests} 回${fmtCost(s.cost)} · 最終 ${dayKey(s.last).slice(5)}`)}`;
      const one = `  ${k.padEnd(nameWidth)}${figures}`;
      // Narrow terminals: the name on its own line rather than wrapped through the bar.
      return vis(one) <= width() ? one : `  ${k}\n    ${figures}`;
    };
    show([bold("モデル別（全期間）"), ...(list.length ? list.map(row) : [gray("  記録がありません")])]);
  }

  async function planView(): Promise<void> {
    const plan = await codexPlan();
    show(plan ? planLines(plan, width()) : [gray("ChatGPT プランにログインしていません（sasacode login）。ほかのプロバイダーは残量を返す API がありません")]);
  }

  api.registerCommand({
    name: "usage",
    description: "使用量：この起動・これまでの記録・プランの残り",
    argumentHint: "[graph | history [日数] | models | plan]",
    complete: (prefix) => SUBCOMMANDS.filter((s) => s.value.startsWith(prefix)),
    async run({ args }) {
      const [sub = "", arg = ""] = args.trim().split(/\s+/);
      if (!sub) return overview();
      if (sub === "graph") return graphView();
      if (sub === "history") return historyView(arg);
      if (sub === "models") return modelsView();
      if (sub === "plan") return planView();
      throw new Error(`サブコマンドは ${SUBCOMMANDS.map((s) => s.value).join(" / ")} のいずれかです`);
    },
  });
};
export default usage;
