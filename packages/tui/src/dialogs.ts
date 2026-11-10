import {
  type Component,
  type Focusable,
  Input,
  matchesKey,
  type SelectItem,
  SelectList,
  stripTerminalSequences,
  Text,
  truncateToWidth,
  visibleWidth,
  wrapTextWithAnsi,
} from "@earendil-works/pi-tui";
import { type ApprovalAnswer, type ApprovalRequest, PermissionPolicy } from "@sasacode/agent";
import { t } from "@sasacode/host";
import { c, selectTheme } from "./theme.ts";
import { plain } from "./views.ts";

const graphemes = new Intl.Segmenter();

/**
 * A dialog's title (and message), wrapped so all of it can be read: ask_user's question runs to 500
 * characters (#111). On a short screen it gets the rows the rest of the dialog leaves, and scrolls
 * with shift+↑↓ as an approval does, so the choices stay in view.
 */
class Heading {
  private scroll = 0;
  private page = 1;

  /** `lines`: styled already, each wrapped on its own. */
  constructor(private lines: string[]) {}

  /** shift+↑↓ scroll the heading; any other key is the dialog's. */
  handleInput(data: string): boolean {
    if (matchesKey(data, "shift+up")) this.scroll = Math.max(0, this.scroll - this.page);
    else if (matchesKey(data, "shift+down")) this.scroll += this.page;
    else return false;
    return true;
  }

  /** At most `room` rows, and the hint saying where they are when that is not all of it. */
  render(width: number, room = Infinity): { lines: string[]; where?: string } {
    const all = this.lines.flatMap((l) => wrapTextWithAnsi(l, width));
    room = Math.max(1, room);
    if (all.length <= room) return { lines: all };
    this.page = Math.max(1, room - 1);
    this.scroll = Math.min(this.scroll, all.length - room);
    const lines = all.slice(this.scroll, this.scroll + room);
    return { lines, where: t("{from}–{to} / {total} 行 · shift+↑↓ で全文をスクロール", { from: this.scroll + 1, to: this.scroll + lines.length, total: all.length }) };
  }
}

/** A title's lines: the first in bold, the rest (a message under it) in gray. */
const titleLines = (title: string) => plain(title).split("\n").map((l, i) => (i === 0 ? c.bold(l) : c.gray(l)));

/** An option's description in full under the list, wrapped: its row had room for only part of it (#111). */
const fullDescription = (description: string, width: number) => wrapTextWithAnsi(c.gray(description), Math.max(1, width - 4)).map((l) => `    ${l}`);

/** A titled SelectList that resolves once. Typing narrows the list (substring match). */
export class Picker implements Component, Focusable {
  focused = false;
  private list!: SelectList;
  private filter = "";
  /** A bracketed paste still arriving in chunks. */
  private pasted?: string;
  private heading: Heading;

  /** `rows`: the most the dialog may take (the title gives way and scrolls); unlimited without it. */
  constructor(
    title: string,
    private items: SelectItem[],
    private done: (item: SelectItem | undefined) => void,
    private maxVisible = 10,
    private rows?: () => number,
  ) {
    this.heading = new Heading(titleLines(title));
    this.rebuild();
  }

  private rebuild(): void {
    const f = this.filter.toLowerCase();
    const shown = f ? this.items.filter((i) => `${i.value} ${i.label} ${i.description ?? ""}`.toLowerCase().includes(f)) : this.items;
    // Shown made safe: an option (ask_user's) may come from the model, and its escape sequences must not reach the terminal (#110).
    const safe = new Map<SelectItem, SelectItem>(shown.map((i) => [{ ...i, label: plain(i.label), description: i.description === undefined ? undefined : plain(i.description) }, i]));
    // Long values (model specs) need room; descriptions get what is left.
    this.list = new SelectList([...safe.keys()], this.maxVisible, selectTheme, { minPrimaryColumnWidth: 24, maxPrimaryColumnWidth: 64 });
    this.list.onSelect = (i) => this.done(safe.get(i) ?? i);
    this.list.onCancel = () => this.done(undefined);
  }

  handleInput(data: string): void {
    if (this.pasted !== undefined || data.includes("\x1b[200~")) {
      // A paste narrows the list like typing does; line breaks and control characters are left out.
      const start = data.indexOf("\x1b[200~");
      this.pasted = (this.pasted ?? "") + (start >= 0 ? data.slice(start + 6) : data);
      const end = this.pasted.indexOf("\x1b[201~");
      if (end < 0) return;
      this.filter += this.pasted.slice(0, end).replace(/[\x00-\x1f\x7f]+/g, " ").trim();
      this.pasted = undefined;
      this.rebuild();
      return;
    }
    if (this.heading.handleInput(data)) return;
    if (matchesKey(data, "backspace")) {
      // A whole character, an emoji too: half of one would be left as a broken one.
      this.filter = [...graphemes.segment(this.filter)].slice(0, -1).map((g) => g.segment).join("");
      this.rebuild();
    } else if (!data.startsWith("\x1b") && [...data].every((ch) => ch >= " " && ch !== "\x7f")) {
      this.filter += data;
      this.rebuild();
    } else this.list.handleInput?.(data);
  }

