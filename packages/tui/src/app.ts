import { appendFileSync, existsSync, readFileSync } from "node:fs";
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
  type PluginHost,
  type UIBridge,
} from "@sasacode/agent";
import { type ModelInfo, textOf, type ToolCall } from "@sasacode/ai";
import type { CommandDefinition, SelectOption } from "@sasacode/plugin-api";
import { ApprovalDialog, Picker } from "./dialogs.ts";
import { c, editorTheme } from "./theme.ts";
import { AssistantView, display, Notice, ToolView, UserView } from "./views.ts";

/** What the TUI needs from the CLI. */
export interface TuiHost {
  agent: Agent;
  host: PluginHost;
  warnings: string[];
  config: { models?: string[] };
  resolve(spec: string): ModelInfo;
  sessionsDir: string;
  historyPath?: string;
  listModels(refresh?: boolean): Promise<{ spec: string; contextWindow?: number; maxContext?: number }[]>;
  loadPlugins(ui: UIBridge): Promise<void>;
  loadSession(path: string): Promise<void>;
  newSession(): Promise<void>;
  fork(index: number): Promise<void>;
  shutdown(): Promise<void>;
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
  private currentSpacer?: Spacer;
  private tools = new Map<string, ToolView>();
  private queued: string[] = [];
  private totalCost = 0;
  private contextTokens = 0;
  private lastCtrlC = 0;
  private exit?: (code: number) => void;
  private ready?: Promise<void>;
  private approvals: Promise<unknown> = Promise.resolve();

  constructor(private host: TuiHost) {
    this.tui = new TuiMainScreen(new ProcessTerminal());
    this.editor = new Editor(this.tui, editorTheme, { paddingX: 1 });
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
    host.host.onChange = () => {
      this.refreshCommands();
      this.updateFooter();
      this.tui.requestRender();
    };
    host.agent.events.on((e) => this.onEvent(e));
    this.tui.addInputListener((data) => this.onKey(data));
    this.renderHistory();
    this.updateFooter();
  }

  async run(initialPrompt: string): Promise<number> {
    // The core commands are registered through the public plugin API like any other (P2).
    await this.host.host.load("core-commands", (api) => {
      for (const cmd of this.coreCommands()) api.registerCommand(cmd);
    });
    this.refreshCommands();
    this.tui.start();
    // Plugins and MCP servers load in the background so input is never blocked (NFR).
    this.ready = this.host.loadPlugins(this.bridge()).catch((e) => this.notify(`plugin loading failed: ${e.message}`, c.red));
    // Ask the providers for their models in the background: /model is instant, and the current
    // model learns its real context window.
    void this.host.listModels().then(() => this.updateFooter(), () => {});
    if (initialPrompt.trim()) this.submit(initialPrompt);
    return new Promise((resolve) => {
      this.exit = (code) => {
        void this.host.shutdown().finally(() => {
          this.tui.stop();
          const s = this.host.agent.session;
          // Without tmux the terminal is gone after this; leave the way back on screen.
          if (s && existsSync(s.path)) process.stdout.write(`\r\x1b[2K${c.gray(`再開: sasacode -r ${s.id}`)}\n`);
          resolve(code);
        });
      };
    });
  }

  private refreshCommands(): void {
    this.editor.setAutocompleteProvider(
      new CombinedAutocompleteProvider(
        this.host.host.commands.map((cmd) => ({
          name: cmd.name,
          description: cmd.description,
          argumentHint: cmd.argumentHint,
          getArgumentCompletions: cmd.complete
            ? async (prefix: string) => (await cmd.complete!(prefix)).map((o) => ({ value: o.value, label: o.label, description: o.description }))
            : undefined,
        })),
        this.host.agent.cwd,
      ),
    );
  }

