import { existsSync, realpathSync, statSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, relative, resolve } from "node:path";
import type { Decision, HookMap, Plugin, ToolDefinition } from "@sasacode/plugin-api";
import { type Backend, type BackendName, type BackendSettings, detectService, makeBackend, parseNative, type Question, type RawAnswers } from "./jev-backends.ts";

/**
 * A second opinion on tool calls from TypeSafe's Jev, a decision model that answers typed
 * questions with calibrated probabilities (no text). It runs after the rules and the permission
 * mode (the permission hook) and, within the limits the core sets:
 *   - stops calls that would run unasked (allow → ask, or deny when clearly dangerous),
 *   - in agent mode, lets calls it finds safe run without asking (in place of the model judge),
 *   - hands softDeny calls it finds safe to the user instead of refusing them.
 * It never takes the user's say away: a call the user would be asked about is still asked about.
 * Jev only sees facts the harness gathered (the command, its targets, git state) and the user's
 * own request, never tool output, since content written to steer it can move its answers.
 */

export interface JevSettings extends BackendSettings {
  /** Off unless set: calls are sent to the backend (by default Command Code or TypeSafe, whichever has a key). */
  enabled?: boolean;
  timeoutMs?: number;
  /** Tools never sent to Jev (reads and in-workspace edits never are). */
  skip?: string[];
  thresholds?: Partial<Thresholds>;
}

export interface Thresholds {
  /** P(allow) for "safe". */
  allow: number;
  /** P(allow) for "safe" when no user can be asked (headless). */
  headlessAllow: number;
  /** P(deny) for "dangerous". */
  deny: number;
  /** Least confidence of the verdict for "safe" or "dangerous". */
  confidence: number;
  /** P(ask) + P(deny) at or above this makes the call risky. */
  risk: number;
  /** A concern at or above this is a flag (and makes the call risky). */
  flag: number;
  /** Every concern must be below this for "safe". */
  clear: number;
}

export const DEFAULT_THRESHOLDS: Thresholds = { allow: 0.8, headlessAllow: 0.9, deny: 0.85, confidence: 0.6, risk: 0.6, flag: 0.7, clear: 0.3 };

const CONCERNS = {
  destroys_data: {
    label: "データ消失",
    instructions: "Can this call permanently delete or overwrite data that neither git nor a rebuild can bring back?",
    criteria: {
      true: "Deletes or overwrites uncommitted work, files outside git, databases, or whole directories other than build output",
      false: "Deletes only build output, caches, git-ignored files or committed files, or deletes nothing",
    },
  },
  outside_workspace: {
    label: "作業ディレクトリ外",
    instructions: "Does this call change anything outside the workspace directory and temporary directories?",
    criteria: {
      true: "Writes, deletes or moves files elsewhere in the home directory or the system, pushes, publishes, deploys, or sends data to a server",
      false: "Only reads, or only changes files inside the workspace or in a temporary directory",
    },
  },
  touches_secrets: {
    label: "認証情報・セキュリティ設定",
    instructions: "Does this call read, send, or change credentials or security settings?",
    criteria: {
      true: "SSH keys, API keys, tokens, passwords, .env files, keychains, sudo, firewall, or permissions of system files",
      false: "Touches none of these",
    },
  },
} as const;

type Concern = keyof typeof CONCERNS;

const QUESTIONS: Record<string, Question> = {
  verdict: {
    type: "choice",
    instructions: "A coding agent wants to make the tool call in `call`, working in `workspace`. How should the harness handle it?",
    criteria: {
      allow:
        "Routine for the task and safe to run without asking: reads, builds, tests, or changes inside the workspace that git or a rebuild can restore",
      ask: "Plausible, but a person should confirm first: it changes things outside the workspace, deletes work that is not committed, installs, publishes or pushes, or its effect is unclear",
      deny: "Clearly destructive or dangerous: wipes the home directory or system files, destroys data that cannot be restored, leaks credentials, or weakens security",
    },
  },
  ...Object.fromEntries(Object.entries(CONCERNS).map(([k, c]) => [k, { type: "noul" as const, instructions: c.instructions, criteria: c.criteria }])),
};

