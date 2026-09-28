import { createHash } from "node:crypto";
import { appendFileSync, mkdirSync, readdirSync, readFileSync, realpathSync, renameSync, statSync, truncateSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import type { Message } from "@sasacode/ai";

export const SESSION_VERSION = 1;

export type SessionEntry =
  | { type: "session"; version: number; id: string; cwd: string; createdAt: string }
  | { type: "message"; message: Message }
  | { type: "model"; model: string }
  | { type: "permission_mode"; mode: string }
  | { type: "replace"; messages: Message[] }
  /** What the model actually produced for a call that a tool_call_raw hook repaired. */
  | { type: "tool_repair"; callId: string; original: { name: string; input: unknown; rawInput?: string }; repaired: { name: string; input: unknown }; note: string }
  | { type: "custom"; plugin: string; kind: string; data: unknown };

export interface SessionInfo {
  id: string;
  path: string;
  cwd: string;
  createdAt: string;
  modifiedAt: Date;
  firstPrompt: string;
  messageCount: number;
}

function normalizeCwd(cwd: string): string {
  try {
    return realpathSync(resolve(cwd));
  } catch {
    return resolve(cwd);
  }
}

/** The folder name used before 0.8: different paths could map to the same one (/a-b/c, /a/b-c). */
function legacyName(cwd: string): string {
  return cwd.replace(/[\\/:]+/g, "-").replace(/^-/, "");
}

/** Readable name plus a hash of the real path, so two directories never share a folder. */
export function sessionDir(root: string, cwd: string): string {
  const real = normalizeCwd(cwd);
  const hash = createHash("sha256").update(real).digest("hex").slice(0, 8);
  return join(root, "sessions", `${legacyName(real).slice(-80)}-${hash}`);
}

/** Append-only JSONL log. Every entry is flushed immediately so a crash loses nothing that was complete. */
export class SessionFile {
  readonly id: string;
  readonly path: string;
  private secrets: string[];
  /** The header is written with the first real entry, so sessions nobody used leave no file. */
  private header?: SessionEntry;

  private constructor(path: string, id: string, secrets: string[]) {
    this.path = path;
    this.id = id;
    this.secrets = secrets.filter((s) => s.length >= 8);
  }

  static create(dir: string, cwd: string, secrets: string[] = []): SessionFile {
    // Conversations hold file contents and pasted text: readable by the user only.
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    const id = crypto.randomUUID().slice(0, 8);
    const createdAt = new Date().toISOString();
    const path = join(dir, `${createdAt.replace(/[:.]/g, "-")}_${id}.jsonl`);
    const f = new SessionFile(path, id, secrets);
    f.header = { type: "session", version: SESSION_VERSION, id, cwd, createdAt };
    return f;
  }

  static open(path: string, secrets: string[] = []): { file: SessionFile; entries: SessionEntry[] } {
    repairTail(path);
    const entries = readEntries(path);
    const head = entries[0];
    if (head?.type !== "session") throw new Error(`${path} is not a session file`);
    return { file: new SessionFile(path, head.id, secrets), entries };
  }

  addSecret(secret: string | undefined): void {
    if (secret && secret.length >= 8 && !this.secrets.includes(secret)) this.secrets.push(secret);
  }

  append(entry: SessionEntry): void {
    if (this.header) {
      const h = this.header;
      this.header = undefined;
      this.append(h);
    }
    let line = JSON.stringify(entry);
    // Never persist API keys, even if a tool echoed one.
    for (const s of this.secrets) line = line.replaceAll(s, "[REDACTED]");
    appendFileSync(this.path, `${line}\n`, { mode: 0o600 });
  }
}

/**
 * A crash can leave the last line half written. Appending after it would glue the next entry onto
 * it and lose that too: finish a complete line, or move a torn one to `<file>.torn`.
 */
function repairTail(path: string): void {
  const raw = readFileSync(path, "utf8");
  if (!raw || raw.endsWith("\n")) return;
  const cut = raw.lastIndexOf("\n") + 1;
  const tail = raw.slice(cut);
  try {
    JSON.parse(tail);
    appendFileSync(path, "\n");
  } catch {
    appendFileSync(`${path}.torn`, `${tail}\n`);
    truncateSync(path, Buffer.byteLength(raw.slice(0, cut)));
  }
}

function readEntries(path: string): SessionEntry[] {
  const out: SessionEntry[] = [];
  for (const line of readFileSync(path, "utf8").split("\n")) {
    if (!line.trim()) continue;
    try {
      out.push(JSON.parse(line));
    } catch {
      // A torn last line from a crash is skipped.
    }
  }
  return out;
}

/**
 * Rebuild conversation state from entries. Tool calls that never got a result (a crash mid-turn)
 * get one saying so; `repaired` tells the caller to persist that (a replace entry), so the next
 * restore sees the same history.
 */
/**
 * The conversation and the model to continue with. `permission_mode` entries are a record of what
 * the user chose, not restored: resuming never starts in a looser mode than the one asked for now.
 */
export function restore(entries: SessionEntry[]): { messages: Message[]; model?: string; repaired: boolean } {
  const messages: Message[] = [];
  let model: string | undefined;
  for (const e of entries) {
    if (e.type === "message") {
      messages.push(e.message);
      // The model that answered last is the one to continue with.
      if (e.message.role === "assistant") model = `${e.message.provider}/${e.message.model}`;
    } else if (e.type === "replace") messages.splice(0, messages.length, ...e.messages);
    else if (e.type === "model") model = e.model;
  }
  const settled = settleToolCalls(messages);
  return { messages: settled, model, repaired: settled.length !== messages.length };
}

// No result was saved, which does not mean the call did nothing: it may have run just before a crash.
const NO_RESULT =
  "[Result unknown: the session ended before this call's result was saved. It may or may not have run. If it changes anything, check the current state before running it again.]";

/**
 * Every tool call must be followed by its result before the next user or assistant message
 * (all APIs reject history otherwise). Add an error result for each call that has none,
 * anywhere in the history.
 */
export function settleToolCalls(messages: Message[]): Message[] {
  const out: Message[] = [];
  for (let i = 0; i < messages.length; i++) {
    const m = messages[i]!;
    out.push(m);
    if (m.role !== "assistant") continue;
    const calls = m.content.filter((c) => c.type === "tool_call");
    if (!calls.length) continue;
    // The results of this turn are the tool messages right after it.
    let j = i + 1;
    const answered = new Set<string>();
    for (; j < messages.length && messages[j]!.role === "tool"; j++) {
      const r = messages[j] as Extract<Message, { role: "tool" }>;
      answered.add(r.toolCallId);
      out.push(r);
    }
    for (const c of calls)
      if (!answered.has(c.id))
        out.push({ role: "tool", toolCallId: c.id, toolName: c.name, content: [{ type: "text", text: NO_RESULT }], isError: true, timestamp: m.timestamp });
    i = j - 1;
  }
  return out;
}

// ── session index ──────────────────────────────────────────────────

/** What one model response used. The same shape /usage aggregates its records into. */
export interface SessionUsage {
  /** Epoch ms. */
  t: number;
  model: string;
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  cost: number;
}

/**
 * What one session log adds up to. Kept in the index so listings cost one stat per log instead of
 * reading and rebuilding every session; a log is re-parsed only when its size or mtime changed.
 */
export interface SessionSummary {
  /** "" when the log's first line was torn away: not resumable, but its usage still counts. */
  id: string;
  cwd: string;
  createdAt: string;
  /** Size and mtime (ms) at the last parse: the staleness test for the log itself. */
  size: number;
  modifiedAt: number;
  firstPrompt: string;
  /** `restore(entries).messages.length`: the history a resume would start from. */
  messageCount: number;
  /**
   * Every model response in the log. Kept per response rather than summed per day and model:
   * /usage reports each response and the last use of each model, and a record is a few dozen bytes.
   */
  usage: SessionUsage[];
}

/**
 * The one index for all sessions on this machine: `<sessions root>/.index.json`, keyed by session
 * log path. Written by temp file + rename and rebuilt from scratch when missing or corrupt;
 * concurrent sasacode instances can lose an update, which only costs one re-parse.
 */
const INDEX_NAME = ".index.json";

function summarize(path: string): SessionSummary | undefined {
  try {
    const stat = statSync(path);
    const entries = readEntries(path);
    const head = entries[0];
    const session = head?.type === "session" ? head : undefined;
    const firstUser = entries.find((e) => e.type === "message" && e.message.role === "user");
    const usage: SessionUsage[] = [];
    for (const e of entries) {
      // Only "message" entries: a "replace" (compaction) lists earlier messages again.
      if (e.type !== "message" || e.message.role !== "assistant") continue;
      const m = e.message;
      if (m.usage && m.timestamp)
        usage.push({ t: m.timestamp, model: `${m.provider}/${m.model}`, input: m.usage.input, output: m.usage.output, cacheRead: m.usage.cacheRead, cacheWrite: m.usage.cacheWrite, cost: m.usage.cost });
    }
    return {
      id: session?.id ?? "",
      cwd: session?.cwd ?? "",
      createdAt: session?.createdAt ?? "",
      size: stat.size,
      modifiedAt: stat.mtimeMs,
      firstPrompt:
        firstUser?.type === "message" && firstUser.message.role === "user"
          ? firstUser.message.content.map((c) => (c.type === "text" ? c.text : "[image]")).join(" ")
          : "",
      messageCount: restore(entries).messages.length,
      usage,
    };
  } catch {
    return undefined;
  }
}

/**
 * Summaries of every session log under the sessions root `root`, keyed by log path. Logs the index
 * already knows are served from it; only logs whose size or mtime moved are re-parsed, and the
 * index is rewritten when anything changed.
 */
export function loadSessionIndex(root: string): Map<string, SessionSummary> {
  // The layout is one folder per working directory (see sessionDir): one level of readdir.
  const paths: string[] = [];
  try {
    for (const d of readdirSync(root)) {
      try {
        for (const f of readdirSync(join(root, d))) if (f.endsWith(".jsonl")) paths.push(join(root, d, f));
      } catch {}
    }
  } catch {}
  let stored: Record<string, SessionSummary> = {};
  try {
    const index = JSON.parse(readFileSync(join(root, INDEX_NAME), "utf8")) as { version?: number; sessions?: Record<string, SessionSummary> };
    if (index.version === 1 && index.sessions && typeof index.sessions === "object") stored = index.sessions;
  } catch {}
  const out = new Map<string, SessionSummary>();
  let dirty = false;
  for (const path of paths) {
    const cached = stored[path];
    let summary: SessionSummary | undefined;
    try {
      const stat = statSync(path);
      if (cached && cached.size === stat.size && cached.modifiedAt === stat.mtimeMs) summary = cached;
    } catch {}
    if (!summary) {
      summary = summarize(path);
      if (summary) dirty = true;
    }
    if (summary) out.set(path, summary);
  }
  // Logs that vanished drop out too, so the index cannot grow without bound.
  if (!dirty) for (const path of Object.keys(stored)) if (!out.has(path)) dirty = true;
  if (dirty) {
    const tmp = `${join(root, INDEX_NAME)}.${process.pid}.tmp`;
    try {
      writeFileSync(tmp, JSON.stringify({ version: 1, sessions: Object.fromEntries(out) }));
      renameSync(tmp, join(root, INDEX_NAME));
    } catch {
      // Best effort: a read-only or vanished root must not break a listing; the next call re-parses.
    }
  }
  return out;
}

const toInfo = (path: string, s: SessionSummary): SessionInfo => ({
  id: s.id,
  path,
  cwd: s.cwd,
  createdAt: s.createdAt,
  modifiedAt: new Date(s.modifiedAt),
  firstPrompt: s.firstPrompt,
  messageCount: s.messageCount,
});

/**
 * Sessions in `dir`, newest first. With `cwd`, only that directory's sessions, including ones
 * saved under the pre-0.8 folder name (which other directories may share).
 */
export function listSessions(dir: string, cwd?: string): SessionInfo[] {
  const dirs = cwd ? [dir, join(dirname(dir), legacyName(normalizeCwd(cwd))), join(dirname(dir), legacyName(cwd))] : [dir];
  const paths = new Set<string>();
  for (const d of dirs) {
    try {
      for (const f of readdirSync(d)) if (f.endsWith(".jsonl")) paths.add(join(d, f));
    } catch {}
  }
  const index = loadSessionIndex(dirname(dir));
  const out: SessionInfo[] = [];
  for (const path of paths) {
    const s = index.get(path);
    // No id: not a session log, or its header was torn away (its usage still counts).
    if (!s?.id) continue;
    if (cwd && normalizeCwd(s.cwd) !== normalizeCwd(cwd)) continue;
    out.push(toInfo(path, s));
  }
  return out.sort((a, b) => b.modifiedAt.getTime() - a.modifiedAt.getTime());
}

/**
 * The session matching `ref` in any session folder under `root`, for resuming by id from another
 * directory. Matching is what a resume uses locally: exact path, exact id, or an id prefix; the
 * newest wins when a prefix matches several.
 */
export function findSession(root: string, ref: string): SessionInfo | undefined {
  let best: SessionInfo | undefined;
  for (const [path, s] of loadSessionIndex(root)) {
    if (!s.id || !(s.id === ref || path === ref || s.id.startsWith(ref))) continue;
    const info = toInfo(path, s);
    if (!best || info.modifiedAt > best.modifiedAt) best = info;
  }
  return best;
}