  /** How plugins reach the user (api.ui). */
  private bridge(): UIBridge {
    return {
      interactive: true,
      notify: (m, level) => this.notify(m, level === "error" ? c.red : level === "warning" ? c.yellow : c.gray),
      confirm: async (title, message) =>
        (await this.pick(message ? `${title}\n${message}` : title, [
          { value: "yes", label: "はい" },
          { value: "no", label: "いいえ" },
        ])) === "yes",
      select: (title, options: SelectOption[]) => this.pick(title, options),
    };
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
    void (this.ready ?? Promise.resolve()).then(() => agent.prompt(text));
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
        this.current = new AssistantView((call, view) => this.trackTool(call, view));
        this.currentSpacer = new Spacer(1);
        this.chat.addChild(this.currentSpacer);
        this.chat.addChild(this.current);
        break;
      case "message_discarded":
        // A plugin asked for this response to be generated again.
        if (this.current) this.chat.removeChild(this.current);
        if (this.currentSpacer) this.chat.removeChild(this.currentSpacer);
        this.current = undefined;
        break;
      case "tool_repaired": {
        const v = this.tools.get(e.call.id);
        if (v) v.repairNote = e.note;
        break;
      }
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
          v.result = e.result;
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
      case "plugin_error":
        this.chat.addChild(new Notice(`プラグイン ${e.plugin} の ${e.hook} ハンドラでエラー: ${e.error}`, c.red));
        break;
      case "messages_replaced":
        this.resetView();
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

  private trackTool(call: ToolCall, view: ToolView): void {
    this.tools.set(call.id, view);
    view.renderer = this.host.host.renderers.get(call.name);
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

  /** Subagents can ask in parallel; dialogs are shown one at a time. */
  private askApproval(req: ApprovalRequest): Promise<ApprovalAnswer> {
    const next = this.approvals.then(() => this.showApproval(req));
    this.approvals = next.catch(() => {});
    return next;
  }

  private showApproval(req: ApprovalRequest): Promise<ApprovalAnswer> {
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
      {
        name: "model",
        description: "モデルを切り替える（プロバイダーから取得した一覧）",
        argumentHint: "<provider/model> | --refresh",
        run: ({ args }) => this.modelCommand(args),
        complete: async (prefix) =>
          (await this.modelChoices())
            .filter((o) => o.value.toLowerCase().includes(prefix.toLowerCase()))
            .slice(0, 50),
      },
      { name: "resume", description: "過去のセッションを再開", run: () => this.resumeCommand() },
      { name: "clear", description: "新しいセッションを始める", run: () => this.clearCommand() },
      { name: "permission", description: "権限モードを切り替える", argumentHint: PERMISSION_MODES.join("|"), run: ({ args }) => this.permissionCommand(args) },
      { name: "fork", description: "過去のメッセージから会話を分岐する", run: () => this.forkCommand() },
      { name: "session", description: "このセッションの ID と保存先", run: () => this.sessionCommand() },
      { name: "exit", description: "終了する", run: () => this.exit?.(0) },
      { name: "quit", description: "終了する（/exit と同じ）", run: () => this.exit?.(0) },
    ];
  }

  private async runCommand(text: string): Promise<void> {
    const [name = "", ...rest] = text.slice(1).split(/\s+/);
    const cmd = this.host.host.commands.find((c) => c.name === name);
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
    const lines = this.host.host.commands.map((cmd) => `  /${cmd.name}${cmd.argumentHint ? ` ${c.gray(cmd.argumentHint)}` : ""}  ${c.gray(cmd.description)}`);
    this.notify(
      [
        c.bold("コマンド"),
        ...lines,
        c.bold("キー"),
        "  enter 送信 · shift/alt+enter 改行 · ↑↓ 履歴 · tab 補完",
        "  esc 中断 · ctrl+o 詳細表示 · shift+tab 権限モード · /exit・ctrl+c×2・ctrl+d 終了",
        c.gray("  実行中に送ったメッセージは次のターンでモデルに届きます"),
      ].join("\n"),
      (s) => s,
    );
  }

  /** Current model, the ones in config, then everything the providers list. */
  private async modelChoices(refresh = false): Promise<SelectOption[]> {
    const agent = this.host.agent;
    const current = `${agent.model.provider}/${agent.model.id}`;
    const pending = this.host.listModels(refresh).catch((e: Error) => {
      this.notify(`モデル一覧を取得できませんでした: ${e.message}`, c.yellow);
      return [];
    });
    const quick = await Promise.race([pending, Bun.sleep(300).then(() => undefined)]);
    if (!quick) this.notify("プロバイダーからモデル一覧を取得しています…");
    const listed = quick ?? (await pending);
    const size = new Map(
      listed.map((m) => [m.spec, m.contextWindow ? `${fmtTokens(m.contextWindow)} ctx` : m.maxContext ? `最大 ${fmtTokens(m.maxContext)}（num_ctx 未設定）` : ""]),
    );
    const specs = [...new Set([current, ...(this.host.config.models ?? []), ...listed.map((m) => m.spec)])];
    return specs.map((s) => ({
      value: s,
      label: s,
      description: [s === current ? "現在" : "", size.get(s) ?? ""].filter(Boolean).join(" · ") || undefined,
    }));
  }

  private async modelCommand(args: string): Promise<void> {
    const agent = this.host.agent;
    let spec = args === "--refresh" ? "" : args;
    if (!spec) {
      const picked = await this.pick("モデルを選択（一覧にないモデルは /model <provider/model>）", await this.modelChoices(args === "--refresh"), 12);
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
    await this.host.loadSession(picked);
    this.resetView();
    this.notify("セッションを再開しました");
  }

  private async clearCommand(): Promise<void> {
    if (this.host.agent.isRunning) this.host.agent.abort();
    await this.host.agent.waitForIdle();
    await this.host.newSession();
    this.resetView();
    this.contextTokens = 0;
    this.totalCost = 0;
    this.updateFooter();
    this.notify("新しいセッションを開始しました");
  }

  private sessionCommand(): void {
    const s = this.host.agent.session;
    if (!s) {
      this.notify("このセッションは保存していません（--no-session）");
      return;
    }
    const saved = existsSync(s.path);
    this.notify(
      [`session ${s.id}`, saved ? s.path : "（最初のメッセージを送ると保存されます）", `再開: sasacode -r ${s.id}`].join("\n"),
      (x) => x,
    );
  }

  private async forkCommand(): Promise<void> {
    const agent = this.host.agent;
    const users = agent.messages
      .map((m, i) => ({ m, i }))
      .filter(({ m }) => m.role === "user" && !textOf(m.content).startsWith("[Summary of"));
    if (!users.length) {
      this.notify("分岐できるメッセージがありません");
      return;
    }
    const picked = await this.pick(
      "どのメッセージから分岐しますか（その直前までの会話で新しいセッションを作り、入力欄に戻します）",
      users.reverse().map(({ m, i }) => ({ value: String(i), label: textOf(m.content).replace(/\s+/g, " ").slice(0, 70) })),
    );
    if (picked === undefined) return;
    if (agent.isRunning) agent.abort();
    await agent.waitForIdle();
    const index = Number(picked);
    const text = textOf(agent.messages[index]!.content).replace(/^\[The user interrupted the previous response\.\]/, "");
    await this.host.fork(index);
    this.resetView();
    this.editor.setText(text);
    this.notify("分岐しました。編集して送信してください");
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

  private pick(title: string, items: SelectOption[], maxVisible = 10): Promise<string | undefined> {
    return new Promise((resolve) => {
      this.showInput(
        new Picker(
          title,
          items,
          (item) => {
            this.showEditor();
            resolve(item?.value);
          },
          maxVisible,
        ),
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
    this.contextTokens = 0;
    this.totalCost = 0;
    this.renderHistory();
    this.updateFooter();
    this.tui.requestRender(true);
  }

  /** Rebuild the transcript from agent.messages (after /resume or on startup with -c). */
  private renderHistory(): void {
    for (const m of this.host.agent.messages) {
      if (m.role === "user") {
        this.chat.addChild(new Spacer(1));
        this.chat.addChild(new UserView(m.content));
      } else if (m.role === "assistant") {
        const view = new AssistantView((call, v) => this.trackTool(call, v));
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
    if (a.session) parts.push(`session ${a.session.id}`);
    if (this.totalCost > 0) parts.push(`$${this.totalCost.toFixed(3)}`);
    const plugins = [...this.host.host.status.values()];
    this.footer.setText(c.gray(parts.join(" · ")) + (plugins.length ? `\n${c.cyan(plugins.join(" · "))}` : ""));
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

/** 131072 → "128k" (context sizes are often powers of two), 1_050_000 → "1.05M", 1534 → "1.5k". */
function fmtTokens(n: number): string {
  if (n >= 1_000_000) return `${+(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1024 && n % 1024 === 0) return `${n / 1024}k`;
  return n >= 1000 ? `${+(n / 1000).toFixed(1)}k` : String(n);
}

/** A single truncated line whose text can change. */
class Line implements Component {
  text = "";
  setText(t: string): void {
    this.text = t;
  }
  render(width: number): string[] {
    return this.text.split("\n").map((l) => truncateToWidth(l, width));
  }
  invalidate(): void {}
}
