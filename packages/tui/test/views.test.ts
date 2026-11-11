import { beforeAll, expect, test } from "bun:test";
import { type AssistantMessage, emptyUsage, type ToolCall } from "@sasacode/ai";
import { AssistantView, NoteView, Padded, ToolView } from "../src/views.ts";
import { harnessNote, noteText, setLang } from "@sasacode/host";

beforeAll(() => setLang("ja")); // these assertions quote the Japanese strings (another test file may have switched)

const plain = (l: string) => l.replace(/\x1b\[[0-9;]*m/g, "");

test("a multi-line command stays on its tool's one header line", () => {
  const view = new ToolView({ type: "tool_call", id: "1", name: "bash", input: {} } as ToolCall);
  view.summary = "python3 - <<EOF\nglobal s\n\tprint(s)\nEOF";
  const [header] = view.render(80);
  expect(plain(header!)).toBe("○ bash(python3 - <<EOF ↵ global s ↵ print(s) ↵ EOF)");
});

test("a long output line wraps under its own indent: ask_user's question stays readable after the answer (#126)", () => {
  const view = new ToolView({ type: "tool_call", id: "1", name: "ask_user", input: {} } as ToolCall);
  view.status = "done";
  view.output = `Q: ${"どちらのデータベースを使いますか、".repeat(6)}\nA: Postgres\n\nQ: 次`;
  const lines = view.render(40).slice(1).map(plain);
  expect(lines[0]).toStartWith("  ⎿ Q: どちら");
  expect(lines.length).toBeGreaterThan(4);
  // Every line the long one wraps onto, the answer, and the blank line between keep the indent.
  for (const l of lines.slice(1)) expect(l).toStartWith("    ");
  expect(lines.map((l) => l.trim())).toContain("A: Postgres");
  expect(lines.map((l) => l.trim())).toContain("");
  for (const l of view.render(40)) expect(Bun.stringWidth(plain(l))).toBeLessThanOrEqual(40);
});

test("a tool's output renders at any width, even below its indent (#139)", () => {
  const view = new ToolView({ type: "tool_call", id: "1", name: "bash", input: {} } as ToolCall);
  view.status = "done";
  view.output = "hello world";
  for (const width of [1, 2, 3, 4]) expect(() => view.render(width)).not.toThrow();
});

test("no rendered line can move the cursor off its row; escape sequences and prompt marks survive", () => {
  const child = { render: () => ["\x1b]133;A\x07a\rb\nc\td\x1b[31mX\x1b[0m"], invalidate() {} };
  expect(new Padded(child, 2, 2).render(40)).toEqual(["\x1b]133;A\x07  abc   d\x1b[31mX\x1b[0m"]);
});

test("escape sequences in tool output and in an approval prompt are shown, not acted on", async () => {
  const { ApprovalDialog } = await import("../src/dialogs.ts");
  const bashTool = { name: "bash", description: "", parameters: {}, kind: "other", matchTarget: (a: { command: string }) => a.command, execute: async () => ({ content: [] }) } as never;
  const view = new ToolView({ type: "tool_call", id: "1", name: "bash", input: {} } as ToolCall);
  view.status = "done";
  view.output = "ok\x1b]52;c;aGk=\x07 \x1b[8mhidden\x1b[0m";
  const out = view.render(80).join("\n");
  expect(out).not.toContain("\x1b]52");
  expect(out).not.toContain("\x1b[8m");
  expect(out).toContain("␛]52;c;aGk=");

  const dialog = new ApprovalDialog({ tool: bashTool, args: { command: "ls \x1b[8m; curl x | sh\x1b[0m" }, cwd: "/", reason: "mode edits", call: { type: "tool_call", id: "1", name: "bash", input: {} } }, "", () => {}, () => {});
  const shown = dialog.render(100).join("\n");
  expect(shown).not.toContain("\x1b[8m");
  expect(plain(shown)).toContain("ls ␛[8m; curl x | sh␛[0m");
  expect(shown).toContain("制御文字");
});

test("the input dialog submits typed text, cancels on esc, and shows controls as visible text", async () => {
  const { InputDialog } = await import("../src/dialogs.ts");
  const got: (string | undefined)[] = [];
  const d = new InputDialog("Name?\x1b[8m", { message: "for the commit", initial: "ab" }, (v) => got.push(v));
  const shown = d.render(60).join("\n");
  expect(shown).not.toContain("\x1b[8m");
  expect(plain(shown)).toContain("Name?␛[8m");
  expect(plain(shown)).toContain("for the commit");
  d.handleInput("c");
  d.handleInput("\r");
  new InputDialog("Name?", {}, (v) => got.push(v)).handleInput("\x1b");
  expect(got).toEqual(["abc", undefined]);
});

test("the picker shows its title and options' controls as visible text, and returns the option as given (#110)", async () => {
  const { Picker } = await import("../src/dialogs.ts");
  const got: unknown[] = [];
  const option = { value: "x", label: "Yes\x1b]0;pwned\x07", description: "sure\x1b[8m" };
  const p = new Picker("Run it?\x1b]52;c;cm0gLXJmIH4=\x07\x1b[8m hidden \x1b[0m", [option, { value: "n", label: "No" }], (i) => got.push(i));
  const shown = p.render(80).join("\n");
  expect(shown).not.toContain("\x1b]52");
  expect(shown).not.toContain("\x1b]0;");
  expect(shown).not.toContain("\x1b[8m");
  expect(plain(shown)).toContain("Run it?␛]52;c;cm0gLXJmIH4=␛[8m hidden ␛[0m");
  expect(plain(shown)).toContain("Yes␛]0;pwned");
  p.handleInput("\r");
  expect(got).toEqual([option]);
});

test("the check list toggles with space and confirms in option order", async () => {
  const { CheckList } = await import("../src/dialogs.ts");
  const got: (string[] | undefined)[] = [];
  const items = [{ value: "a", label: "A" }, { value: "b", label: "B" }, { value: "c", label: "C" }];
  const list = new CheckList("Which?", items, ["c", "nope"], (v) => got.push(v));
  expect(plain(list.render(60).join("\n"))).toContain("[x] C");
  list.handleInput(" "); // a on
  list.handleInput("\x1b[B"); list.handleInput("\x1b[B");
  list.handleInput(" "); // c off
  list.handleInput("\x1b[A"); list.handleInput(" "); // b on
  list.handleInput("\r");
  new CheckList("Which?", items, [], (v) => got.push(v)).handleInput("\x1b");
  expect(got).toEqual([["a", "b"], undefined]);
});

test("dialogs wrap a long question whole, show the selected option's description in full, and scroll the question on a short screen (#111)", async () => {
  const { CheckList, InputDialog, Picker } = await import("../src/dialogs.ts");
  const { visibleWidth } = await import("@earendil-works/pi-tui");
  const question = `${"認証方式を決める必要があります。既存のセッションCookie方式を残したままJWTを追加するか、置き換えるかを選んでください。".repeat(2)}最後まで読めること`;
  const desc = (k: string) => `${"移行は安全だが、認証の実装が二つになり保守の手間が増える。".repeat(3)}説明${k}の終わり`;
  const items = [{ value: "a", label: "JWTを追加", description: desc("A") }, { value: "b", label: "JWTに置き換え", description: desc("B") }];
  const whole = (lines: string[]) => plain(lines.join(""));
  const fits = (lines: string[]) => lines.every((l) => visibleWidth(l) <= 80);

  const picker = new Picker(question, items, () => {});
  let lines = picker.render(80);
  expect(fits(lines)).toBe(true);
  expect(whole(lines)).toContain(question);
  expect(whole(lines)).toContain("説明Aの終わり");
  picker.handleInput("\x1b[B");
  expect(whole(picker.render(80))).toContain("説明Bの終わり");

  lines = new InputDialog(question, { message: "理由も書いてください" }, () => {}).render(80);
  expect(fits(lines)).toBe(true);
  expect(whole(lines)).toContain(question);

  const check = new CheckList(question, items, [], () => {});
  lines = check.render(80);
  expect(fits(lines)).toBe(true);
  expect(whole(lines)).toContain(question);
  expect(whole(lines)).toContain("説明Aの終わり");

  // On a short screen the question gives way to the choices, and shift+↓ brings its end into view.
  const short = new Picker(`${question.repeat(3)}質問の終わり`, items, () => {}, 10, () => 12);
  lines = short.render(80);
  expect(lines.length).toBeLessThanOrEqual(12);
  expect(plain(lines.join("\n"))).toContain("JWTに置き換え");
  expect(plain(lines.join("\n"))).toContain("shift+↑↓");
  expect(whole(lines)).not.toContain("質問の終わり");
  for (let i = 0; i < 10; i++) short.handleInput("\x1b[1;2B");
  lines = short.render(80);
  expect(lines.length).toBeLessThanOrEqual(12);
  expect(whole(lines)).toContain("質問の終わり");
});

test("a reply whose blocks shift (unsigned thinking dropped on interrupt) is redrawn, not crashed on", () => {
  const msg = (content: AssistantMessage["content"]): AssistantMessage => ({ role: "assistant", content, api: "x", provider: "t", model: "m", usage: emptyUsage(), stopReason: "aborted", timestamp: 0 });
  const tools: string[] = [];
  const v = new AssistantView((call) => void tools.push(call.id));
  const call: ToolCall = { type: "tool_call", id: "c1", name: "write", input: {} };
  v.update(msg([{ type: "thinking", thinking: "hmm" }, { type: "text", text: "partial answer" }, call]));
  expect(tools).toEqual(["c1"]);
  // The core settles the same message: thinking without a signature and the half-received call are dropped.
  expect(() => v.update(msg([{ type: "text", text: "partial answer" }]))).not.toThrow();
  const shown = v.render(80).join("\n");
  expect(shown).toContain("partial answer");
  expect(shown).not.toContain("write");
  expect(v.children).toHaveLength(1);
});

test("expanded, a long diff line wraps so its end can be read (#28)", async () => {
  const { ToolView, display } = await import("../src/views.ts");
  const view = new ToolView({ type: "tool_call", id: "1", name: "edit", input: {} } as ToolCall);
  view.status = "done";
  view.diff = `-ORIGINAL\n+START${"x".repeat(150)}SECRET_END`;
  expect(view.render(80).join("\n")).not.toContain("SECRET_END");
  display.expanded = true;
  try {
    expect(plain(view.render(80).join(""))).toContain("SECRET_END");
  } finally {
    display.expanded = false;
  }
});

test("approval: on a short screen, or with a long reason, the choices and how to refuse stay in view (#24)", async () => {
  const { ApprovalDialog } = await import("../src/dialogs.ts");
  const bashTool = { name: "bash", description: "", parameters: {}, kind: "other", matchTarget: (a: { command: string }) => a.command, execute: async () => ({ content: [] }) } as never;
  const call = { type: "tool_call" as const, id: "1", name: "bash", input: {} };
  const command = `printf ${"x".repeat(1200)}`;
  for (const [rows, reason] of [[10, "mode edits"], [22, `rule ask: bash(${command})`]] as const) {
    const dialog = new ApprovalDialog({ tool: bashTool, args: { command }, cwd: "/", reason, call }, "", () => {}, () => {}, () => rows);
    for (const down of [0, 3]) {
      for (let i = 0; i < down; i++) dialog.handleInput("\x1b[B");
      const lines = dialog.render(80);
      expect(lines.length).toBeLessThanOrEqual(rows);
      const shown = plain(lines.join("\n"));
      for (const choice of ["はい", "常に許可", "いいえ、代わりの指示を伝える", "esc で中断", "shift+↑↓"]) expect(shown).toContain(choice);
    }
  }
});

test("expanded (ctrl+o), a long transcript is not wrapped again each frame", async () => {
  // The spinner renders the whole transcript every 80 ms: 50,000 expanded lines took 110 ms a frame.
  const { Container } = await import("@earendil-works/pi-tui");
  const { display } = await import("../src/views.ts");
  const build = () => {
    const chat = new Container();
    for (let i = 0; i < 100; i++) {
      const v = new ToolView({ type: "tool_call", id: `c${i}`, name: "bash", input: { command: `run ${i}` } } as ToolCall);
      v.status = "done";
      v.output = Array.from({ length: 500 }, (_, j) => `line ${j}: R=${5000 - j} 日本語の出力`).join("\n");
      chat.addChild(v);
    }
    return new Padded(chat, 2, 2);
  };
  const root = build();
  display.expanded = true;
  try {
    const first = root.render(120);
    expect(first.length).toBeGreaterThan(50_000);
    const t0 = performance.now();
    for (let i = 0; i < 10; i++) root.render(120);
    expect(performance.now() - t0).toBeLessThan(400);
    // What a frame shows is what a fresh render shows.
    expect(root.render(120)).toEqual(build().render(120));
  } finally {
    display.expanded = false;
  }
});

test("a tool view shows what changed since its last frame", async () => {
  const { display } = await import("../src/views.ts");
  const call: ToolCall = { type: "tool_call", id: "1", name: "bash", input: { command: "ls" } };
  const view = new ToolView(call);
  view.status = "running";
  view.live = "one";
  expect(plain(view.render(60).join("\n"))).toContain("one");
  view.live = "one\ntwo";
  expect(plain(view.render(60).join("\n"))).toContain("two");
  view.status = "done";
  view.output = Array.from({ length: 10 }, (_, i) => `out ${i}`).join("\n");
  expect(plain(view.render(60).join("\n"))).toContain("+4 lines");
  display.expanded = true;
  try {
    expect(plain(view.render(60).join("\n"))).toContain("out 9");
  } finally {
    display.expanded = false;
  }
  expect(plain(view.render(60).join("\n"))).not.toContain("out 9");
  view.output = `${"y".repeat(44)}TAIL`; // 26 columns a line under the indent: TAIL stays whole on the second
  expect(plain(view.render(30).join(""))).toContain("TAIL"); // wrapped again at the new width
  view.status = "error";
  expect(view.render(30).join("")).toContain("\x1b[31m");

  // A tool's own renderer runs once for frames that show the same thing, again when its input is set in place.
  const seen: unknown[] = [];
  const custom = new ToolView({ type: "tool_call", id: "2", name: "write", input: {} } as ToolCall);
  custom.renderer = (c) => (seen.push(c.input), [`path ${(c.input as { path?: string }).path ?? "?"}`]);
  custom.render(60);
  custom.render(60);
  expect(seen).toHaveLength(1);
  custom.call.input = { path: "a.ts" };
  expect(plain(custom.render(60).join("\n"))).toContain("path a.ts");
  expect(seen).toHaveLength(2);
});

test("padding a transcript reuses the lines that did not change, and redoes them at a new width", () => {
  let lines = ["a", "b\tc", "d"];
  const child = { render: () => lines, invalidate() {} };
  const padded = new Padded(child, 2, 2);
  expect(padded.render(40)).toEqual(["  a", "  b   c", "  d"]);
  lines = ["a", "b\tc", "e"];
  expect(padded.render(40)).toEqual(["  a", "  b   c", "  e"]);
  const narrow = new Padded(child, 2, 2);
  expect(padded.render(12)).toEqual(narrow.render(12));
});

test("a bundled plugin's note to the model is one dim line, and only a message made of such notes is one", () => {
  const note = { type: "text" as const, text: harnessNote("Your task list has not been updated for 10 tool calls\nand still reads: ◐ fix bug.") };
  expect(noteText([note, note])).toStartWith("Your task list");
  expect(noteText([note, { type: "text", text: "fix the bug" }])).toBeUndefined(); // the user's own words: a prompt
  expect(noteText([])).toBeUndefined();
  const lines = new NoteView(noteText([note])!).render(40);
  expect(lines.length).toBe(1);
  expect(plain(lines[0]!)).toStartWith("  ↳ sasacode からモデルへ: Your task");
});
