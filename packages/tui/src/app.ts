import { appendFileSync, readFileSync } from "node:fs";
import {
  CombinedAutocompleteProvider,
  type Component,
  Container,
  Editor,
  Loader,
  matchesKey,
  ProcessTerminal,
  Spacer,
  type TUI,
  truncateToWidth,
  TuiMainScreen,
} from "@earendil-works/pi-tui";
import {
  type Agent,
  type AgentEvent,
  type ApprovalAnswer,
  type ApprovalRequest,
  listSessions,
  PERMISSION_MODE_LABELS,
  PERMISSION_MODES,
  type PermissionMode,
} from "@sasacode/agent";
import { type ModelInfo, textOf, type ToolCall } from "@sasacode/ai";
import type { CommandDefinition } from "@sasacode/plugin-api";
import { ApprovalDialog, Picker } from "./dialogs.ts";
import { c, editorTheme } from "./theme.ts";
import { AssistantView, display, Notice, ToolView, UserView } from "./views.ts";

/** What the TUI needs from the CLI. */
export interface TuiHost {
  agent: Agent;
  warnings: string[];
  commands: CommandDefinition[];
  config: { models?: string[] };
  resolve(spec: string): ModelInfo;
  sessionsDir: string;
  historyPath?: string;
  loadSession(path: string): void;
  newSession(): void;
}

export async function runTui(host: TuiHost, initialPrompt = ""): Promise<number> {
  const app = new App(host);
  return app.run(initialPrompt);
}

class App {
  private tui: TUI;
  private chat = new Container();
  private status = new Container();
  private inputSlot = new Container();
  private footer = new Line();
  private editor: Editor;
  private loader?: Loader;
  private current?: AssistantView;
  private tools = new Map<string, ToolView>();
  private queued: string[] = [];
  private totalCost = 0;
  private contextTokens = 0;
  private lastCtrlC = 0;
  private exit?: (code: number) => void;
  private commands: CommandDefinition[];

  constructor(private host: TuiHost) {
    this.tui = new TuiMainScreen(new ProcessTerminal());
    this.editor = new Editor(this.tui, editorTheme, { paddingX: 1 });
    this.commands = [...this.coreCommands(), ...host.commands];
    this.editor.setAutocompleteProvider(
      new CombinedAutocompleteProvider(
        this.commands.map((cmd) => ({ name: cmd.name, description: cmd.description, argumentHint: cmd.argumentHint })),
        host.agent.cwd,
      ),
    );
    this.editor.onSubmit = (text) => this.submit(text);
    for (const h of this.loadHistory()) this.editor.addToHistory(h);

    const header = new Notice(`${c.bold("sasacode")} ${c.gray(host.agent.cwd)}\n${c.gray("/help でコマンド一覧 · esc で中断 · ctrl+o で詳細表示 · shift+tab で権限モード切替")}`);
    for (const w of host.warnings) this.chat.addChild(new Notice(`warning: ${w}`, c.yellow));
    this.tui.addChild(header);
    this.tui.addChild(this.chat);
    this.tui.addChild(this.status);
    this.tui.addChild(this.inputSlot);
    this.tui.addChild(this.footer);
    this.showEditor();

    host.agent.approve = (req) => this.askApproval(req);
    host.agent.events.on((e) => this.onEvent(e));
    this.tui.addInputListener((data) => this.onKey(data));
    this.renderHistory();
    this.updateFooter();
  }

  run(initialPrompt: string): Promise<number> {
    this.tui.start();
    if (initialPrompt.trim()) this.submit(initialPrompt);
    return new Promise((resolve) => {
      this.exit = (code) => {
        this.tui.stop();
        resolve(code);
      };
    });
  }

  // ── input ──────────────────────────────────────────────────────────

  private onKey(data: string): { consume: boolean } | undefined {
    const agent = this.host.agent;
    if (
      matchesKey(data, "escape") &&
      agent.isRunning &&
      this.inputSlot.children[0] === this.editor &&
      !this.editor.isShowingAutocomplete()
    ) {
      agent.abort();
      return { consume: true };
    }
    if (matchesKey(data, "ctrl+c")) {
      if (agent.isRunning) agent.abort();
      else if (this.editor.getText()) this.editor.setText("");
      else if (Date.now() - this.lastCtrlC < 1500) this.exit?.(0);
      else {
        this.lastCtrlC = Date.now();
        this.notify("もう一度 ctrl+c で終了します");
      }
      return { consume: true };
    }
    if (matchesKey(data, "ctrl+d") && !this.editor.getText() && !agent.isRunning) {
      this.exit?.(0);
      return { consume: true };
    }
    if (matchesKey(data, "ctrl+o")) {
      display.expanded = !display.expanded;
      this.chat.invalidate();
      this.tui.requestRender(true);
      return { consume: true };
    }
    if (matchesKey(data, "shift+tab") && this.inputSlot.children[0] === this.editor) {
      const modes = PERMISSION_MODES;
      this.setMode(modes[(modes.indexOf(agent.permissions.mode) + 1) % modes.length]!);
      return { consume: true };
    }
    return undefined;
  }