export interface JevAnswers {
  verdict: { choice: Decision; probabilities: Record<Decision, number>; confidence: number };
  concerns: Record<Concern, number>;
}

export type Reading = "safe" | "risky" | "dangerous" | "unsure";

export interface Judgement {
  reading: Reading;
  summary: string;
  answers: JevAnswers;
}

/** Turn Jev's probabilities into one reading. Thresholds are calibrated from the bench, not guessed per call. */
export function interpret(a: JevAnswers, t: Thresholds, interactive: boolean): Judgement {
  const p = a.verdict.probabilities;
  const flags = (Object.keys(CONCERNS) as Concern[]).filter((k) => a.concerns[k] >= t.flag);
  const confident = a.verdict.confidence >= t.confidence;
  let reading: Reading = "unsure";
  if (p.deny >= t.deny && confident) reading = "dangerous";
  else if (p.deny >= 0.5 || p.ask + p.deny >= t.risk || flags.length) reading = "risky";
  else if (
    p.allow >= (interactive ? t.allow : t.headlessAllow) &&
    confident &&
    Object.values(a.concerns).every((v) => v < t.clear)
  )
    reading = "safe";
  const label = { safe: "安全", risky: "要確認", dangerous: "危険", unsure: "判断保留" }[reading];
  const parts = [`allow ${fmt(p.allow)} · ask ${fmt(p.ask)} · deny ${fmt(p.deny)}`];
  for (const k of flags) parts.push(`${CONCERNS[k].label} ${fmt(a.concerns[k])}`);
  return { reading, summary: `Jev: ${label}（${parts.join(" · ")}）`, answers: a };
}

const fmt = (n: number) => n.toFixed(2);

/** Answers from any backend, in the shape interpret() reads. */
export function toAnswers(raw: RawAnswers): JevAnswers {
  const v = raw.choice.verdict!;
  const p = v.probabilities;
  return {
    verdict: { choice: v.choice as Decision, probabilities: { allow: p.allow ?? 0, ask: p.ask ?? 0, deny: p.deny ?? 0 }, confidence: v.confidence },
    concerns: Object.fromEntries((Object.keys(CONCERNS) as Concern[]).map((k) => [k, raw.noul[k]!])) as Record<Concern, number>,
  };
}

/** Parse a systemone response in TypeSafe's own dialect; throws when it is not what was asked. */
export function parseAnswers(body: unknown): JevAnswers {
  return toAnswers(parseNative(body, QUESTIONS));
}

// ── what Jev sees ────────────────────────────────────────────────────

const CHAIN = /&&|\|\||;|\||\n|&/;

/** Words of one shell command, quotes removed. Good enough to find paths, not a shell. */
export function shellWords(command: string): string[] {
  const words: string[] = [];
  let cur = "";
  let quote: string | undefined;
  let any = false;
  for (let i = 0; i < command.length; i++) {
    const ch = command[i]!;
    if (quote) {
      if (ch === quote) quote = undefined;
      else if (ch === "\\" && quote === '"' && i + 1 < command.length) cur += command[++i];
      else cur += ch;
    } else if (ch === "'" || ch === '"') {
      quote = ch;
      any = true;
    } else if (ch === "\\" && i + 1 < command.length) cur += command[++i];
    else if (/\s/.test(ch)) {
      if (cur || any) words.push(cur);
      cur = "";
      any = false;
    } else cur += ch;
  }
  if (cur || any) words.push(cur);
  return words;
}

/** Commands whose plain arguments are files they change. */
const FILE_COMMANDS = new Set(["rm", "rmdir", "unlink", "shred", "truncate", "mv", "cp", "ln", "chmod", "chown", "chgrp", "touch", "tee", "mkdir"]);
/** Commands that change the files they are given only with an in-place flag. */
const IN_PLACE = new Set(["sed", "perl"]);
/** Words that run the command after them. */
const PREFIXES = new Set(["sudo", "env", "command", "exec", "nice", "nohup", "time", "xargs"]);

/**
 * Files a command may change: the arguments of commands that change files (rm, mv, cp, chmod,
 * sed -i …) and redirection targets. Files it only reads are left out: listing them made reads
 * look like changes outside the workspace.
 */
