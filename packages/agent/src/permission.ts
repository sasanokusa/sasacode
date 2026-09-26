import { lstatSync, readlinkSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
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
  /** Denied like `deny`, but a permission hook may hand the call to the user instead (never run it unasked). */
  softDeny?: string[];
}

export interface PermissionCheck {
  tool: ToolDefinition<any>;
  args: Record<string, unknown>;
  cwd: string;
}

export interface Verdict {
  decision: Decision;
  reason: string;
  /**
   * "rule": the user's (or a plugin's) explicit allow/ask/deny rule; "soft": a softDeny rule;
   * "mode": the permission mode's default.
   */
  source: "rule" | "soft" | "mode";
  /** Agent mode, checked without a judge: the model still has to judge this call (see judged()). */
  needsJudge?: boolean;
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
  private rules: Record<Decision | "softDeny", ParsedRule[]> = { allow: [], ask: [], deny: [], softDeny: [] };

  constructor(mode: PermissionMode = "edits", rules: PermissionRules = {}) {
    this.mode = mode;
    this.addRules(rules);
  }

  addRules(rules: PermissionRules): void {
    for (const d of ["allow", "ask", "deny", "softDeny"] as const) for (const r of rules[d] ?? []) this.rules[d].push(parseRule(r));
  }

  /** Without a judge, an agent-mode call comes back as "ask" with `needsJudge` set. */
  async check(c: PermissionCheck, judge?: Judge): Promise<Verdict> {
    const denied = this.rules.deny.find((r) => ruleMatches(r, c));
    if (denied) return { decision: "deny", reason: `rule deny: ${denied.source}`, source: "rule" };
    const soft = this.rules.softDeny.find((r) => ruleMatches(r, c));
    if (soft) return { decision: "deny", reason: `rule softDeny: ${soft.source}`, source: "soft" };
    const asked = this.rules.ask.find((r) => ruleMatches(r, c));
    if (asked) return { decision: "ask", reason: `rule ask: ${asked.source}`, source: "rule" };
    // Where a path really goes cannot be told (a link loop, no permission): let the user decide.
    const unclear = (c.tool.paths?.(c.args, c.cwd) ?? []).find((p) => tryRealPath(p) === undefined);
    if (unclear) return { decision: "ask", reason: `cannot resolve ${unclear}`, source: "rule" };
    if (allowedByRules(this.rules.allow, c)) return { decision: "allow", reason: "allow rule", source: "rule" };
    return { ...(await this.modeDecision(c, judge)), source: "mode" };
  }

  /** The agent-mode judgement that check() left open (`needsJudge`). */
  async judged(c: PermissionCheck, judge: Judge): Promise<Verdict> {
    return { ...(await askJudge(c, judge)), source: "mode" };
  }