  /** The editor hands over text with paste markers already expanded, and clears itself. */
  private submit(text: string): void {
    if (!text.trim()) return;
    this.editor.addToHistory(text);
    this.saveHistory(text);
    if (text.startsWith("/")) {
      void this.runCommand(text);
      return;
    }
    const agent = this.host.agent;
    if (agent.isRunning) {
      this.queued.push(text);
      this.renderStatus();
    }
    void agent.prompt(text);
  }

  // ── agent events ───────────────────────────────────────────────────

  private onEvent(e: AgentEvent): void {
    switch (e.type) {
      case "agent_start":
        this.loader = new Loader(this.tui, c.cyan, c.gray, "考え中… (esc で中断)");
        this.loader.start();
        this.renderStatus();
        break;
      case "message_start":
        this.current = new AssistantView((call, view) => this.tools.set(call.id, view));
        this.chat.addChild(new Spacer(1));
        this.chat.addChild(this.current);
        break;
      case "message_update":
        this.current?.update(e.message);
        break;
      case "message_end": {
        const m = e.message;
        if (m.role === "user") {
          const t = textOf(m.content);
          const qi = this.queued.findIndex((q) => t.endsWith(q));
          if (qi >= 0) this.queued.splice(qi, 1);
          this.chat.addChild(new Spacer(1));
          this.chat.addChild(new UserView(m.content));
          this.renderStatus();
        } else if (m.role === "assistant") {
          this.current?.update(m);
          for (const b of m.content) if (b.type === "tool_call") this.describeTool(b);
          this.totalCost += m.usage.cost;
          const ctx = m.usage.input + m.usage.cacheRead + m.usage.cacheWrite + m.usage.output;
          if (ctx) this.contextTokens = ctx;
          if (m.stopReason === "refusal") this.chat.addChild(new Notice("モデルが応答を拒否しました", c.yellow));
          if (m.stopReason === "max_tokens") this.chat.addChild(new Notice("出力が max_tokens に達しました", c.yellow));
          this.current = undefined;
          this.updateFooter();
        } else {
          const view = this.tools.get(m.toolCallId);
          if (view && view.status !== "done" && view.status !== "error") {
            view.output = textOf(m.content);
            view.status = m.isError ? "error" : "done";
          }
        }
        break;
      }
      case "tool_start": {
        const v = this.tools.get(e.call.id);
        if (v) {
          v.status = "running";
          if (e.summary) v.summary = e.summary;
        }
        this.loader?.setMessage(`${e.call.name} を実行中… (esc で中断)`);
        break;
      }
      case "tool_update": {
        const v = this.tools.get(e.call.id);
        if (v) v.live = (v.live + e.text).slice(-20_000);
        break;
      }
      case "tool_end": {
        const v = this.tools.get(e.call.id);
        if (v) {
          v.output = textOf(e.result.content);
          v.diff = typeof e.result.details?.diff === "string" ? e.result.details.diff : undefined;
          v.status = e.result.isError ? "error" : "done";
        }
        this.loader?.setMessage("考え中… (esc で中断)");
        break;
      }
      case "context_limit":
        this.chat.addChild(new Notice("コンテキストの上限に達したため停止しました。/clear で新しいセッションを始めてください。", c.yellow));
        break;
      case "error":
        this.chat.addChild(new Notice(`エラー: ${e.error}`, c.red));
        break;
      case "agent_end":
        this.loader?.stop();
        this.loader = undefined;
        if (e.cause === "aborted") this.chat.addChild(new Notice("中断しました", c.yellow));
        if (e.cause === "max_turns") this.chat.addChild(new Notice("最大ターン数に達しました", c.yellow));
        this.renderStatus();
        break;
    }
    this.tui.requestRender();
  }