  render(width: number): string[] {
    const list = this.list.render(width);
    // The selected option's description, when its row cut it short.
    const description = this.list.getSelectedItem()?.description?.replace(/[\r\n]+/g, " ").trim();
    const row = list.map((l) => stripTerminalSequences(l)).find((l) => l.startsWith("→ "));
    const detail = description && !row?.includes(description) ? fullDescription(description, width) : [];
    const heading = this.heading.render(width, this.rows ? this.rows() - list.length - detail.length - 2 : undefined);
    const hint = this.filter ? t("絞り込み: {filter}", { filter: this.filter }) : t("入力で絞り込み · ↑↓ で選択 · enter で決定 · esc でキャンセル");
    return [
      truncateToWidth(c.gray("─".repeat(width)), width),
      ...heading.lines,
      ...list,
      ...detail,
      truncateToWidth(c.gray(heading.where ? `${heading.where} · ${hint}` : hint), width),
    ];
  }

  invalidate(): void {
    this.list.invalidate();
  }
}

/**
 * Tool approval: allow once, always allow (adds a session rule), deny, or deny with instructions for the model.
 * Esc stops the whole run (`interrupt`), as it does everywhere else while the agent works. What is asked
 * about (the command, a warning, the reason) shares the rows the choices leave on the screen (`rows`);
 * when it needs more it scrolls with shift+↑↓ (pgup/pgdn scroll the conversation), so the choices, the
 * one selected and how to refuse always stay in view.
 */
export class ApprovalDialog implements Component, Focusable {
  private _focused = false;
  private list: SelectList;
  private input?: Input;
  private title: string;
  private body: Text;
  private scroll = 0;
  private page = 1;

  constructor(
    req: ApprovalRequest,
    summary: string,
    done: (a: ApprovalAnswer) => void,
    interrupt: () => void,
    private rows: () => number = () => 24,
  ) {
    const rule = PermissionPolicy.ruleFor(req);
    const raw = req.tool.name === "bash" ? String(req.args.command) : summary || JSON.stringify(req.args);
    // What is approved must be exactly what is shown: control characters are made visible, and called out.
    const warn = /[\x00-\x08\x0b-\x1f\x7f-\x9f]/.test(raw)
      ? `\n${c.red(t("⚠ 制御文字（端末のエスケープシーケンスなど）を含んでいます。␛ などの表示を確かめてください。"))}`
      : "";
    this.title = `${c.yellow("?")} ${c.bold(t("{tool} を実行しますか？", { tool: req.tool.name }))}`;
    this.body = new Text(`${c.cyan(plain(raw))}${warn}\n${c.gray(plain(req.reason))}`, 0, 0);
    this.list = new SelectList(
      [
        { value: "allow", label: t("はい") },
        { value: "always", label: t("常に許可（終了するまで）"), description: plain(rule) },
        { value: "deny", label: t("いいえ") },
        { value: "feedback", label: t("いいえ、代わりの指示を伝える") },
      ],
      4,
      selectTheme,
    );
    this.list.onSelect = (item) => {
      if (item.value === "feedback") {
        this.input = new Input();
        this.input.focused = this._focused;
        this.input.onSubmit = (text) => done({ decision: "deny", feedback: text.trim() || undefined });
        // Back to the choices: the instructions were not what the user wanted after all.
        this.input.onEscape = () => {
          this.input = undefined;
        };
        return;
      }
      done({ decision: item.value as ApprovalAnswer["decision"] });
    };
    this.list.onCancel = interrupt;
  }

  get focused(): boolean {
    return this._focused;
  }
  set focused(v: boolean) {
    this._focused = v;
    if (this.input) this.input.focused = v;
  }

  handleInput(data: string): void {
    if (this.input) this.input.handleInput(data);
    else if (matchesKey(data, "shift+up")) this.scroll = Math.max(0, this.scroll - this.page);
    else if (matchesKey(data, "shift+down")) this.scroll += this.page;
    else this.list.handleInput?.(data);
  }

  render(width: number): string[] {
    const choices = this.input
      ? [c.gray(t("モデルへの指示（enter で送信 · esc で選択に戻る）:")), ...this.input.render(width)]
      : this.list.render(width);
    const body = this.body.render(width);
    // The rule, the title and the hint line take one row each.
    const room = Math.max(1, this.rows() - choices.length - 3);
    let shown = body;
    let hint = this.input ? "" : t("esc で中断");
    if (body.length > room) {
      this.page = Math.max(1, room - 1);
      this.scroll = Math.min(this.scroll, body.length - room);
      shown = body.slice(this.scroll, this.scroll + room);
      const where = t("{from}–{to} / {total} 行 · shift+↑↓ で全文をスクロール", { from: this.scroll + 1, to: this.scroll + shown.length, total: body.length });
      hint = hint ? `${hint} · ${where}` : where;
    }
    const lines = [truncateToWidth(c.yellow("─".repeat(width)), width), truncateToWidth(this.title, width), ...shown, ...choices];
    if (hint) lines.push(truncateToWidth(c.gray(hint), width));
    return lines;
  }

