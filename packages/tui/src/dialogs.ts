import { type Component, type Focusable, Input, type SelectItem, SelectList, Text, truncateToWidth } from "@earendil-works/pi-tui";
import { type ApprovalAnswer, type ApprovalRequest, PermissionPolicy } from "@sasacode/agent";
import { c, selectTheme } from "./theme.ts";

/** A titled SelectList that resolves once. */
export class Picker implements Component, Focusable {
  focused = false;
  private list: SelectList;
  private title: string;

  constructor(title: string, items: SelectItem[], done: (item: SelectItem | undefined) => void, maxVisible = 10) {
    this.title = title;
    this.list = new SelectList(items, maxVisible, selectTheme);
    this.list.onSelect = (i) => done(i);
    this.list.onCancel = () => done(undefined);
  }

  handleInput(data: string): void {
    this.list.handleInput?.(data);
  }

  render(width: number): string[] {
    return [
      truncateToWidth(c.gray("─".repeat(width)), width),
      ...this.title.split("\n").map((l, i) => truncateToWidth(i === 0 ? c.bold(l) : c.gray(l), width)),
      ...this.list.render(width),
      truncateToWidth(c.gray("↑↓ で選択 · enter で決定 · esc でキャンセル"), width),
    ];
  }

  invalidate(): void {
    this.list.invalidate();
  }
}

/** Tool approval: allow once, always allow (adds a session rule), deny, or deny with instructions for the model. */
export class ApprovalDialog implements Component, Focusable {
  private _focused = false;
  private list: SelectList;
  private input?: Input;
  private header: Text;

  constructor(req: ApprovalRequest, summary: string, done: (a: ApprovalAnswer) => void) {
    const rule = PermissionPolicy.ruleFor(req);
    const detail = req.tool.name === "bash" ? String(req.args.command) : summary || JSON.stringify(req.args);
    this.header = new Text(
      `${c.yellow("?")} ${c.bold(`${req.tool.name} を実行しますか？`)}\n${c.cyan(detail)}\n${c.gray(req.reason)}`,
      0,
      0,
    );
    this.list = new SelectList(
      [
        { value: "allow", label: "はい" },
        { value: "always", label: "常に許可（このセッション中）", description: rule },
        { value: "deny", label: "いいえ" },
        { value: "feedback", label: "いいえ、代わりの指示を伝える" },
      ],
      4,
      selectTheme,
    );
    this.list.onSelect = (item) => {
      if (item.value === "feedback") {
        this.input = new Input();
        this.input.focused = this._focused;
        this.input.onSubmit = (text) => done({ decision: "deny", feedback: text.trim() || undefined });
        return;
      }
      done({ decision: item.value as ApprovalAnswer["decision"] });
    };
    this.list.onCancel = () => done({ decision: "deny" });
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
    else this.list.handleInput?.(data);
  }

  render(width: number): string[] {
    const lines = [truncateToWidth(c.yellow("─".repeat(width)), width), ...this.header.render(width)];
    if (this.input) lines.push(c.gray("モデルへの指示（enter で送信）:"), ...this.input.render(width));
    else lines.push(...this.list.render(width), truncateToWidth(c.gray("esc で拒否"), width));
    return lines;
  }

  invalidate(): void {
    this.header.invalidate();
    this.list.invalidate();
  }
}