  private async modeDecision(c: PermissionCheck, judge?: Judge): Promise<Omit<Verdict, "source">> {
    // Through a symlink, a path inside the project can point anywhere: judge where it really goes.
    const insideCwd = (c.tool.paths?.(c.args, c.cwd) ?? []).every((p) => isInside(tryRealPath(p) ?? p, tryRealPath(c.cwd) ?? c.cwd));
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
        if (!judge) return { decision: "ask", reason: "mode agent: not judged yet", needsJudge: true };
        return askJudge(c, judge);
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

async function askJudge(c: PermissionCheck, judge: Judge): Promise<Omit<Verdict, "source">> {
  try {
    const j = await judge(c);
    return { decision: j.safe ? "allow" : "ask", reason: `agent judged ${j.safe ? "safe" : "risky"}: ${j.reason}` };
  } catch (e) {
    return { decision: "ask", reason: `judge failed: ${(e as Error).message}` };
  }
}

/** A rule written for this tool, or for the tool whose rules it takes on (permissionsAs). */
function covers(rule: ParsedRule, tool: ToolDefinition<any>): boolean {
  return rule.tool.test(tool.name) || (!!tool.permissionsAs && rule.tool.test(tool.permissionsAs));
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

/**
 * deny/ask rules match when any spelling of a path does (as written, or with symlinks resolved);
 * allow rules only when every spelling does, so a symlink cannot smuggle a path past either.
 */
function ruleMatches(rule: ParsedRule, c: PermissionCheck, forAllow = false): boolean {
  if (!covers(rule, c.tool)) return false;
  if (rule.pattern === undefined) return true;
  const target = c.tool.matchTarget?.(c.args);
  if (target !== undefined) {
    const re = wildcard(rule.pattern);
    return re.test(target) || segments(target).some((s) => re.test(s));
  }
  const paths = c.tool.paths?.(c.args, c.cwd) ?? [];
  if (!paths.length) return false;
  // Compare like with like. A relative pattern is anchored at the working directory's real path,
  // but links inside the project are not followed (a repo could point `src` at ~/.ssh). An
  // absolute pattern is the user's own path: its fixed part is resolved (/var → /private/var).
  const pattern = rule.pattern;
  const variants = isAbsolute(pattern) ? [pattern, realPattern(pattern)] : [resolve(c.cwd, pattern), resolve(tryRealPath(c.cwd) ?? c.cwd, pattern)];
  const globs = [...new Set(variants)].map((g) => new Bun.Glob(g));
  const hit = (p: string) => globs.some((g) => g.match(p));
  const spellings = paths.flatMap((p) => [p, tryRealPath(p) ?? p]);
  return forAllow ? spellings.every(hit) : spellings.some(hit);
}

/** A glob with the directories before its first wildcard resolved through symlinks. */
function realPattern(glob: string): string {
  const parts = glob.split("/");
  const firstWild = parts.findIndex((s) => /[*?[{]/.test(s));
  if (firstWild <= 0) return tryRealPath(glob) ?? glob;
  const fixed = parts.slice(0, firstWild).join("/") || "/";
  return [tryRealPath(fixed) ?? fixed, ...parts.slice(firstWild)].join("/");
}

/**
 * `p` with symlinks resolved, walked one component at a time as the OS does: a link's target is
 * put in front of the rest of the path before any ".." that follows is applied (so `pivot/..`
 * inside a link goes up from where `pivot` really points). Parts that do not exist yet are kept as
 * written, and so is a link whose target does not exist yet, followed to that target (writing
 * through it would create it). Throws when the path cannot be resolved safely (a link loop, no
 * permission, ".." below a part that does not exist).
 */
export function realPath(p: string): string {
  const todo = p.split("/").filter(Boolean);
  let done = "/";
  let absent = false;
  let hops = 0;
  while (todo.length) {
    const part = todo.shift()!;
    if (part === ".") continue;
    if (part === "..") {
      // The OS would fail here, but the folder may be created in between: do not guess.
      if (absent) throw new Error(`cannot resolve ${p}: ".." below a path that does not exist`);
      done = dirname(done);
      continue;
    }
    const next = join(done, part);
    if (!absent) {
      let link: string | undefined;
      try {
        const st = lstatSync(next);
        if (st.isSymbolicLink()) link = readlinkSync(next);
      } catch (e) {
        if (!missing(e)) throw e;
        absent = true;
      }
      if (link !== undefined) {
        if (++hops > 40) throw new Error(`too many symlinks: ${p}`);
        todo.unshift(...link.split("/").filter(Boolean));
        if (isAbsolute(link)) done = "/";
        continue;
      }
    }
    done = next;
  }
  return done;
}

function missing(e: unknown): boolean {
  return (e as NodeJS.ErrnoException).code === "ENOENT";
}

function tryRealPath(p: string): string | undefined {
  try {
    return realPath(p);
  } catch {
    return undefined;
  }
}

function segments(command: string): string[] {
  return command
    .split(CHAIN)
    .map((s) => s.trim())
    .filter(Boolean);
}

/** For commands, every chained piece must be covered by some allow rule, and substitutions never are. */
function allowedByRules(rules: ParsedRule[], c: PermissionCheck): boolean {
  const mine = rules.filter((r) => covers(r, c.tool));
  if (!mine.length) return false;
  if (mine.some((r) => r.pattern === undefined)) return true;
  const target = c.tool.matchTarget?.(c.args);
  if (target === undefined) return mine.some((r) => ruleMatches(r, c, true));
  if (SUBSTITUTION.test(target)) return false;
  return segments(target).every((s) => mine.some((r) => wildcard(r.pattern!).test(s)));
}

export function isInside(path: string, dir: string): boolean {
  const rel = relative(dir, path);
  return rel === "" || (!rel.startsWith("..") && !isAbsolute(rel));
}
