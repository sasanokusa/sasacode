import { visibleWidth } from "@earendil-works/pi-tui";
import type { AssistantMessage } from "@sasacode/ai";
import { c } from "./theme.ts";

/** Tokens and cost of everything sent since sasacode started, across /clear and /resume. */
export class UsageTally {
  requests = 0;
  input = 0;
  output = 0;
  cacheRead = 0;
  cacheWrite = 0;
  cost = 0;
  readonly models = new Set<string>();
  readonly startedAt = Date.now();

  add(m: AssistantMessage): void {
    this.requests++;
    this.input += m.usage.input;
    this.output += m.usage.output;
    this.cacheRead += m.usage.cacheRead;
    this.cacheWrite += m.usage.cacheWrite;
    this.cost += m.usage.cost;
    this.models.add(`${m.provider}/${m.model}`);
  }

  /** Share of prompt tokens served from the cache. */
  get hitRate(): number {
    const prompt = this.input + this.cacheRead + this.cacheWrite;
    return prompt ? this.cacheRead / prompt : 0;
  }
}

export function fmtTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
  return `${(n / 1_000_000).toFixed(2)}M`;
}

export function fmtDuration(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}秒`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}分${s % 60}秒`;
  return `${Math.floor(m / 60)}時間${m % 60}分`;
}

/** A gray label padded to 12 columns (by display width: Japanese labels are two columns a character). */
const label = (s: string) => c.gray(s + " ".repeat(Math.max(1, 12 - visibleWidth(s))));

/** The lines for /usage and the exit summary. */
export function usageLines(t: UsageTally): string[] {
  const tokens = [
    `入力 ${fmtTokens(t.input)}`,
    `出力 ${fmtTokens(t.output)}`,
    `キャッシュ読込 ${fmtTokens(t.cacheRead)}${t.cacheRead ? `（${Math.round(t.hitRate * 100)}%）` : ""}`,
    ...(t.cacheWrite ? [`キャッシュ書込 ${fmtTokens(t.cacheWrite)}`] : []),
  ].join(c.gray(" · "));
  const lines = [`${label("トークン")}${tokens}`];
  lines.push(`${label("リクエスト")}${t.requests} 回${t.cost > 0 ? c.gray(" · ") + `$${t.cost.toFixed(3)}` : ""}`);
  if (t.models.size) lines.push(`${label("モデル")}${[...t.models].join(", ")}`);
  return lines;
}

/** Printed on the normal screen after the TUI closes: what was used and how to come back. */
export function exitSummary(t: UsageTally, sessionId: string | undefined): string {
  const head = `${c.bold("sasacode")} ${c.gray(`を終了しました · ${fmtDuration(Date.now() - t.startedAt)}`)}`;
  const lines = [head, ...(t.requests ? usageLines(t) : [])];
  if (sessionId) lines.push(`${label("再開")}${c.cyan(`sasacode -r ${sessionId}`)}`);
  return `${lines.map((l, i) => (i ? `  ${l}` : l)).join("\n")}\n`;
}
