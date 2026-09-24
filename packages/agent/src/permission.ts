import { isAbsolute, relative, resolve } from "node:path";
import type { ToolDefinition } from "@sasacode/plugin-api";

/**
 * auto    — allow everything
 * agent   — the model judges risk; safe calls run, others ask the user
 * ask     — ask for every call, reads included
 * edits   — read/write/edit inside the working directory run, everything else asks (default)
 */
export type PermissionMode = "auto" | "agent" | "ask" | "edits";
export const PERMISSION_MODES: PermissionMode[] = ["edits", "ask", "agent", "auto"];
export const PERMISSION_MODE_LABELS: Record<PermissionMode, string> = {
  edits: "読み取り・編集のみ許可",
  ask: "ユーザー確認",
  agent: "エージェント判断",
  auto: "自動許可",
};

export type Decision = "allow" | "ask" | "deny";

/** Rules look like `bash`, `bash(git status*)`, `edit(src/**)`, `mcp__github__*`. */
export interface PermissionRules {
  allow?: string[];
  ask?: string[];
  deny?: string[];
}

export interface PermissionCheck {
  tool: ToolDefinition<any>;
  args: Record<string, unknown>;
  cwd: string;
}

export interface Verdict {
  decision: Decision;
  reason: string;
  /** "rule": the user's (or a plugin's) explicit allow/ask/deny rule; "mode": the permission mode's default. */
  source: "rule" | "mode";
}

/** Asks the model whether a call is safe. Used by the "agent" mode. */
export type Judge = (check: PermissionCheck) => Promise<{ safe: boolean; reason: string }>;

interface ParsedRule {
  tool: RegExp;
  pattern?: string;
  source: string;
}

export class PermissionPolicy {
  mode: PermissionMode;
  private rules: Record<Decision, ParsedRule[]> = { allow: [], ask: [], deny: [] };

  constructor(mode: PermissionMode = "edits", rules: PermissionRules = {}) {
    this.mode = mode;
    this.addRules(rules);
  }

  addRules(rules: PermissionRules): void {
    for (const d of ["allow", "ask", "deny"] as const) for (const r of rules[d] ?? []) this.rules[d].push(parseRule(r));
  }

  async check(c: PermissionCheck, judge?: Judge): Promise<Verdict> {
    for (const d of ["deny", "ask"] as const) {
      const hit = this.rules[d].find((r) => ruleMatches(r, c));
      if (hit) return { decision: d, reason: `rule ${d}: ${hit.source}`, source: "rule" };
    }
    if (allowedByRules(this.rules.allow, c)) return { decision: "allow", reason: "allow rule", source: "rule" };
    return { ...(await this.modeDecision(c, judge)), source: "mode" };
  }

  private async modeDecision(c: PermissionCheck, judge?: Judge): Promise<Omit<Verdict, "source">> {
    const insideCwd = (c.tool.paths?.(c.args, c.cwd) ?? []).every((p) => isInside(p, c.cwd));
    const hasPaths = !!c.tool.paths;
    switch (this.mode) {
      case "auto":
        return { decision: "allow", reason: "mode auto" };
      case "ask":
        return { decision: "ask", reason: "mode ask" };
      case "edits":
        if ((c.tool.kind === "read" || c.tool.kind === "edit") && hasPaths && insideCwd)
          return { decision: "allow", reason: "mode edits: inside working directory" };
        return { decision: "ask", reason: "mode edits" };
      case "agent": {
        if (c.tool.kind === "read" && hasPaths && insideCwd) return { decision: "allow", reason: "read inside working directory" };
        if (!judge) return { decision: "ask", reason: "no judge available" };
        try {
          const j = await judge(c);
          return { decision: j.safe ? "allow" : "ask", reason: `agent judged ${j.safe ? "safe" : "risky"}: ${j.reason}` };
        } catch (e) {
          return { decision: "ask", reason: `judge failed: ${(e as Error).message}` };
        }
      }
    }
  }

  /** Rule that "always allow" in the approval dialog adds for this call. */
  static ruleFor(c: PermissionCheck): string {
    const target = c.tool.matchTarget?.(c.args);
    if (target !== undefined) return `${c.tool.name}(${target})`;
    return c.tool.name;
  }
}

function parseRule(source: string): ParsedRule {
  const m = /^([^()]+?)(?:\((.*)\))?$/s.exec(source.trim());
  if (!m) throw new Error(`invalid permission rule: ${source}`);
  return { tool: wildcard(m[1]!), pattern: m[2], source };
}

function wildcard(p: string): RegExp {
  return new RegExp(`^${p.replace(/[.+?^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*")}$`, "s");
}

// Shell operators that chain commands; an allow rule must match every piece.
const CHAIN = /&&|\|\||;|\||\n|&/;
// Substitutions and output redirection can do anything, so allow rules never cover them.
const SUBSTITUTION = /\$\(|`|<\(|>/;

function ruleMatches(rule: ParsedRule, c: PermissionCheck): boolean {
  if (!rule.tool.test(c.tool.name)) return false;
  if (rule.pattern === undefined) return true;
  const target = c.tool.matchTarget?.(c.args);
  if (target !== undefined) {
    const re = wildcard(rule.pattern);
    return re.test(target) || segments(target).some((s) => re.test(s));
  }
  const paths = c.tool.paths?.(c.args, c.cwd) ?? [];
  if (!paths.length) return false;
  const glob = new Bun.Glob(isAbsolute(rule.pattern) ? rule.pattern : resolve(c.cwd, rule.pattern));
  return paths.every((p) => glob.match(p));
}

function segments(command: string): string[] {
  return command
    .split(CHAIN)
    .map((s) => s.trim())
    .filter(Boolean);
}

/** For commands, every chained piece must be covered by some allow rule, and substitutions never are. */
function allowedByRules(rules: ParsedRule[], c: PermissionCheck): boolean {
  const mine = rules.filter((r) => r.tool.test(c.tool.name));
  if (!mine.length) return false;
  if (mine.some((r) => r.pattern === undefined)) return true;
  const target = c.tool.matchTarget?.(c.args);
  if (target === undefined) return mine.some((r) => ruleMatches(r, c));
  if (SUBSTITUTION.test(target)) return false;
  return segments(target).every((s) => mine.some((r) => wildcard(r.pattern!).test(s)));
}

export function isInside(path: string, dir: string): boolean {
  const rel = relative(dir, path);
  return rel === "" || (!rel.startsWith("..") && !isAbsolute(rel));
}