  private describeTool(call: ToolCall): void {
    const view = this.tools.get(call.id);
    const tool = this.host.agent.getTools().find((t) => t.name === call.name);
    if (view && tool?.summary && !call.rawInput) {
      try {
        view.summary = tool.summary(call.input as never);
      } catch {}
    }
  }

  private askApproval(req: ApprovalRequest): Promise<ApprovalAnswer> {
    return new Promise((resolve) => {
      let summary = "";
      try {
        summary = req.tool.summary?.(req.args) ?? "";
      } catch {}
      const dialog = new ApprovalDialog(req, summary, (answer) => {
        this.showEditor();
        resolve(answer);
      });
      this.showInput(dialog);
    });
  }

  // ── commands ───────────────────────────────────────────────────────

  private coreCommands(): CommandDefinition[] {
    return [
      { name: "help", description: "コマンドとキー操作を表示", run: () => this.help() },
      { name: "model", description: "モデルを切り替える", argumentHint: "<provider/model>", run: ({ args }) => this.modelCommand(args) },
      { name: "resume", description: "過去のセッションを再開", run: () => this.resumeCommand() },
      { name: "clear", description: "新しいセッションを始める", run: () => this.clearCommand() },
      { name: "permission", description: "権限モードを切り替える", argumentHint: PERMISSION_MODES.join("|"), run: ({ args }) => this.permissionCommand(args) },
    ];
  }

  private async runCommand(text: string): Promise<void> {
    const [name = "", ...rest] = text.slice(1).split(/\s+/);
    const cmd = this.commands.find((c) => c.name === name);
    if (!cmd) {
      this.notify(`不明なコマンド: /${name}（/help で一覧）`, c.yellow);
      return;
    }
    try {
      await cmd.run({ args: rest.join(" ").trim() });
    } catch (e) {
      this.notify(`/${name} が失敗しました: ${(e as Error).message}`, c.red);
    }
  }

  private help(): void {
    const lines = this.commands.map((cmd) => `  /${cmd.name}${cmd.argumentHint ? ` ${c.gray(cmd.argumentHint)}` : ""}  ${c.gray(cmd.description)}`);
    this.notify(
      [
        c.bold("コマンド"),
        ...lines,
        c.bold("キー"),
        "  enter 送信 · shift/alt+enter 改行 · ↑↓ 履歴 · tab 補完",
        "  esc 中断 · ctrl+o 詳細表示 · shift+tab 権限モード · ctrl+c×2 / ctrl+d 終了",
        c.gray("  実行中に送ったメッセージは次のターンでモデルに届きます"),
      ].join("\n"),
      (s) => s,
    );
  }

  private async modelCommand(args: string): Promise<void> {
    const agent = this.host.agent;
    let spec = args;
    if (!spec) {
      const currentSpec = `${agent.model.provider}/${agent.model.id}`;
      const choices = [...new Set([currentSpec, ...(this.host.config.models ?? [])])];
      const picked = await this.pick(
        "モデルを選択（任意のモデルは /model <provider/model>）",
        choices.map((m) => ({ value: m, label: m, description: m === currentSpec ? "現在" : undefined })),
      );
      if (!picked) return;
      spec = picked;
    }
    agent.setModel(this.host.resolve(spec));
    this.notify(`モデル: ${spec}`);
    this.updateFooter();
  }

  private async resumeCommand(): Promise<void> {
    const sessions = listSessions(this.host.sessionsDir).filter((s) => s.path !== this.host.agent.session?.path);
    if (!sessions.length) {
      this.notify("このディレクトリの過去セッションはありません");
      return;
    }
    const picked = await this.pick(
      "再開するセッション",
      sessions.slice(0, 50).map((s) => ({
        value: s.path,
        label: (s.firstPrompt || "(空)").replace(/\s+/g, " ").slice(0, 60),
        description: `${s.modifiedAt.toLocaleString()} · ${s.messageCount} msgs`,
      })),
    );
    if (!picked) return;
    if (this.host.agent.isRunning) this.host.agent.abort();
    await this.host.agent.waitForIdle();
    this.host.loadSession(picked);
    this.resetView();
    this.notify("セッションを再開しました");
  }

  private async clearCommand(): Promise<void> {
    if (this.host.agent.isRunning) this.host.agent.abort();
    await this.host.agent.waitForIdle();
    this.host.newSession();
    this.resetView();
    this.contextTokens = 0;
    this.totalCost = 0;
    this.updateFooter();
    this.notify("新しいセッションを開始しました");
  }

