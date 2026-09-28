// The session index: listings cost one stat per log instead of re-reading every session, changed
// logs alone are re-parsed, and an id finds its session from any directory.
import { afterAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, statSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { restore, SessionFile, type SessionEntry, sessionDir } from "@sasacode/agent";
import { findSession, listSessions, loadSessionIndex, type SessionInfo } from "../src/index.ts";

const home = mkdtempSync(join(tmpdir(), "sasacode-index-"));
afterAll(() => rmSync(home, { recursive: true, force: true }));
const root = join(home, "sessions");

const header = (id: string, cwd: string) => JSON.stringify({ type: "session", version: 1, id, cwd, createdAt: "2026-09-24T00:00:00.000Z" });
const user = (text: string, t: number) => JSON.stringify({ type: "message", message: { role: "user", content: [{ type: "text", text }], timestamp: t } });
const assistant = (t: number, model = "m") =>
  JSON.stringify({ type: "message", message: { role: "assistant", provider: "p", model, content: [], api: "chat", stopReason: "stop", timestamp: t, usage: { input: 10, output: 5, cacheRead: 0, cacheWrite: 0, cost: 0.01 } } });

function writeLog(name: string, lines: string[]): string {
  const path = join(root, name);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, `${lines.join("\n")}\n`);
  return path;
}

// Whole-second stamps: mtimes survive utimesSync exactly, so the staleness test is deterministic.
const stamp = new Date(1_758_000_000_000);

test("the index summarizes a log: id, cwd, first prompt, message count and usage", () => {
  const path = writeLog("a/one.jsonl", [header("aaaa1111", "/work/a"), user("first prompt", 1), assistant(2), user("more", 3)]);
  const s = loadSessionIndex(root).get(path)!;
  expect(s).toMatchObject({ id: "aaaa1111", cwd: "/work/a", createdAt: "2026-09-24T00:00:00.000Z", firstPrompt: "first prompt", messageCount: 3 });
  expect(s.usage).toEqual([{ t: 2, model: "p/m", input: 10, output: 5, cacheRead: 0, cacheWrite: 0, cost: 0.01 }]);
  // The written index is what later listings read.
  const file = JSON.parse(readFileSync(join(root, ".index.json"), "utf8")) as { version: number; sessions: Record<string, { id: string }> };
  expect(file.version).toBe(1);
  expect(file.sessions[path]!.id).toBe("aaaa1111");
});

test("unchanged logs are served from the index; only changed logs are re-parsed", () => {
  const a = writeLog("inc/a.jsonl", [header("aaaa2222", "/work/b"), user("prompt a", 1), assistant(2)]);
  const b = writeLog("inc/b.jsonl", [header("bbbb2222", "/work/b"), user("prompt b", 1), assistant(2)]);
  for (const f of [a, b]) utimesSync(f, stamp, stamp);
  expect(loadSessionIndex(root).get(a)!.messageCount).toBe(2);
  // b now reads differently on disk, but its size and mtime are unchanged, so a re-parse would
  // only waste time (logs are append-only in practice): the summary keeps its values.
  writeFileSync(b, [header("bbbb2222", "/work/b"), user("PROMPT b", 1), assistant(2)].join("\n") + "\n");
  utimesSync(b, stamp, stamp);
  // a really changed (appended to), so it alone is re-read.
  writeFileSync(a, [header("aaaa2222", "/work/b"), user("prompt a", 1), assistant(2), user("appended", 3)].join("\n") + "\n");
  const index = loadSessionIndex(root);
  expect(index.get(b)!.firstPrompt).toBe("prompt b");
  expect(index.get(a)!.firstPrompt).toBe("prompt a");
  expect(index.get(a)!.messageCount).toBe(3);
});

test("a corrupt index is rebuilt from the logs", () => {
  writeFileSync(join(root, ".index.json"), "{ not json");
  const path = join(root, "a", "one.jsonl");
  expect(loadSessionIndex(root).get(path)!.id).toBe("aaaa1111");
  const file = JSON.parse(readFileSync(join(root, ".index.json"), "utf8")) as { version: number; sessions: Record<string, { firstPrompt: string }> };
  expect(file.version).toBe(1);
  expect(file.sessions[path]!.firstPrompt).toBe("first prompt");
});