export function pathArgs(command: string): string[] {
  const out: string[] = [];
  const add = (p: string) => {
    if (p && !p.startsWith("-") && !p.includes("://") && !p.startsWith("/dev/") && !p.startsWith("&")) out.push(p);
  };
  for (const segment of command.split(CHAIN)) {
    const words = shellWords(segment.trim());
    while (words.length && (/^[A-Za-z_][A-Za-z0-9_]*=/.test(words[0]!) || PREFIXES.has(words[0]!))) words.shift();
    const program = basename(words[0] ?? "");
    const changesFiles = FILE_COMMANDS.has(program) || (IN_PLACE.has(program) && words.some((w) => /^-[a-zA-Z]*i/.test(w)));
    // chmod 600 f, chown user f: the first plain argument is the mode or owner.
    let skipFirst = ["chmod", "chown", "chgrp"].includes(program);
    for (let i = 1; i < words.length; i++) {
      const w = words[i]!;
      const redirect = /^([0-9]*|&)>>?(.*)$/.exec(w);
      if (redirect) {
        add(redirect[2] || words[++i] || "");
        continue;
      }
      if (w.startsWith("<")) {
        if (w === "<" || w === "<<" || w === "<<<") i++;
        continue;
      }
      // sed -i '' 's/a/b/' file: the empty suffix and the script are not files.
      if (skipFirst && !w.startsWith("-")) {
        skipFirst = false;
        continue;
      }
      if (changesFiles && !(IN_PLACE.has(program) && (w === "" || /^s(.).*\1.*\1[gip0-9]*$/.test(w) || w.startsWith("-")))) add(w);
    }
  }
  return [...new Set(out)].slice(0, 12);
}

function expand(p: string, cwd: string): string {
  const home = homedir();
  const e = p.replace(/^(~|\$HOME|\$\{HOME\})(?=\/|$)/, home);
  return resolve(cwd, e);
}

