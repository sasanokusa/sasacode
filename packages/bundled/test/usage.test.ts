import { afterAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { byDay, graph, planLines, readRecords } from "../src/usage.ts";

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
