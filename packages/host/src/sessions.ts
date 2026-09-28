// The session catalog: every session log on this machine summarized once in an index, so that
// /resume, -c, -r <id> from another directory and /usage read one small file instead of every log.
import { readdirSync, readFileSync, renameSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { legacyName, normalizeCwd, readEntries, restore } from "@sasacode/agent";

export interface SessionInfo {
  id: string;
  path: string;
  cwd: string;
  createdAt: string;
  modifiedAt: Date;
  firstPrompt: string;
  messageCount: number;
}

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
