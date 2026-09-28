import { expect, test } from "bun:test";
import type { PluginUI, SelectOption } from "@sasacode/plugin-api";
import plugin, { ask } from "./index.ts";
import { loadPlugin } from "../test-support.ts";

function fakeUI(picks: (string | undefined)[], lang: "ja" | "en" = "en") {
  const shown: { title: string; options: SelectOption[] }[] = [];
  const ui = {
    interactive: true, lang,
    select: async (title: string, options: SelectOption[]) => { shown.push({ title, options }); return picks.shift(); },
  } as unknown as PluginUI;
  return { ui, shown };
}
function modernUI(picks: (string | undefined)[], many: (string[] | undefined)[], typed: (string | undefined)[]) {
  const seen: string[] = [];
  const ui = {
    interactive: true, lang: "en",
    select: async (title: string, options: SelectOption[]) => { seen.push(`select ${options.at(-1)!.label}`); return picks.shift(); },
    selectMany: async () => { seen.push("selectMany"); return many.shift(); },
    input: async () => { seen.push("input"); return typed.shift(); },
  } as unknown as PluginUI;
  return { ui, seen };
}
const ctx = { cwd: "/", signal: new AbortController().signal };
const body = (r: { content: unknown[] }) => (r.content[0] as { text: string }).text;
const q = (question: string, ...labels: string[]) => ({ question, options: labels.map((label) => ({ label })) });

test("asks each question in turn and reports the chosen labels", async () => {
  const { ui, shown } = fakeUI(["1", "0"]);
  const r = await ask(ui, { questions: [q("Which DB?", "SQLite", "Postgres"), q("ORM?", "None", "Drizzle")] }, ctx);
  expect(shown.map((s) => s.title)).toEqual(["Question 1/2\nWhich DB?", "Question 2/2\nORM?"]);
  expect(shown[0]!.options.at(-1)!.label).toBe("Other (answer in chat)");
  expect(body(r)).toBe("Q: Which DB?\nA: Postgres\n\nQ: ORM?\nA: None");
  expect(r.isError).toBeUndefined();
});

test("duplicate labels are told apart by position", async () => {
  const { ui } = fakeUI(["1"]);
  const r = await ask(ui, { questions: [{ question: "Pick", options: [{ label: "A", description: "first" }, { label: "A", description: "second" }] }] }, ctx);
  expect((r.details as any).answers[0].answer).toEqual(["A"]);
  expect(body(r)).toContain("A: A");
});

test("choosing Other stops and tells the model to wait for the user's words", async () => {
  const { ui, shown } = fakeUI(["0", "\0other"], "ja");
  const r = await ask(ui, { questions: [q("One?", "x", "y"), q("Two?", "x", "y"), q("Three?", "x", "y")] }, ctx);
  expect(shown).toHaveLength(2);
  expect(shown[0]!.options.at(-1)!.label).toBe("その他（チャットで答える）");
  expect(body(r)).toContain("A: x");
  expect(body(r)).toContain("End your turn now");
  expect(body(r)).toContain("1 later question(s) were not asked");
});

test("a dismissed question is not answered for the user", async () => {
  const { ui } = fakeUI([undefined]);
  const r = await ask(ui, { questions: [q("One?", "x", "y")] }, ctx);
  expect(body(r)).toContain("dismissed");
  expect((r.details as any).answers).toEqual([{ question: "One?", outcome: "dismissed" }]);
});

test("an interrupt ends the call while the picker is open", async () => {
  const abort = new AbortController();
  const ui = { interactive: true, lang: "en", select: () => new Promise(() => {}) } as unknown as PluginUI;
  const pending = ask(ui, { questions: [q("One?", "x", "y")] }, { cwd: "/", signal: abort.signal });
  abort.abort();
  const r = await pending;
  expect(r.isError).toBe(true);
});

test("runs without an approval prompt and says so when no user is there", async () => {
  const { invoke } = await loadPlugin("ask-user", plugin, process.cwd());
  const r = await invoke("ask_user", { questions: [q("One?", "x", "y")] });
  expect(r.isError).toBeFalsy();
  expect(body(r)).toContain("No user is available");
});

test("the schema rejects a question with a single option", async () => {
  const { invoke } = await loadPlugin("ask-user", plugin, process.cwd());
  const r = await invoke("ask_user", { questions: [q("One?", "x")] });
  expect(r.isError).toBe(true);
});

test("with API 1.10, Other asks for the answer right there", async () => {
  const { ui, seen } = modernUI(["\0other", "1"], [], ["  MySQL 8  "]);
  const r = await ask(ui, { questions: [q("Which DB?", "SQLite", "Postgres"), q("ORM?", "None", "Drizzle")] }, ctx);
  expect(seen).toEqual(["select Other (type an answer)", "input", "select Other (type an answer)"]);
  expect(body(r)).toBe("Q: Which DB?\nA: (in the user's own words) MySQL 8\n\nQ: ORM?\nA: Drizzle");
});

test("an empty or cancelled free-text answer counts as dismissed", async () => {
  for (const typed of ["", undefined]) {
    const { ui } = modernUI(["\0other"], [], [typed]);
    const r = await ask(ui, { questions: [q("One?", "x", "y"), q("Two?", "x", "y")] }, ctx);
    expect(body(r)).toContain("dismissed");
    expect(body(r)).toContain("1 later question(s) were not asked");
  }
});

test("multiSelect checks several options, with Other among them", async () => {
  const { ui, seen } = modernUI([], [["0", "2", "\0other"]], ["Svelte"]);
  const r = await ask(ui, { questions: [{ ...q("Which frameworks?", "React", "Vue", "Solid"), multiSelect: true }] }, ctx);
  expect(seen).toEqual(["selectMany", "input"]);
  expect(body(r)).toBe("Q: Which frameworks?\nA: React; Solid; (in the user's own words) Svelte");
});

test("confirming no options in a multiSelect is a dismissal", async () => {
  const { ui } = modernUI([], [[]], []);
  const r = await ask(ui, { questions: [{ ...q("Which?", "a", "b"), multiSelect: true }] }, ctx);
  expect(body(r)).toContain("dismissed");
});

test("multiSelect falls back to one choice on a host before 1.10", async () => {
  const { ui, shown } = fakeUI(["1"]);
  const r = await ask(ui, { questions: [{ ...q("Which?", "a", "b"), multiSelect: true }] }, ctx);
  expect(shown).toHaveLength(1);
  expect(body(r)).toBe("Q: Which?\nA: b");
});
