import { existsSync, realpathSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, isAbsolute, join, relative, resolve } from "node:path";
import type { Decision, HookMap, Plugin, ToolDefinition } from "@sasacode/plugin-api";

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

export const JEV_ENDPOINT = "https://api.commandcode.ai/provider/v1/systemone";

export interface JevSettings {
  /** Off unless set: calls are sent to Command Code, which needs a plan with Provider API access. */
  enabled?: boolean;
  model?: string;
  endpoint?: string;
  apiKeyEnv?: string;
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
  /** A concern at or above this is a flag (and makes the call risky). */
  flag: number;
  /** Every concern must be below this for "safe". */
  clear: number;
}

export const DEFAULT_THRESHOLDS: Thresholds = { allow: 0.8, headlessAllow: 0.9, deny: 0.85, confidence: 0.6, flag: 0.7, clear: 0.3 };

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
    instructions: "Does this call change anything outside the workspace directory?",
    criteria: {
      true: "Writes, deletes or moves files outside the workspace, changes the home directory or the system, pushes, publishes, deploys, or sends data to a server",
      false: "Only reads, or only changes files inside the workspace",
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

const QUESTIONS = {
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
  ...Object.fromEntries(Object.entries(CONCERNS).map(([k, c]) => [k, { type: "noul", instructions: c.instructions, criteria: c.criteria }])),
};

const FITS_REQUEST = {
  type: "noul",
  instructions: "Is this call a step toward what the user asked for in `user_request`?",
  criteria: {
    true: "Serves the request directly, or is a reasonable preparation for it",
    false: "Unrelated to the request, or goes beyond what was asked",
  },
};

export interface JevAnswers {
  verdict: { choice: Decision; probabilities: Record<Decision, number>; confidence: number };
  concerns: Record<Concern, number>;
  fitsRequest?: number;
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
  const offRequest = a.fitsRequest !== undefined && a.fitsRequest <= 1 - t.flag;
  const confident = a.verdict.confidence >= t.confidence;
  let reading: Reading = "unsure";
  if (p.deny >= t.deny && confident) reading = "dangerous";
  else if (p.deny >= 0.5 || p.ask + p.deny >= 0.6 || flags.length || offRequest) reading = "risky";
  else if (
    p.allow >= (interactive ? t.allow : t.headlessAllow) &&
    confident &&
    Object.values(a.concerns).every((v) => v < t.clear) &&
    (a.fitsRequest === undefined || a.fitsRequest >= 0.5)
  )
    reading = "safe";
  const label = { safe: "安全", risky: "要確認", dangerous: "危険", unsure: "判断保留" }[reading];
  const parts = [`allow ${fmt(p.allow)} · ask ${fmt(p.ask)} · deny ${fmt(p.deny)}`];
  for (const k of flags) parts.push(`${CONCERNS[k].label} ${fmt(a.concerns[k])}`);
  if (offRequest) parts.push(`依頼との関連 ${fmt(a.fitsRequest!)}`);
  return { reading, summary: `Jev: ${label}（${parts.join(" · ")}）`, answers: a };
}

const fmt = (n: number) => n.toFixed(2);

/** Parse the answers of a systemone response; throws when they are not what was asked. */
export function parseAnswers(body: unknown): JevAnswers {
  const answers = (body as { answers?: Record<string, any> })?.answers;
  const v = answers?.verdict;
  const probs = v?.probabilities;
  if (!answers || !probs || typeof v.confidence !== "number") throw new Error("Jev gave no verdict");
  const num = (x: unknown) => {
    if (typeof x !== "number" || !Number.isFinite(x)) throw new Error("Jev gave a malformed answer");
    return x;
  };
  const concerns = Object.fromEntries((Object.keys(CONCERNS) as Concern[]).map((k) => [k, num(answers[k]?.noul)])) as Record<Concern, number>;
  return {
    verdict: {
      choice: v.choice,
      probabilities: { allow: num(probs.allow ?? 0), ask: num(probs.ask ?? 0), deny: num(probs.deny ?? 0) },
      confidence: v.confidence,
    },
    concerns,
    fitsRequest: answers.fits_request ? num(answers.fits_request.noul) : undefined,
  };
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
/** Words that run the command after them. */
const PREFIXES = new Set(["sudo", "env", "command", "exec", "nice", "nohup", "time", "xargs"]);

/** Arguments of a command that are, or look like, files it may touch. */
export function pathArgs(command: string): string[] {
  const out: string[] = [];
  for (const segment of command.split(CHAIN)) {
    const words = shellWords(segment.trim());
    while (words.length && (/^[A-Za-z_][A-Za-z0-9_]*=/.test(words[0]!) || PREFIXES.has(words[0]!))) words.shift();
    const changesFiles = FILE_COMMANDS.has(basename(words[0] ?? ""));
    for (const w of words.slice(1)) {
      const arg = w.replace(/^[0-9]*[<>]+/, "");
      if (!arg || arg.startsWith("-") || arg.includes("://")) continue;
      if (changesFiles || /^(~|\$HOME|\$\{HOME\}|\.{1,2}(\/|$)|\/)/.test(arg) || arg.includes("/")) out.push(arg);
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
  const steps = command?.split(CHAIN).map((s) => s.trim()).filter(Boolean) ?? [];
  const written = f.tool.paths?.(f.args, f.cwd) ?? (command !== undefined ? pathArgs(command) : []);
  const root = git(f.cwd, ["rev-parse", "--show-toplevel"])?.trim() || undefined;
  const workspace = real(f.cwd);
  const targets = written.slice(0, 12).map((p) => {
    const where = real(expand(p, f.cwd));
    const exists = existsSync(where) ? (statSync(where).isDirectory() ? "directory" : "file") : "does not exist";
    return {
      path: p,
      resolves_to: tildify(where),
      location: where === homedir() ? "the home directory itself" : inside(where, workspace) ? "inside the workspace" : "outside the workspace",
      exists,
      git: exists === "does not exist" ? "n/a" : gitState(where, root),
    };
  });
  const state: Record<string, unknown> = { workspace: tildify(workspace), call };
  if (steps.length > 1) state.steps = steps.map((s) => clip(redact(s), 400));
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

async function keychain(account: string): Promise<string | undefined> {
  const cmd =
    process.platform === "darwin"
      ? ["security", "find-generic-password", "-s", "sasacode", "-a", account, "-w"]
      : process.platform === "linux"
        ? ["secret-tool", "lookup", "service", "sasacode", "account", account]
        : undefined;
  if (!cmd) return;
  try {
    const p = Bun.spawn(cmd, { stdout: "pipe", stderr: "ignore" });
    const out = (await new Response(p.stdout).text()).trim();
    return (await p.exited) === 0 && out ? out : undefined;
  } catch {
    return;
  }
}

const jevGuard: Plugin = (api) => {
  const s = api.settings as JevSettings;
  const thresholds = { ...DEFAULT_THRESHOLDS, ...s.thresholds };
  const model = s.model ?? "typesafe/jev";
  const endpoint = s.endpoint ?? JEV_ENDPOINT;
  const keyEnv = s.apiKeyEnv ?? "CMD_API_KEY";
  const skip = s.skip ?? ["todo_write", "task"];
  const cache = new Map<string, JevAnswers>();
  const stats = { calls: 0, failures: 0, changed: 0 };
  const recent: string[] = [];
  let key: Promise<string | undefined> | undefined;
  let warned = "";

  api.registerCommand({
    name: "jev",
    description: "Jev ガード（実行前の安全判断）の状態",
    run: () =>
      api.ui.notify(
        s.enabled
          ? [
              `Jev ガード: 有効（${model}）`,
              `問い合わせ ${stats.calls} 回 · 判定を変えた ${stats.changed} 回 · 失敗 ${stats.failures} 回`,
              ...recent.slice(-5),
            ].join("\n")
          : 'Jev ガード: 無効。config の plugins.settings["jev-guard"] に {"enabled": true} を書くと有効（Command Code の Provider API キー CMD_API_KEY が必要）',
      ),
  });
  if (!s.enabled) return;

  const warn = (message: string) => {
    stats.failures++;
    if (message === warned) return;
    warned = message;
    api.ui.notify(`jev-guard: ${message}（Jev なしで判定を続けます）`, "warning");
  };

  async function ask(state: Record<string, unknown>, signal?: AbortSignal): Promise<JevAnswers | undefined> {
    // The keychain holds the Command Code key only under the provider's own variable.
    key ??= process.env[keyEnv] ? Promise.resolve(process.env[keyEnv]) : keyEnv === "CMD_API_KEY" ? keychain("commandcode") : Promise.resolve(undefined);
    const apiKey = await key;
    if (!apiKey) return void warn(`API キーがありません（${keyEnv}）`);
    const questions: Record<string, unknown> = { ...QUESTIONS };
    if (state.user_request) questions.fits_request = FITS_REQUEST;
    const body = JSON.stringify({ model, state, questions });
    const hit = cache.get(body);
    if (hit) return hit;
    stats.calls++;
    try {
      const deadline = AbortSignal.any([AbortSignal.timeout(s.timeoutMs ?? 5000), ...(signal ? [signal] : [])]);
      const post = () =>
        fetch(endpoint, { method: "POST", headers: { authorization: `Bearer ${apiKey}`, "content-type": "application/json" }, body, signal: deadline });
      let res = await post();
      // Overloaded or rate limited: one more try within the same time limit.
      if (res.status === 429 || res.status >= 500) res = await post();
      if (!res.ok) return void warn(`HTTP ${res.status} ${clip((await res.text()).trim(), 200)}`);
      const answers = parseAnswers(await res.json());
      if (cache.size >= 256) cache.delete(cache.keys().next().value!);
      cache.set(body, answers);
      return answers;
    } catch (e) {
      return void warn((e as Error).message);
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
