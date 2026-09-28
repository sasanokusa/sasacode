import { visibleWidth } from "@earendil-works/pi-tui";
import { type AssistantMessage, fmtTokens } from "@sasacode/ai";
import { t } from "@sasacode/host";
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

export function fmtDuration(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 60) return t("{s}秒", { s });
  const m = Math.floor(s / 60);
  if (m < 60) return t("{m}分{s}秒", { m, s: s % 60 });
  return t("{h}時間{m}分", { h: Math.floor(m / 60), m: m % 60 });
}

/** A gray label padded to 12 columns (by display width: Japanese labels are two columns a character). */
const label = (s: string) => c.gray(s + " ".repeat(Math.max(1, 12 - visibleWidth(s))));

/** The lines for /usage and the exit summary. */
export function usageLines(u: UsageTally): string[] {
  const tokens = [
    t("入力 {n}", { n: fmtTokens(u.input) }),
    t("出力 {n}", { n: fmtTokens(u.output) }),
    t("キャッシュ読込 {n}", { n: fmtTokens(u.cacheRead) }) + (u.cacheRead ? `（${Math.round(u.hitRate * 100)}%）` : ""),
    ...(u.cacheWrite ? [t("キャッシュ書込 {n}", { n: fmtTokens(u.cacheWrite) })] : []),
  ].join(c.gray(" · "));
  const lines = [`${label(t("トークン"))}${tokens}`];
  lines.push(`${label(t("リクエスト"))}${t("{n} 回", { n: u.requests })}${u.cost > 0 ? c.gray(" · ") + `$${u.cost.toFixed(3)}` : ""}`);
  if (u.models.size) lines.push(`${label(t("モデル"))}${[...u.models].join(", ")}`);
  return lines;
}

/** Printed on the normal screen after the TUI closes: what was used and how to come back. */
export function exitSummary(u: UsageTally, sessionId: string | undefined): string {
  const head = `${c.bold("sasacode")} ${c.gray(t("を終了しました · {time}", { time: fmtDuration(Date.now() - u.startedAt) }))}`;
  const lines = [head, ...(u.requests ? usageLines(u) : [])];
  if (sessionId) lines.push(`${label(t("再開"))}${c.cyan(`sasacode -r ${sessionId}`)}`);
  return `${lines.map((l, i) => (i ? `  ${l}` : l)).join("\n")}\n`;
}