  private async permissionCommand(args: string): Promise<void> {
    let mode = args as PermissionMode;
    if (!mode) {
      const current = this.host.agent.permissions.mode;
      const picked = await this.pick(
        "権限モード",
        PERMISSION_MODES.map((m) => ({ value: m, label: `${m}  ${PERMISSION_MODE_LABELS[m]}`, description: m === current ? "現在" : undefined })),
      );
      if (!picked) return;
      mode = picked as PermissionMode;
    }
    if (!PERMISSION_MODES.includes(mode)) throw new Error(`モードは ${PERMISSION_MODES.join(", ")} のいずれかです`);
    this.setMode(mode);
  }

  private setMode(mode: PermissionMode): void {
    const agent = this.host.agent;
    agent.permissions.mode = mode;
    agent.session?.append({ type: "permission_mode", mode });
    this.updateFooter();
    this.tui.requestRender();
  }

  // ── view helpers ───────────────────────────────────────────────────

  private pick(title: string, items: { value: string; label: string; description?: string }[]): Promise<string | undefined> {
    return new Promise((resolve) => {
      this.showInput(
        new Picker(title, items, (item) => {
          this.showEditor();
          resolve(item?.value);
        }),
      );
    });
  }

  private showInput(component: Component): void {
    this.inputSlot.clear();
    this.inputSlot.addChild(component);
    this.tui.setFocus(component);
    this.tui.requestRender();
  }

  private showEditor(): void {
    this.showInput(this.editor);
  }

  private notify(text: string, color: (s: string) => string = c.gray): void {
    this.chat.addChild(new Spacer(1));
    this.chat.addChild(new Notice(text, color));
    this.tui.requestRender();
  }

  private renderStatus(): void {
    this.status.clear();
    if (this.loader) this.status.addChild(this.loader);
    for (const q of this.queued) this.status.addChild(new Notice(`↳ 次のターンで送信: ${q.split("\n")[0]}`));
    this.tui.requestRender();
  }

  private resetView(): void {
    this.chat.clear();
    this.tools.clear();
    this.renderHistory();
    this.tui.requestRender(true);
  }

  /** Rebuild the transcript from agent.messages (after /resume or on startup with -c). */
  private renderHistory(): void {
    for (const m of this.host.agent.messages) {
      if (m.role === "user") {
        this.chat.addChild(new Spacer(1));
        this.chat.addChild(new UserView(m.content));
      } else if (m.role === "assistant") {
        const view = new AssistantView((call, v) => this.tools.set(call.id, v));
        this.chat.addChild(new Spacer(1));
        this.chat.addChild(view);
        view.update(m);
        for (const b of m.content) if (b.type === "tool_call") this.describeTool(b);
        const ctx = m.usage.input + m.usage.cacheRead + m.usage.cacheWrite + m.usage.output;
        if (ctx) this.contextTokens = ctx;
        this.totalCost += m.usage.cost;
      } else {
        const v = this.tools.get(m.toolCallId);
        if (v) {
          v.output = textOf(m.content);
          v.status = m.isError ? "error" : "done";
        }
      }
    }
  }

  private updateFooter(): void {
    const a = this.host.agent;
    const pct = a.model.contextWindow ? Math.round((this.contextTokens / a.model.contextWindow) * 100) : 0;
    const parts = [
      `${a.model.provider}/${a.model.id}`,
      `権限: ${PERMISSION_MODE_LABELS[a.permissions.mode]}`,
      `ctx ${fmtTokens(this.contextTokens)} (${pct}%)`,
    ];
    if (this.totalCost > 0) parts.push(`$${this.totalCost.toFixed(3)}`);
    this.footer.setText(c.gray(parts.join(" · ")));
  }

  private loadHistory(): string[] {
    if (!this.host.historyPath) return [];
    try {
      return readFileSync(this.host.historyPath, "utf8")
        .split("\n")
        .filter(Boolean)
        .slice(-200)
        .map((l) => JSON.parse(l));
    } catch {
      return [];
    }
  }

  private saveHistory(text: string): void {
    if (!this.host.historyPath) return;
    try {
      appendFileSync(this.host.historyPath, `${JSON.stringify(text)}\n`);
    } catch {}
  }
}

function fmtTokens(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n);
}

/** A single truncated line whose text can change. */
class Line implements Component {
  text = "";
  setText(t: string): void {
    this.text = t;
  }
  render(width: number): string[] {
    return [truncateToWidth(this.text, width)];
  }
  invalidate(): void {}
}
