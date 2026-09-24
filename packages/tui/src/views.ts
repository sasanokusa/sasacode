import { type Component, Container, Markdown, Text, truncateToWidth } from "@earendil-works/pi-tui";
import type { AssistantMessage, ToolCall, UserContent } from "@sasacode/ai";
import type { ToolRenderer, ToolResult } from "@sasacode/plugin-api";
import { c, markdownTheme } from "./theme.ts";

/** Shared flag toggled by ctrl+o: show full tool output and thinking. */
export const display = { expanded: false };

const COLLAPSED_LINES = 6;

export class UserView extends Text {
  constructor(content: UserContent[]) {
    const text = content
      .map((b) => (b.type === "text" ? b.text : "[image]"))
      .filter((t) => !t.startsWith("[The user interrupted"))
      .join("\n");
    super(c.bgUser(`> ${text}`), 0, 0);
  }
}

class ThinkingView implements Component {
  text = "";
  render(width: number): string[] {
    const lines = this.text.trim().split("\n").filter((l) => l.trim());
    if (!this.text.trim()) return [];
    const shown = display.expanded ? lines : lines.slice(-2);
    const out = new Text(c.dim(c.italic(`∴ ${shown.join("\n  ")}`)), 0, 0).render(width);
    if (!display.expanded && lines.length > 2) out.unshift(truncateToWidth(c.gray(`∴ thinking… (${lines.length} lines, ctrl+o)`), width));
    return out;
  }
  invalidate() {}
}

/** An assistant turn: thinking, markdown text, and tool calls in stream order. */
export class AssistantView extends Container {
  private blocks = new Map<number, Component>();
  private tools = new Map<string, ToolView>();

  constructor(private onTool: (call: ToolCall, view: ToolView) => void) {
    super();
  }

  update(message: AssistantMessage): void {
    message.content.forEach((b, i) => {
      if (!b) return;
      let view = this.blocks.get(i);
      if (b.type === "text") {
        if (!view) this.add(i, (view = new Markdown("", 0, 0, markdownTheme)));
        (view as Markdown).setText(b.text);
      } else if (b.type === "thinking") {
        if (!view) this.add(i, (view = new ThinkingView()));
        (view as ThinkingView).text = b.thinking;
      } else {
        if (!view) {
          const tv = new ToolView(b);
          this.add(i, (view = tv));
          this.tools.set(b.id, tv);
          this.onTool(b, tv);
        }
        (view as ToolView).call = b;
      }
    });
    this.invalidate();
  }

  private add(i: number, v: Component): void {
    this.blocks.set(i, v);
    this.addChild(v);
  }
}

type ToolStatus = "pending" | "running" | "done" | "error";

export class ToolView implements Component {
  status: ToolStatus = "pending";
  summary = "";
  live = "";
  output = "";
  diff?: string;
  result?: ToolResult;
  renderer?: ToolRenderer;

  constructor(public call: ToolCall) {}

  render(width: number): string[] {
    const dot = { pending: c.gray("○"), running: c.yellow("●"), done: c.green("●"), error: c.red("●") }[this.status];
    const summary = this.summary || argPreview(this.call.input);
    const lines = [truncateToWidth(`${dot} ${c.bold(this.call.name)}${c.gray(`(${summary})`)}`, width)];
    const body = this.status === "running" ? this.live : this.output;
    let custom: string[] | undefined;
    if (this.renderer && this.status !== "running")
      try {
        custom = this.renderer(this.call, this.result, { expanded: display.expanded, width })?.map((l) => truncateToWidth(l, width));
      } catch {}
    if (custom) lines.push(...custom);
    else if (this.diff && this.status === "done") lines.push(...renderDiff(this.diff, width));
    else if (body.trim()) {
      const all = body.trimEnd().split("\n");
      const shown = display.expanded ? all : this.status === "running" ? all.slice(-COLLAPSED_LINES) : all.slice(0, COLLAPSED_LINES);
      const color = this.status === "error" ? c.red : c.gray;
      for (const [i, l] of shown.entries())
        lines.push(...new Text(color(`${i === 0 ? "  ⎿ " : "    "}${l}`), 0, 0).render(width));
      if (!display.expanded && all.length > shown.length)
        lines.push(truncateToWidth(c.gray(`    … +${all.length - shown.length} lines (ctrl+o)`), width));
    }
    return lines;
  }

  invalidate() {}
}

function argPreview(input: Record<string, unknown>): string {
  const s = JSON.stringify(input);
  return s.length > 80 ? `${s.slice(0, 77)}…` : s;
}

function renderDiff(diff: string, width: number): string[] {
  const all = diff.split("\n");
  const shown = display.expanded ? all : all.slice(0, 16);
  const out = shown.map((l) => {
    const line = truncateToWidth(`    ${l}`, width);
    if (l.startsWith("+")) return c.green(line);
    if (l.startsWith("-")) return c.red(line);
    if (l.startsWith("@@")) return c.cyan(line);
    return c.gray(line);
  });
  if (all.length > shown.length) out.push(truncateToWidth(c.gray(`    … +${all.length - shown.length} lines (ctrl+o)`), width));
  return out;
}

export class Notice extends Text {
  constructor(text: string, color: (s: string) => string = c.gray) {
    super(color(text), 0, 0);
  }
}
