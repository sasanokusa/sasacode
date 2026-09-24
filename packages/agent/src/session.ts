import { appendFileSync, mkdirSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import type { Message } from "@sasacode/ai";

export const SESSION_VERSION = 1;

export type SessionEntry =
  | { type: "session"; version: number; id: string; cwd: string; createdAt: string }
  | { type: "message"; message: Message }
  | { type: "model"; model: string }
  | { type: "permission_mode"; mode: string }
  | { type: "custom"; kind: string; data: unknown };

export interface SessionInfo {
  id: string;
  path: string;
  cwd: string;
  createdAt: string;
  modifiedAt: Date;
  firstPrompt: string;
  messageCount: number;
}

export function sessionDir(root: string, cwd: string): string {
  return join(root, "sessions", cwd.replace(/[\\/:]+/g, "-").replace(/^-/, ""));
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

/** Rebuild conversation state from entries, dropping a trailing turn that never completed. */
export function restore(entries: SessionEntry[]): { messages: Message[]; model?: string; permissionMode?: string } {
  const messages: Message[] = [];
  let model: string | undefined;
  let permissionMode: string | undefined;
  for (const e of entries) {
    if (e.type === "message") {
      messages.push(e.message);
      // The model that answered last is the one to continue with.
      if (e.message.role === "assistant") model = `${e.message.provider}/${e.message.model}`;
    } else if (e.type === "model") model = e.model;
    else if (e.type === "permission_mode") permissionMode = e.mode;
  }
  return { messages: trimIncomplete(messages), model, permissionMode };
}

export function trimIncomplete(messages: Message[]): Message[] {
  // Find the last assistant message; if its tool calls are not all answered, cut it and what follows.
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i]!;
    if (m.role !== "assistant") continue;
    const ids = m.content.filter((c) => c.type === "tool_call").map((c) => c.id);
    const answered = new Set(
      messages.slice(i + 1).flatMap((r) => (r.role === "tool" ? [r.toolCallId] : [])),
    );
    if (ids.every((id) => answered.has(id))) return messages;
    return messages.slice(0, i);
  }
  return messages;
}

export function listSessions(dir: string): SessionInfo[] {
  let files: string[];
  try {
    files = readdirSync(dir).filter((f) => f.endsWith(".jsonl"));
  } catch {
    return [];
  }
  const out: SessionInfo[] = [];
  for (const f of files) {
    const path = join(dir, f);
    try {
      const entries = readEntries(path);
      const head = entries[0];
      if (head?.type !== "session") continue;
      const msgs = entries.filter((e) => e.type === "message");
      const firstUser = msgs.find((e) => e.message.role === "user");
      const firstPrompt =
        firstUser?.message.role === "user"
          ? firstUser.message.content.map((c) => (c.type === "text" ? c.text : "[image]")).join(" ")
          : "";
      out.push({
        id: head.id,
        path,
        cwd: head.cwd,
        createdAt: head.createdAt,
        modifiedAt: statSync(path).mtime,
        firstPrompt,
        messageCount: msgs.length,
      });
    } catch {}
  }
  return out.sort((a, b) => b.modifiedAt.getTime() - a.modifiedAt.getTime());
}
