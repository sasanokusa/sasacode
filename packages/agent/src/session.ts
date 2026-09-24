import { createHash } from "node:crypto";
import { appendFileSync, mkdirSync, readdirSync, readFileSync, realpathSync, statSync, truncateSync } from "node:fs";
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
    mkdirSync(dir, { recursive: true });
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
    appendFileSync(this.path, `${line}\n`);
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
export function restore(entries: SessionEntry[]): { messages: Message[]; model?: string; permissionMode?: string; repaired: boolean } {
  const messages: Message[] = [];
  let model: string | undefined;
  let permissionMode: string | undefined;
  for (const e of entries) {
    if (e.type === "message") {
      messages.push(e.message);
      // The model that answered last is the one to continue with.
      if (e.message.role === "assistant") model = `${e.message.provider}/${e.message.model}`;
    } else if (e.type === "replace") messages.splice(0, messages.length, ...e.messages);
    else if (e.type === "model") model = e.model;
    else if (e.type === "permission_mode") permissionMode = e.mode;
  }
  const settled = settleToolCalls(messages);
  return { messages: settled, model, permissionMode, repaired: settled.length !== messages.length };
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
  const out: SessionInfo[] = [];
  for (const path of paths) {
    try {
      const entries = readEntries(path);
      const head = entries[0];
      if (head?.type !== "session") continue;
      if (cwd && normalizeCwd(head.cwd) !== normalizeCwd(cwd)) continue;
      const firstUser = entries.find((e) => e.type === "message" && e.message.role === "user");
      const firstPrompt =
        firstUser?.type === "message" && firstUser.message.role === "user"
          ? firstUser.message.content.map((c) => (c.type === "text" ? c.text : "[image]")).join(" ")
          : "";
      out.push({
        id: head.id,
        path,
        cwd: head.cwd,
        createdAt: head.createdAt,
        modifiedAt: statSync(path).mtime,
        firstPrompt,
        messageCount: restore(entries).messages.length,
      });
    } catch {}
  }
  return out.sort((a, b) => b.modifiedAt.getTime() - a.modifiedAt.getTime());
}
