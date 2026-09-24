// Regressions from the v0.6.3 and v0.7.0 reviews: symlink escapes, work after an interrupt, restores, runaway loops.
import { afterAll, expect, test } from "bun:test";
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { type AssistantMessage, emptyUsage, type Message, registerApi, replayProvider } from "@sasacode/ai";
import { editTool, readTool, writeTool } from "@sasacode/tools";
import { Agent, listSessions, PermissionPolicy, restore, SessionFile, sessionDir } from "../src/index.ts";

const root = mkdtempSync(join(tmpdir(), "sasacode-bounds-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));
const proj = join(root, "proj");
const outside = join(root, "outside");
mkdirSync(proj);
mkdirSync(outside);
writeFileSync(join(outside, "secret.txt"), "SECRET");
symlinkSync(outside, join(proj, "linked"));

test("paths through a symlink are judged by where they really go", async () => {
  const p = new PermissionPolicy("edits");
  expect((await p.check({ tool: readTool, args: { path: "linked/secret.txt" }, cwd: proj })).decision).toBe("ask");
  expect((await p.check({ tool: writeTool, args: { path: "linked/new.txt", content: "x" }, cwd: proj })).decision).toBe("ask");
  expect((await p.check({ tool: writeTool, args: { path: "real/new.txt", content: "x" }, cwd: proj })).decision).toBe("allow");
  // A deny rule on the real location cannot be dodged through the link …
  const deny = new PermissionPolicy("auto", { deny: [`read(${outside}/**)`] });
  expect((await deny.check({ tool: readTool, args: { path: "linked/secret.txt" }, cwd: proj })).decision).toBe("deny");
  // … and an allow rule for a project folder does not cover what a link in it points to
  // (a repo could turn `src` into a link to ~/.ssh under a user's `read(src/**)` rule).
  const allow = new PermissionPolicy("ask", { allow: ["read(linked/**)"] });
  expect((await allow.check({ tool: readTool, args: { path: "linked/secret.txt" }, cwd: proj })).decision).toBe("ask");
  // An absolute allow rule for the real location still works.
  const abs = new PermissionPolicy("ask", { allow: [`read(${outside}/**)`] });
  expect((await abs.check({ tool: readTool, args: { path: `${outside}/secret.txt` }, cwd: proj })).decision).toBe("allow");
});

test("a link to a file that does not exist yet is judged by its target", async () => {
  symlinkSync("../outside/new.txt", join(proj, "dangling.txt"));
  symlinkSync("../outside/newdir", join(proj, "dangling-dir"));
  const p = new PermissionPolicy("edits");
  expect((await p.check({ tool: writeTool, args: { path: "dangling.txt", content: "x" }, cwd: proj })).decision).toBe("ask");
  expect((await p.check({ tool: writeTool, args: { path: "dangling-dir/a/b.txt", content: "x" }, cwd: proj })).decision).toBe("ask");
  const deny = new PermissionPolicy("auto", { deny: [`write(${outside}/**)`] });
  expect((await deny.check({ tool: writeTool, args: { path: "dangling.txt", content: "x" }, cwd: proj })).decision).toBe("deny");
  // A link loop cannot be resolved at all: the user decides, even in auto mode.
  symlinkSync("loop-b", join(proj, "loop-a"));
  symlinkSync("loop-a", join(proj, "loop-b"));
  const auto = new PermissionPolicy("auto");
  expect((await auto.check({ tool: writeTool, args: { path: "loop-a", content: "x" }, cwd: proj })).decision).toBe("ask");
});

test("write stops if interrupted while it waits on the file system", async () => {
  writeFileSync(join(proj, "w.txt"), "old");
  const ac = new AbortController();
  const pending = writeTool.execute({ path: "w.txt", content: "new" }, { cwd: proj, signal: ac.signal });
  ac.abort(); // the tool is now waiting on readFile
  expect((await pending).isError).toBe(true);
  expect(readFileSync(join(proj, "w.txt"), "utf8")).toBe("old");
});

test("write and edit do nothing once interrupted", async () => {
  const ac = new AbortController();
  ac.abort();
  const w = await writeTool.execute({ path: "after-abort.txt", content: "x" }, { cwd: proj, signal: ac.signal });
  expect(w.isError).toBe(true);
  expect(existsSync(join(proj, "after-abort.txt"))).toBe(false);
  writeFileSync(join(proj, "e.txt"), "a");
  const e = await editTool.execute({ path: "e.txt", old_string: "a", new_string: "b" }, { cwd: proj, signal: ac.signal });
  expect(e.isError).toBe(true);
  expect(readFileSync(join(proj, "e.txt"), "utf8")).toBe("a");
});

test("an interrupt during one tool stops the approved calls after it", async () => {
  const reply = (content: AssistantMessage["content"]): AssistantMessage => ({
    role: "assistant", content, api: "replay", provider: "t", model: "m", usage: emptyUsage(),
    stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop", timestamp: 0,
  });
  registerApi(
    "replay",
    replayProvider([
      reply([
        { type: "tool_call", id: "1", name: "bash", input: { command: "sleep 5" } },
        { type: "tool_call", id: "2", name: "write", input: { path: "late.txt", content: "x" } },
      ]),
    ]),
  );
  const agent = new Agent({ model: { id: "m", provider: "t", api: "replay", contextWindow: 1e5, maxOutput: 1e3 }, cwd: proj, systemPrompt: "", permissions: new PermissionPolicy("auto"), tools: [writeTool, (await import("@sasacode/tools")).bashTool] });
  agent.events.on((e) => {
    if (e.type === "tool_start" && e.call.name === "bash") setTimeout(() => agent.abort(), 100);
  });
  expect(await agent.prompt("go")).toBe("aborted");
  expect(existsSync(join(proj, "late.txt"))).toBe(false);
  expect(JSON.stringify(agent.messages.at(-1))).toContain("Interrupted by the user before this tool ran");
});

test("restoring twice gives the same valid history once the repair is recorded", () => {
  const s = SessionFile.create(join(root, "sessions"), proj);
  const user = (t: string): Message => ({ role: "user", content: [{ type: "text", text: t }], timestamp: 0 });
  const asst = (content: AssistantMessage["content"]): AssistantMessage => ({
    role: "assistant", content, api: "x", provider: "t", model: "m", usage: emptyUsage(), stopReason: "tool_use", timestamp: 0,
  });
  s.append({ type: "message", message: user("go") });
  s.append({ type: "message", message: asst([{ type: "tool_call", id: "dangling", name: "bash", input: {} }]) });
  const first = restore(SessionFile.open(s.path).entries);
  expect(first.repaired).toBe(true);
  s.append({ type: "replace", messages: first.messages }); // what setup does after a repair
  s.append({ type: "message", message: user("next") });
  s.append({ type: "message", message: { ...asst([{ type: "text", text: "ok" }]), stopReason: "stop" } });
  const second = restore(SessionFile.open(s.path).entries);
  expect(second.repaired).toBe(false);
  expect(second.messages.map((m) => m.role)).toEqual(["user", "assistant", "tool", "user", "assistant"]);
  // Even without the recorded repair, a dangling call in the middle is settled, not left behind.
  const log = [user("a"), asst([{ type: "tool_call", id: "x", name: "bash", input: {} }]), user("b")];
  const settled = restore(log.map((message) => ({ type: "message" as const, message }))).messages;
  expect(settled.map((m) => m.role)).toEqual(["user", "assistant", "tool", "user"]);
});

const turn = (content: AssistantMessage["content"]): AssistantMessage => ({
  role: "assistant", content, api: "replay", provider: "t", model: "m", usage: emptyUsage(),
  stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop", timestamp: 0,
});
const model = { id: "m", provider: "t", api: "replay", contextWindow: 1e5, maxOutput: 1e3 };

test("each result is saved before the next call starts", async () => {
  const session = SessionFile.create(join(root, "sessions-each"), proj);
  const saved: string[] = [];
  const probe = {
    name: "probe", description: "", parameters: { type: "object", properties: {} }, kind: "read" as const,
    execute: async () => {
      saved.push(readFileSync(session.path, "utf8"));
      return { content: [{ type: "text" as const, text: "probe ran" }] };
    },
  };
  registerApi("replay", replayProvider([turn([
    { type: "tool_call", id: "1", name: "probe", input: {} },
    { type: "tool_call", id: "2", name: "probe", input: {} },
  ]), turn([{ type: "text", text: "done" }])]));
  const agent = new Agent({ model, cwd: proj, systemPrompt: "", session, permissions: new PermissionPolicy("auto"), tools: [probe] });
  await agent.prompt("go");
  expect(saved[0]).not.toContain("probe ran");
  expect(saved[1]).toContain("probe ran"); // the first call's result was on disk when the second started
});

test("a run whose calls keep being refused stops after 5 such turns", async () => {
  const bad = (i: number) => turn([{ type: "tool_call", id: String(i), name: "no_such_tool", input: {} }]);
  const provider = replayProvider(Array.from({ length: 20 }, (_, i) => bad(i)));
  registerApi("replay", provider);
  const agent = new Agent({ model, cwd: proj, systemPrompt: "", permissions: new PermissionPolicy("auto"), tools: [readTool] });
  expect(await agent.prompt("go")).toBe("no_progress");
  expect(provider.requests).toHaveLength(5);
});

test("a torn last line is set aside before the session is appended to", () => {
  const s = SessionFile.create(join(root, "sessions-torn"), proj);
  s.append({ type: "message", message: { role: "user", content: [{ type: "text", text: "one" }], timestamp: 0 } });
  appendFileSync(s.path, '{"type":"message","mess');
  const { file } = SessionFile.open(s.path);
  file.append({ type: "message", message: { role: "user", content: [{ type: "text", text: "two" }], timestamp: 0 } });
  const texts = SessionFile.open(s.path).entries.flatMap((e) => (e.type === "message" ? [JSON.stringify(e.message.content)] : []));
  expect(texts.join()).toContain("one");
  expect(texts.join()).toContain("two");
  expect(readFileSync(`${s.path}.torn`, "utf8")).toContain('"mess');
  // A complete entry that only lacks its newline is kept.
  appendFileSync(s.path, JSON.stringify({ type: "model", model: "t/m" }));
  SessionFile.open(s.path).file.append({ type: "permission_mode", mode: "ask" });
  expect(SessionFile.open(s.path).entries.map((e) => e.type)).toEqual(["session", "message", "message", "model", "permission_mode"]);
});

test("directories whose names flatten the same get separate session folders", () => {
  const home = join(root, "home-dirs");
  expect(sessionDir(home, "/work/a-b/c")).not.toBe(sessionDir(home, "/work/a/b-c"));
  // Sessions in the shared pre-0.8 folder are told apart by the directory they record.
  const legacy = join(home, "sessions", "work-a-b-c");
  const mine = SessionFile.create(legacy, "/work/a-b/c");
  const theirs = SessionFile.create(legacy, "/work/a/b-c");
  for (const f of [mine, theirs]) f.append({ type: "model", model: "t/m" });
  expect(listSessions(sessionDir(home, "/work/a-b/c"), "/work/a-b/c").map((x) => x.id)).toEqual([mine.id]);
});
