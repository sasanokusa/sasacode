import { type Component, type Focusable, Input, Markdown, matchesKey, Text, truncateToWidth } from "@earendil-works/pi-tui";
import type { ShowTextOptions } from "@sasacode/plugin-api";
import { c, markdownTheme } from "./theme.ts";
import { plain } from "./views.ts";

/** Read-only pager. Content is data: no HTML execution, link opening or automatic clipboard writes. */
export class TextViewer implements Component, Focusable {
  focused = false;
  private offset = 0;
  private query = "";
  private matchIndex = 0;
  private input?: Input;
  private lines: string[] = [];
  private width = 0;
  private note = "";
  private closed = false;
  private readonly source: string;
  private readonly body: Text | Markdown;

  constructor(private options: ShowTextOptions, private done: () => void,
    private copy: (text: string) => Promise<unknown>, private redraw: () => void,
    private height: () => number = () => 16, private lang: "ja" | "en" = "ja") {
    this.source = plain(options.text);
    this.body = options.format === "markdown" ? new Markdown(this.source, 0, 0, markdownTheme) : new Text(this.source, 0, 0);
  }

  handleInput(data: string): void {
    if (this.closed) return;
    if (this.input) {
      if (matchesKey(data, "escape")) { this.input = undefined; return; }
      this.input.handleInput(data);
      return;
    }
    const page = Math.max(1, this.height());
    if (matchesKey(data, "escape") || data === "q") { this.closed = true; this.done(); }
    else if (matchesKey(data, "up") || data === "k") this.offset--;
    else if (matchesKey(data, "down") || data === "j") this.offset++;
    else if (matchesKey(data, "pageUp")) this.offset -= page;
    else if (matchesKey(data, "pageDown") || data === " ") this.offset += page;
    else if (matchesKey(data, "home") || data === "g") this.offset = 0;
    else if (matchesKey(data, "end") || data === "G") this.offset = this.lines.length - page;
    else if (data === "/") {
      this.input = new Input(); this.input.focused = true;
      this.input.onSubmit = (query) => { this.query = plain(query).toLowerCase(); this.matchIndex = this.offset; this.input = undefined; this.find(1, true); };
    } else if (data === "n") this.find(1);
    else if (data === "N") this.find(-1);
    else if (data === "y") {
      this.note = this.lang === "ja" ? "コピー中…" : "Copying…";
      void this.copy(this.source).then((method) => {
        this.note = method === "terminal" ? (this.lang === "ja" ? "端末にコピーを要求しました" : "Copy requested from terminal") : (this.lang === "ja" ? "コピーしました" : "Copied"); this.redraw();
      }, () => { this.note = this.lang === "ja" ? "コピーに失敗しました" : "Copy failed"; this.redraw(); });
    }
    this.offset = Math.max(0, Math.min(this.offset, Math.max(0, this.lines.length - page)));
  }

  private find(direction: number, includeCurrent = false): void {
    if (!this.query || !this.lines.length) return;
    for (let step = includeCurrent ? 0 : 1; step <= this.lines.length; step++) {
      const index = (this.matchIndex + direction * step + this.lines.length) % this.lines.length;
      const text = this.lines[index].replace(/\x1b\[[0-9;]*m/g, "").toLowerCase();
      if (text.includes(this.query)) { this.offset = this.matchIndex = index; this.note = `/${this.query}`; return; }
    }
    this.note = this.lang === "ja" ? "見つかりません" : "No match";
  }

  render(width: number): string[] {
    if (width !== this.width) { this.width = width; this.lines = this.body.render(Math.max(1, width)).map((line) =>
      // Even renderer-generated OSC hyperlinks are display-only here. Keep only SGR styling.
      line.replace(/\x1b\][\s\S]*?(?:\x07|\x1b\\)/g, "").replace(/\x1b(?!\[[0-9;]*m)/g, "␛")); }
    const page = Math.max(1, this.height());
    this.offset = Math.max(0, Math.min(this.offset, Math.max(0, this.lines.length - page)));
    const hint = this.lang === "ja" ? "↑↓/PgUp/PgDn:移動 /:検索 n/N:次/前 y:コピー esc:閉じる" : "↑↓/PgUp/PgDn:scroll /:search n/N:next/prev y:copy esc:close";
    const footer = `${this.offset + 1}–${Math.min(this.offset + page, this.lines.length)}/${this.lines.length} ${this.note}`;
    return [truncateToWidth(c.bold(plain(this.options.title)), width),
      ...this.lines.slice(this.offset, this.offset + page),
      ...(this.input ? this.input.render(width) : [truncateToWidth(c.gray(hint), width)]),
      truncateToWidth(c.gray(footer), width)];
  }

  invalidate(): void { this.width = 0; this.body.invalidate(); }
}