  invalidate(): void {
    this.body.invalidate();
    this.list.invalidate();
  }
}

/** One line of free text (api.ui.input). Enter submits what was typed, esc cancels. */
export class InputDialog implements Component, Focusable {
  private _focused = false;
  private input: Input;
  private heading: Heading;

  constructor(
    title: string,
    options: { message?: string; placeholder?: string; initial?: string },
    done: (value: string | undefined) => void,
    private rows?: () => number,
  ) {
    this.heading = new Heading([...titleLines(title), ...(options.message === undefined ? [] : plain(options.message).split("\n").map((l) => c.gray(l)))]);
    this.input = new Input({ placeholder: options.placeholder ? plain(options.placeholder) : undefined, placeholderStyle: c.gray });
    // As a paste, so the cursor ends up after it and control characters are left out.
    if (options.initial) this.input.handleInput(`\x1b[200~${options.initial.replace(/\x1b/g, "")}\x1b[201~`);
    this.input.onSubmit = (value) => done(value);
    this.input.onEscape = () => done(undefined);
  }

  get focused(): boolean {
    return this._focused;
  }
  set focused(v: boolean) {
    this._focused = v;
    this.input.focused = v;
  }

  handleInput(data: string): void {
    if (!this.heading.handleInput(data)) this.input.handleInput(data);
  }

  render(width: number): string[] {
    const input = this.input.render(width);
    const heading = this.heading.render(width, this.rows ? this.rows() - input.length - 2 : undefined);
    const hint = t("enter で決定 · esc でキャンセル");
    return [
      truncateToWidth(c.gray("─".repeat(width)), width),
      ...heading.lines,
      ...input,
      truncateToWidth(c.gray(heading.where ? `${heading.where} · ${hint}` : hint), width),
    ];
  }

  invalidate(): void {
    this.input.invalidate();
  }
}

/** Check any number of items (api.ui.selectMany). Space toggles, enter confirms, esc cancels. */
export class CheckList implements Component, Focusable {
  focused = false;
  private cursor = 0;
  private checked: Set<string>;
  private heading: Heading;

  constructor(
    title: string,
    private items: SelectItem[],
    selected: string[],
    private done: (values: string[] | undefined) => void,
    private maxVisible = 10,
    private rows?: () => number,
  ) {
    this.heading = new Heading(titleLines(title));
    this.checked = new Set(selected.filter((v) => items.some((i) => i.value === v)));
  }

  handleInput(data: string): void {
    const n = this.items.length;
    if (this.heading.handleInput(data)) return;
    if (matchesKey(data, "escape")) this.done(undefined);
    else if (matchesKey(data, "enter")) this.done(this.items.map((i) => i.value).filter((v) => this.checked.has(v)));
    else if (!n) return;
    else if (matchesKey(data, "up")) this.cursor = (this.cursor - 1 + n) % n;
    else if (matchesKey(data, "down")) this.cursor = (this.cursor + 1) % n;
    else if (matchesKey(data, "space")) {
      const v = this.items[this.cursor]!.value;
      if (!this.checked.delete(v)) this.checked.add(v);
    }
  }

  render(width: number): string[] {
    const start = Math.max(0, Math.min(this.cursor - Math.floor(this.maxVisible / 2), this.items.length - this.maxVisible));
    const shown = this.items.slice(start, start + this.maxVisible);
    let detail: string[] = [];
    const rows = shown.map((item, k) => {
      const at = start + k === this.cursor;
      const box = this.checked.has(item.value) ? "[x]" : "[ ]";
      const label = `${at ? "›" : " "} ${box} ${plain(item.label)}`;
      const desc = item.description ? `  ${c.gray(plain(item.description))}` : "";
      const row = (at ? c.bold(c.cyan(label)) : label) + desc;
      // The description of the option at the cursor in full, when its row cuts it short.
      if (at && desc && visibleWidth(row) > width) detail = fullDescription(plain(item.description!).replace(/[\r\n]+/g, " ").trim(), width);
      return truncateToWidth(row, width);
    });
    rows.push(...detail);
    if (this.items.length > this.maxVisible) rows.push(truncateToWidth(c.gray(`(${this.cursor + 1}/${this.items.length})`), width));
    const heading = this.heading.render(width, this.rows ? this.rows() - rows.length - 2 : undefined);
    const hint = t("space で選択/解除 · ↑↓ で移動 · enter で決定 · esc でキャンセル");
    return [
      truncateToWidth(c.gray("─".repeat(width)), width),
      ...heading.lines,
      ...rows,
      truncateToWidth(c.gray(heading.where ? `${heading.where} · ${hint}` : hint), width),
    ];
  }

  invalidate(): void {}
}
