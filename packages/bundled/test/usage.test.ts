import { afterAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { byDay, graph, planLines, readRecords, type UsageRecord } from "../src/usage.ts";

const home = mkdtempSync(join(tmpdir(), "sasacode-usage-"));
afterAll(() => rmSync(home, { recursive: true, force: true }));
const plain = (s: string) => s.replace(/\x1b\[[0-9;]*m/g, "");

const day = (d: string, h = 12) => new Date(`${d}T${String(h).padStart(2, "0")}:00:00`).getTime();
const assistant = (t: number, input: number, output: number, model = "m") =>
  JSON.stringify({ type: "message", message: { role: "assistant", provider: "p", model, content: [], timestamp: t, stopReason: "stop", usage: { input, output, cacheRead: 0, cacheWrite: 0, cost: 0.01 } } });

test("records come from every session log; compaction's replace entries are not counted twice", () => {
  mkdirSync(join(home, "sessions", "proj-a"), { recursive: true });
  mkdirSync(join(home, "sessions", "proj-b"), { recursive: true });
  writeFileSync(
    join(home, "sessions", "proj-a", "s1.jsonl"),
    [
      JSON.stringify({ type: "session", version: 1, id: "s1", cwd: "/a", createdAt: "" }),
      assistant(day("2026-09-24"), 100, 10),
      JSON.stringify({ type: "message", message: { role: "user", content: [], timestamp: day("2026-09-24") } }),
      JSON.stringify({ type: "replace", messages: [JSON.parse(assistant(day("2026-09-24"), 100, 10)).message] }),
      "{ torn",
    ].join("\n"),
  );
  writeFileSync(join(home, "sessions", "proj-b", "s2.jsonl"), [assistant(day("2026-09-26", 9), 50, 5, "other"), assistant(day("2026-09-26", 18), 50, 5, "other")].join("\n"));
  const records = readRecords(home);
  expect(records.map((r) => r.model)).toEqual(["p/m", "p/other", "p/other"]);
  const days = byDay(records);
  expect(days.get("2026-09-24")).toMatchObject({ tokens: 110, output: 10, requests: 1 });
  expect(days.get("2026-09-26")).toMatchObject({ tokens: 110, requests: 2 });
  expect(readRecords(join(home, "nowhere"))).toEqual([]);
});

test("the graph has a week per column, Sunday on top, today in the last column and nothing after it", () => {
  const days = byDay(readRecords(home));
  const now = day("2026-09-26"); // a Saturday: the last column is full
  const rows = graph(days, 10, now);
  expect(rows).toHaveLength(9); // months, 7 days, legend
  expect(plain(rows[0]!)).toContain("9月");
  const cells = (row: string) => (row.match(/\x1b\[48;5;(\d+)m {2}/g) ?? []).map((c) => Number(/;(\d+)m/.exec(c)![1]));
  expect(cells(rows[7]!)).toHaveLength(10); // Saturday row
  expect(cells(rows[7]!).at(-1)).not.toBe(237); // today was used
  expect(cells(rows[5]!).at(-1)).not.toBe(237); // Thursday the 24th
  expect(cells(rows[1]!).at(-1)).toBe(237); // Sunday the 20th: nothing
  const wednesday = graph(days, 10, day("2026-09-23"));
  expect(cells(wednesday[7]!)).toHaveLength(9); // Saturday the 26th is still ahead
});

test("plan windows show use, time to reset and a warning at the limit", () => {
  const now = Date.now();
  const lines = planLines(
    { plan_type: "plus", rate_limit: { limit_reached: true, primary_window: { used_percent: 95, limit_window_seconds: 18_000, reset_at: now / 1000 + 90 * 60 }, secondary_window: { used_percent: 40, limit_window_seconds: 604_800, reset_at: now / 1000 + 3 * 86_400 } } },
    100,
    now,
  ).map(plain);
  expect(lines[0]).toBe("ChatGPT plus");
  expect(lines[1]).toMatch(/5時間の枠.*95% 使用 · あと 1時間30分でリセット/);
  expect(lines[2]).toMatch(/週の枠.*40% 使用 · あと 3日でリセット/);
  expect(lines[3]).toContain("上限に達しています");
});

// The pre-index algorithm, kept as the reference the index is checked against.
function fullScan(root: string): UsageRecord[] {
  const out: UsageRecord[] = [];
  let dirs: string[] = [];
  try {
    dirs = readdirSync(root);
  } catch {
    return out;
  }
  for (const d of dirs) {
    let files: string[] = [];
    try {
      files = readdirSync(join(root, d)).filter((f) => f.endsWith(".jsonl"));
    } catch {
      continue;
    }
    for (const f of files) {
      let text = "";
      try {
        text = readFileSync(join(root, d, f), "utf8");
      } catch {
        continue;
      }
      for (const line of text.split("\n")) {
        if (!line.startsWith('{"type":"message"') || !line.includes('"role":"assistant"')) continue;
        try {
          const m = (JSON.parse(line) as { message: { provider: string; model: string; usage?: Omit<UsageRecord, "t" | "model">; timestamp?: number } }).message;
          if (m.usage && m.timestamp) out.push({ t: m.timestamp, model: `${m.provider}/${m.model}`, ...m.usage });
        } catch {}
      }
    }
  }
  return out.sort((a, b) => a.t - b.t);
}

test("usage served from the index matches a full scan of the same logs, before and after a change", () => {
  const h = mkdtempSync(join(tmpdir(), "sasacode-usage-scan-"));
  const dir = join(h, "sessions", "proj");
  mkdirSync(dir, { recursive: true });
  const one = [JSON.stringify({ type: "session", version: 1, id: "s1", cwd: "/a", createdAt: "" }), assistant(day("2026-09-24"), 100, 10), JSON.stringify({ type: "replace", messages: [JSON.parse(assistant(day("2026-09-24"), 100, 10)).message] })];
  // No session header and a torn line: counted by both algorithms alike.
  const two = [assistant(day("2026-09-25", 8), 50, 5, "other"), "{ torn", assistant(day("2026-09-25", 20), 7, 3, "other")];
  writeFileSync(join(dir, "one.jsonl"), one.join("\n"));
  writeFileSync(join(dir, "two.jsonl"), two.join("\n"));
  expect(readRecords(h)).toEqual(fullScan(join(h, "sessions")));
  // one.jsonl is appended to (so it alone is re-parsed); two.jsonl keeps its indexed values.
  writeFileSync(join(dir, "one.jsonl"), [...one, assistant(day("2026-09-26"), 1, 1)].join("\n"));
  const records = readRecords(h);
  expect(records).toEqual(fullScan(join(h, "sessions")));
  const days = (r: UsageRecord[]) => [...byDay(r).entries()];
  expect(days(records)).toEqual(days(fullScan(join(h, "sessions"))));
  rmSync(h, { recursive: true, force: true });
});