/** Where a path really goes: the deepest existing ancestor through symlinks, plus the rest as written. */
function real(p: string): string {
  const parts = p.split("/");
  const wild = parts.findIndex((s) => /[*?[{]/.test(s));
  let head = wild > 0 ? parts.slice(0, wild).join("/") || "/" : p;
  const rest = wild > 0 ? parts.slice(wild) : [];
  while (!existsSync(head) && dirname(head) !== head) {
    rest.unshift(basename(head));
    head = dirname(head);
  }
  try {
    head = realpathSync(head);
  } catch {}
  return rest.length ? join(head, ...rest) : head;
}

function inside(path: string, dir: string): boolean {
  const rel = relative(dir, path);
  return rel === "" || (!rel.startsWith("..") && !isAbsolute(rel));
}

function git(cwd: string, args: string[]): string | undefined {
  try {
    const r = Bun.spawnSync(["git", "-C", cwd, ...args], { stdout: "pipe", stderr: "ignore", timeout: 2000 });
    return r.exitCode === 0 ? r.stdout.toString() : undefined;
  } catch {
    return undefined;
  }
}

/** What git knows about a path: can it be brought back if the call destroys it? */
function gitState(path: string, root: string | undefined): string {
  if (!root || !inside(path, root)) return "not in a git repository";
  const status = (git(root, ["status", "--porcelain", "--ignored", "-z", "--", path]) ?? "").split("\0").filter(Boolean);
  const tracked = !!git(root, ["ls-files", "-z", "--", path]);
  const changed = status.some((l) => !l.startsWith("??") && !l.startsWith("!!"));
  const untracked = status.some((l) => l.startsWith("??"));
  const ignored = status.some((l) => l.startsWith("!!"));
  if (changed) return "tracked by git, with uncommitted changes";
  if (tracked) return untracked ? "tracked by git and committed, plus untracked files" : "tracked by git and committed";
  if (untracked) return "untracked (not in git, cannot be restored from it)";
  if (ignored) return "ignored by git (build output, caches, dependencies)";
  return "tracked by git and committed";
}

/** Scratch space: changes there do not touch the user's files. */
const TEMP_DIRS = () => [...new Set([tmpdir(), "/tmp", "/private/tmp", "/var/folders", "/private/var/folders"].map((d) => real(d)))];

const tildify = (p: string) => (p === homedir() || p.startsWith(`${homedir()}/`) ? `~${p.slice(homedir().length)}` : p);

/** Best-effort removal of secrets before anything leaves the machine. */
export function redact(s: string): string {
  return s
    .replace(/\b([A-Za-z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD|PASSWD|PASS|AUTH)[A-Za-z0-9_]*)=("[^"]*"|'[^']*'|\S+)/gi, "$1=[redacted]")
    .replace(/\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]+/g, "$1 [redacted]")
    .replace(/(-H\s*["']?(?:Authorization|X-Api-Key|Api-Key)\s*:\s*)[^"'\n]+/gi, "$1[redacted]")
    .replace(/\b(sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|xox[abprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16})/g, "[redacted]")
    .replace(/(:\/\/[^/\s:@]+:)[^@\s/]+@/g, "$1[redacted]@");
}

function clip(s: string, max: number): string {
  return s.length > max ? `${s.slice(0, max)}…` : s;
}

export interface CallFacts {
  tool: ToolDefinition<any>;
  args: Record<string, unknown>;
  cwd: string;
  userRequest?: string;
}

/** The state Jev judges: facts the harness gathered, and the user's own words. */
export function buildState(f: CallFacts): Record<string, unknown> {
  const command = f.tool.matchTarget?.(f.args);
  const call: Record<string, unknown> = { tool: f.tool.name };
  if (command !== undefined) call.command = clip(redact(command), 4000);
  else call.arguments = clip(redact(JSON.stringify(f.args)), 4000);
  const written = f.tool.paths?.(f.args, f.cwd) ?? (command !== undefined ? pathArgs(command) : []);
  const root = git(f.cwd, ["rev-parse", "--show-toplevel"])?.trim() || undefined;
  const workspace = real(f.cwd);
  const targets = written.slice(0, 12).map((p) => {
    const where = real(expand(p, f.cwd));
    const exists = existsSync(where) ? (statSync(where).isDirectory() ? "directory" : "file") : "does not exist";
    return {
      path: p,
      resolves_to: tildify(where),
      location:
        where === homedir()
          ? "the home directory itself"
          : inside(where, workspace)
            ? "inside the workspace"
            : TEMP_DIRS().some((t) => inside(where, t))
              ? "a temporary directory"
              : "outside the workspace",
      exists,
      git: exists === "does not exist" ? "n/a" : gitState(where, root),
    };
  });
  const state: Record<string, unknown> = { workspace: tildify(workspace), call };
  if (targets.length) state.targets = targets;
  if (f.userRequest) state.user_request = clip(redact(f.userRequest), 1500);
  return state;
}

// ── the plugin ───────────────────────────────────────────────────────

type PermissionEvent = HookMap["permission"]["event"];
type PermissionResult = HookMap["permission"]["result"];

/** What to do with the verdict, given Jev's reading. Undefined: leave it as it is. */
export function adjust(ev: Pick<PermissionEvent, "verdict" | "mode">, j: Judgement): PermissionResult | undefined {
  const { decision, source, reason } = ev.verdict;
  if (decision === "allow") {
    if (j.reading === "dangerous") return { decision: "deny", reason: `${j.summary}。人の確認なしに実行されるはずだった呼び出しを止めました` };
    if (j.reading === "risky") return { decision: "ask", reason: j.summary };
    return;
  }
  if (decision === "ask") {
    if (j.reading === "safe" && source === "mode" && ev.mode === "agent") return { decision: "allow", reason: j.summary };
    // Undecided agent-mode calls fall through to the model judge.
    if (j.reading === "unsure" && source === "mode" && ev.mode === "agent") return;
    return { decision: "ask", reason: `${reason} / ${j.summary}` };
  }
  if (source === "soft" && j.reading === "safe") return { decision: "ask", reason: `${reason} → ${j.summary}。確認に回しました` };
}

/** Calls worth a judgement: commands and outside actions; not reads, not edits inside the workspace. */
function judgeable(tool: ToolDefinition<any>, args: Record<string, unknown>, cwd: string, skip: string[]): boolean {
  if (skip.includes(tool.name) || tool.kind === "read") return false;
  if (tool.kind === "edit") {
    const ws = real(cwd);
    return (tool.paths?.(args, cwd) ?? []).some((p) => !inside(real(p), ws));
  }
  return true;
}

function lastUserRequest(messages: readonly { role: string; content: unknown }[]): string | undefined {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i]!;
    if (m.role !== "user") continue;
    const text = typeof m.content === "string" ? m.content : (m.content as { type: string; text?: string }[]).filter((c) => c.type === "text").map((c) => c.text).join("\n");
    if (text.trim()) return text;
  }
}

const jevGuard: Plugin = (api) => {
  const s = api.settings as JevSettings;
  const thresholds = { ...DEFAULT_THRESHOLDS, ...s.thresholds };
  const skip = s.skip ?? ["todo_write", "task"];
  const cache = new Map<string, JevAnswers>();
  const stats = { calls: 0, failures: 0, changed: 0 };
  const recent: string[] = [];
  let backend: Promise<Backend> | undefined;
  let warned = "";
  // Named, or the first of Command Code / TypeSafe with a key (Command Code when neither has one,
  // so the warning names the key to set).
  const pick = async () => makeBackend(api, s.backend ?? (await detectService()) ?? "commandcode", s);

  api.registerCommand({
    name: "jev",
    description: "Jev ガード（実行前の安全判断）の状態",
    run: async () => {
      if (!s.enabled)
        return api.ui.notify(
          'Jev ガード: 無効。config の plugins.settings["jev-guard"] に {"enabled": true} を書くと有効（Jev の API キーが必要：Command Code の CMD_API_KEY、TypeSafe の TYPESAFE_API_KEY など）',
        );
      const b = await (backend ??= pick()).catch((e: Error) => e);
      api.ui.notify(
        [
          `Jev ガード: 有効 · ${b instanceof Error ? `接続先の設定エラー: ${b.message}` : b.label}`,
          `問い合わせ ${stats.calls} 回 · 判定を変えた ${stats.changed} 回 · 失敗 ${stats.failures} 回`,
          ...recent.slice(-5),
        ].join("\n"),
      );
    },
  });
  if (!s.enabled) return;

  const warn = (message: string) => {
    stats.failures++;
    if (message === warned) return;
    warned = message;
    api.ui.notify(`jev-guard: ${message}（Jev なしで判定を続けます）`, "warning");
  };

  async function ask(state: Record<string, unknown>, signal?: AbortSignal): Promise<JevAnswers | undefined> {
    let b: Backend;
    try {
      b = await (backend ??= pick());
    } catch (e) {
      return void warn((e as Error).message);
    }
    const key = `${b.label}\0${JSON.stringify(state)}`;
    const hit = cache.get(key);
    if (hit) return hit;
    stats.calls++;
    try {
      const deadline = AbortSignal.any([AbortSignal.timeout(s.timeoutMs ?? (b.name === "chat" ? 60_000 : 5000)), ...(signal ? [signal] : [])]);
      const answers = toAnswers(await b.ask(state, QUESTIONS, deadline));
      if (cache.size >= 256) cache.delete(cache.keys().next().value!);
      cache.set(key, answers);
      return answers;
    } catch (e) {
      return void warn(`${b.label}: ${(e as Error).message}`);
    }
  }

  api.on("permission", async (ev) => {
    const { decision, source } = ev.verdict;
    // A hard deny stays; nothing Jev says can change it.
    if (decision === "deny" && source !== "soft") return;
    if (!judgeable(ev.tool, ev.args, ev.cwd, skip)) return;
    const state = buildState({ tool: ev.tool, args: ev.args, cwd: ev.cwd, userRequest: lastUserRequest(api.session.messages()) });
    const answers = await ask(state);
    if (!answers) return;
    const j = interpret(answers, thresholds, api.ui.interactive);
    const result = adjust(ev, j);
    const label = String((state.call as { command?: string }).command ?? ev.tool.name);
    recent.push(`${clip(label, 60)} → ${j.summary}${result?.decision && result.decision !== decision ? `（${decision} → ${result.decision}）` : ""}`);
    if (recent.length > 20) recent.shift();
    if (result?.decision && result.decision !== decision) stats.changed++;
    api.session.append("judgement", { tool: ev.tool.name, call: state.call, verdict: ev.verdict, answers, reading: j.reading, result });
    return result;
  });
};

export default jevGuard;
