import { type Component, type Focusable, Input, matchesKey, type SelectItem, SelectList, Text, truncateToWidth } from "@earendil-works/pi-tui";
import { type ApprovalAnswer, type ApprovalRequest, PermissionPolicy } from "@sasacode/agent";
import { t } from "@sasacode/host";
import { c, selectTheme } from "./theme.ts";
import { plain } from "./views.ts";

const graphemes = new Intl.Segmenter();

/** A titled SelectList that resolves once. Typing narrows the list (substring match). */
export class Picker implements Component, Focusable {
  focused = false;
  private list!: SelectList;
  private filter = "";
  /** A bracketed paste still arriving in chunks. */
  private pasted?: string;

  constructor(
    private title: string,
    private items: SelectItem[],
    private done: (item: SelectItem | undefined) => void,
    private maxVisible = 10,
  ) {
    this.rebuild();
  }

  private rebuild(): void {
    const f = this.filter.toLowerCase();
    const shown = f ? this.items.filter((i) => `${i.value} ${i.label} ${i.description ?? ""}`.toLowerCase().includes(f)) : this.items;
    // Long values (model specs) need room; descriptions get what is left.
    this.list = new SelectList(shown, this.maxVisible, selectTheme, { minPrimaryColumnWidth: 24, maxPrimaryColumnWidth: 64 });
    this.list.onSelect = (i) => this.done(i);
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
    const hint = this.filter ? t("絞り込み: {filter}", { filter: this.filter }) : t("入力で絞り込み · ↑↓ で選択 · enter で決定 · esc でキャンセル");
    return [
      truncateToWidth(c.gray("─".repeat(width)), width),
      ...this.title.split("\n").map((l, i) => truncateToWidth(i === 0 ? c.bold(l) : c.gray(l), width)),
      ...this.list.render(width),
      truncateToWidth(c.gray(hint), width),
    ];
  }

  invalidate(): void {
    this.list.invalidate();
  }
}

/**
 * Tool approval: allow once, always allow (adds a session rule), deny, or deny with instructions for the model.
 * Esc stops the whole run (`interrupt`), as it does everywhere else while the agent works. A long command
 * is shown in a window that shift+↑↓ scroll (pgup/pgdn scroll the conversation), so the choices and the one selected stay on screen.
 */
export class ApprovalDialog implements Component, Focusable {
  private _focused = false;
  private list: SelectList;
  private input?: Input;
  private title: string;
  private detail: Text;
  private footer: Text;
  private scroll = 0;
  private page = 1;

  constructor(
    req: ApprovalRequest,
    summary: string,
    done: (a: ApprovalAnswer) => void,
    interrupt: () => void,
    private maxDetailRows: () => number = () => 12,
  ) {
    const rule = PermissionPolicy.ruleFor(req);
    const raw = req.tool.name === "bash" ? String(req.args.command) : summary || JSON.stringify(req.args);
    // What is approved must be exactly what is shown: control characters are made visible, and called out.
    const warn = /[\x00-\x08\x0b-\x1f\x7f-\x9f]/.test(raw)
      ? `${c.red(t("⚠ 制御文字（端末のエスケープシーケンスなど）を含んでいます。␛ などの表示を確かめてください。"))}\n`
      : "";
    this.title = `${c.yellow("?")} ${c.bold(t("{tool} を実行しますか？", { tool: req.tool.name }))}`;
    this.detail = new Text(c.cyan(plain(raw)), 0, 0);
    this.footer = new Text(`${warn}${c.gray(plain(req.reason))}`, 0, 0);
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
    const detail = this.detail.render(width);
    const rows = Math.max(1, this.maxDetailRows());
    let shown = detail;
    let more: string[] = [];
    if (detail.length > rows) {
      this.page = Math.max(1, rows - 1);
      this.scroll = Math.min(this.scroll, detail.length - rows);
      shown = detail.slice(this.scroll, this.scroll + rows);
      more = [
        truncateToWidth(
          c.gray(t("{from}–{to} / {total} 行 · shift+↑↓ で全文をスクロール", { from: this.scroll + 1, to: this.scroll + shown.length, total: detail.length })),
          width,
        ),
      ];
    }
    const lines = [truncateToWidth(c.yellow("─".repeat(width)), width), truncateToWidth(this.title, width), ...shown, ...more, ...this.footer.render(width)];
    if (this.input) lines.push(c.gray(t("モデルへの指示（enter で送信 · esc で選択に戻る）:")), ...this.input.render(width));
    else lines.push(...this.list.render(width), truncateToWidth(c.gray(t("esc で中断")), width));
    return lines;
  }

  invalidate(): void {
    this.detail.invalidate();
    this.footer.invalidate();
    this.list.invalidate();
  }
}

/** One line of free text (api.ui.input). Enter submits what was typed, esc cancels. */
export class InputDialog implements Component, Focusable {
  private _focused = false;
  private input: Input;

  constructor(
    private title: string,
    private options: { message?: string; placeholder?: string; initial?: string },
    done: (value: string | undefined) => void,
  ) {
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
    this.input.handleInput(data);
  }

  render(width: number): string[] {
    const lines = plain(this.title).split("\n");
    const message = this.options.message === undefined ? [] : plain(this.options.message).split("\n");
    return [
      truncateToWidth(c.gray("─".repeat(width)), width),
      ...lines.map((l, i) => truncateToWidth(i === 0 ? c.bold(l) : c.gray(l), width)),
      ...message.map((l) => truncateToWidth(c.gray(l), width)),
      ...this.input.render(width),
      truncateToWidth(c.gray(t("enter で決定 · esc でキャンセル")), width),
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

  constructor(
    private title: string,
    private items: SelectItem[],
    selected: string[],
    private done: (values: string[] | undefined) => void,
    private maxVisible = 10,
  ) {
    this.checked = new Set(selected.filter((v) => items.some((i) => i.value === v)));
  }

  handleInput(data: string): void {
    const n = this.items.length;
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
    const rows = shown.map((item, k) => {
      const at = start + k === this.cursor;
      const box = this.checked.has(item.value) ? "[x]" : "[ ]";
      const label = `${at ? "›" : " "} ${box} ${plain(item.label)}`;
      const desc = item.description ? `  ${c.gray(plain(item.description))}` : "";
      return truncateToWidth((at ? c.bold(c.cyan(label)) : label) + desc, width);
    });
    if (this.items.length > this.maxVisible) rows.push(truncateToWidth(c.gray(`(${this.cursor + 1}/${this.items.length})`), width));
    return [
      truncateToWidth(c.gray("─".repeat(width)), width),
      ...plain(this.title).split("\n").map((l, i) => truncateToWidth(i === 0 ? c.bold(l) : c.gray(l), width)),
      ...rows,
      truncateToWidth(c.gray(t("space で選択/解除 · ↑↓ で移動 · enter で決定 · esc でキャンセル")), width),
    ];
  }

  invalidate(): void {}
}