// The pre-index algorithm, kept as the reference for "listSessions output unchanged".
function oldListSessions(dir: string, cwd?: string): SessionInfo[] {
  const legacyName = (d: string) => d.replace(/[\\/:]+/g, "-").replace(/^-/, "");
  const real = (d: string) => {
    try {
      return realpathSync(resolve(d));
    } catch {
      return resolve(d);
    }
  };
  const dirs = cwd ? [dir, join(dirname(dir), legacyName(real(cwd))), join(dirname(dir), legacyName(cwd))] : [dir];
  const paths = new Set<string>();
  for (const d of dirs) {
    try {
      for (const f of readdirSync(d)) if (f.endsWith(".jsonl")) paths.add(join(d, f));
    } catch {}
  }
  const out: SessionInfo[] = [];
  for (const path of paths) {
    try {
      const entries: SessionEntry[] = [];
      for (const line of readFileSync(path, "utf8").split("\n")) {
        if (!line.trim()) continue;
        try {
          entries.push(JSON.parse(line));
        } catch {}
      }
      const head = entries[0];
      if (head?.type !== "session") continue;
      if (cwd && real(head.cwd) !== real(cwd)) continue;
      const firstUser = entries.find((e) => e.type === "message" && e.message.role === "user");
      const firstPrompt =
        firstUser?.type === "message" && firstUser.message.role === "user"
          ? firstUser.message.content.map((c) => (c.type === "text" ? c.text : "[image]")).join(" ")
          : "";
      out.push({ id: head.id, path, cwd: head.cwd, createdAt: head.createdAt, modifiedAt: statSync(path).mtime, firstPrompt, messageCount: restore(entries).messages.length });
    } catch {}
  }
  return out.sort((x, y) => y.modifiedAt.getTime() - x.modifiedAt.getTime());
}

test("listSessions output is unchanged versus the pre-index semantics", () => {
  // Distinct mtimes throughout: the expected order must not depend on readdir order.
  const at = (s: number) => new Date(stamp.getTime() + s * 1000);
  const mine = writeLog("work-x/mine.jsonl", [header("cccc3333", "/work/x"), user("mine prompt", 1), assistant(2, "other")]);
  const theirs = writeLog("work-x/theirs.jsonl", [header("dddd3333", "/work/y"), user("theirs", 1)]);
  const headless = writeLog("work-x/headless.jsonl", [user("no header", 1), assistant(2)]);
  const torn = writeLog("work-x/torn.jsonl", [header("eeee3333", "/work/x"), user("before the crash", 1), "{ torn"]);
  utimesSync(mine, at(0), at(0));
  utimesSync(headless, at(1), at(1));
  utimesSync(theirs, at(2), at(2));
  utimesSync(torn, at(3), at(3));
  for (const cwd of [undefined, "/work/x", "/work/y"]) {
    expect(listSessions(join(root, "work-x"), cwd)).toEqual(oldListSessions(join(root, "work-x"), cwd));
  }
  // Newest first; a log without its session header is not a session (its usage still counts).
  expect(listSessions(join(root, "work-x")).map((s) => s.id)).toEqual(["eeee3333", "dddd3333", "cccc3333"]);
  expect(listSessions(join(root, "work-x"), "/work/x").map((s) => s.id)).toEqual(["eeee3333", "cccc3333"]);
  const info = listSessions(join(root, "work-x"), "/work/x").find((s) => s.path === mine)!;
  expect(info).toMatchObject({ cwd: "/work/x", firstPrompt: "mine prompt", messageCount: 2 });
  const noHeader = loadSessionIndex(root).get(headless)!;
  expect(noHeader.id).toBe("");
  expect(noHeader.usage).toHaveLength(1);
});

test("a session id is found from any directory, naming its working directory", () => {
  const path = writeLog("elsewhere/s.jsonl", [header("ffff4444", "/work/elsewhere"), user("other", 1), assistant(2)]);
  utimesSync(path, stamp, stamp);
  const found = findSession(root, "ffff")!; // id prefix
  expect(found).toMatchObject({ id: "ffff4444", path, cwd: "/work/elsewhere" });
  expect(findSession(root, "ffff4444")!.path).toBe(path); // exact id
  expect(findSession(root, path)!.id).toBe("ffff4444"); // exact path
  expect(findSession(root, "no-such-session")).toBeUndefined();
  expect(findSession(root, "headless")).toBeUndefined(); // a log without its header is not resumable
});

test("sessions in the shared pre-0.8 folder are told apart by the directory they record", () => {
  const home2 = join(home, "home-dirs");
  const legacy = join(home2, "sessions", "work-a-b-c");
  const mine = SessionFile.create(legacy, "/work/a-b/c");
  const theirs = SessionFile.create(legacy, "/work/a/b-c");
  for (const f of [mine, theirs]) f.append({ type: "model", model: "t/m" });
  expect(listSessions(sessionDir(home2, "/work/a-b/c"), "/work/a-b/c").map((x) => x.id)).toEqual([mine.id]);
});
