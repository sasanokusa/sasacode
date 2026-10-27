import { appendFileSync, existsSync, readFileSync } from "node:fs";
import {
  CombinedAutocompleteProvider,
  type Component,
  Container,
  Editor,
  getKeybindings,
  matchesKey,
  ProcessTerminal,
  ScrollView,
  setAltScreenSearchText,
  Spacer,
  type Terminal,
  type TUI,
  truncateToWidth,
  TuiAltScreen,
  TuiMainScreen,
  VStack,
} from "@earendil-works/pi-tui";
import { type Agent, type AgentEvent, type ApprovalAnswer, type ApprovalRequest, PERMISSION_MODE_LABELS, PERMISSION_MODES, type PermissionMode, type PluginHost, type UIBridge } from "@sasacode/agent";
import { type Lang, listSessions, retranslate, setLang, t } from "@sasacode/host";
import { fmtTokens, type ModelInfo, textOf, type ThinkingLevel, type ToolCall } from "@sasacode/ai";
import type { CommandDefinition, SelectOption, ShowTextOptions, UserContent } from "@sasacode/plugin-api";
import { matchAmbiguousWidth } from "./ambiguous.ts";
import { Activity } from "./activity.ts";
import { ApprovalDialog, CheckList, InputDialog, Picker } from "./dialogs.ts";
import { TextViewer } from "./text-viewer.ts";
import { copyToClipboard } from "./clipboard.ts";
import { exitSummary, UsageTally } from "./summary.ts";
import { c, editorTheme } from "./theme.ts";
import { AssistantView, display, Notice, Padded, plain, ToolView, UserView } from "./views.ts";

const EFFORTS: ThinkingLevel[] = ["off", "low", "medium", "high", "xhigh", "max"];
const EFFORT_LABELS: Record<ThinkingLevel, string> = {
  off: "推論しない",
  low: "浅く・速く",
  medium: "標準",
  high: "深く（既定）",
  xhigh: "さらに深く",
  max: "最大（対応していないモデルでは xhigh 相当）",
};

/** Columns left free at both sides of everything on screen. */
const MARGIN = 2;

/** Runs at least this long end with the terminal bell (tui.bell). */
const BELL_AFTER_MS = 30_000;

/** What the TUI needs from the CLI. */
/** How the agent marks a message sent after an interrupt; not part of what the user wrote. */
const INTERRUPTED = "[The user interrupted the previous response.]";
/** An image in the input, and how a sent message names one (the image-attach plugin's forms). */
const IMAGE_MARKER = /\[Image #(\d+)\]/g;
const IMAGE_LABEL = /\[Image (\d+): ([^\]\n]*)\]/g;

/** The conversation search box's words, in the interface's language. */
const searchText = () => ({ placeholder: t("会話内を検索"), noMatches: t("一致するものがありません") });

/** At most `n` characters, cut between whole characters (an emoji is not split into a broken one). */
function clip(s: string, n: number): string {
  const all = [...new Intl.Segmenter().segment(s)];
  return all.length > n ? `${all.slice(0, n - 1).map((g) => g.segment).join("")}…` : s;
}

/** A paste up to this size is shown in the input as it is; a longer one is folded into a marker. */
const SHOWN_PASTE_LINES = 40;
const SHOWN_PASTE_CHARS = 4000;

export interface TuiHost {
  agent: Agent;
  host: PluginHost;
  warnings: string[];
  config: { models?: string[]; tui?: { altScreen?: boolean; bell?: boolean } };
  resolve(spec: string): ModelInfo;
  sessionsDir: string;
  historyPath?: string;
  listModels(refresh?: boolean): Promise<{ spec: string; contextWindow?: number; maxContext?: number }[]>;
  loadPlugins(ui: UIBridge): Promise<void>;
  loadSession(path: string): Promise<void>;
  newSession(): Promise<void>;
  fork(index: number): Promise<void>;
  shutdown(): Promise<void>;
  /** Keep the interface language for the next start (the `lang` setting in config.json). */
  saveLang?(lang: Lang): void;
}

const LANGUAGES: SelectOption[] = [
  { value: "ja", label: "日本語" },
  { value: "en", label: "English" },
];
const LANGUAGE_NAMES: Record<string, Lang> = { ja: "ja", jp: "ja", japanese: "ja", 日本語: "ja", en: "en", english: "en", 英語: "en" };

/** `terminal`: the real one, or a stand-in (tests drive the app through it). */
export async function runTui(host: TuiHost, initialPrompt = "", terminal: Terminal = new ProcessTerminal()): Promise<number> {
  const app = new App(host, terminal);
  return app.run(initialPrompt);
}

class App {
  private tui: TUI;
  private chat = new Container();
  private status = new Container();
  private inputSlot = new Container();
  private footer = new Line();
  private editor: Editor;
  private activity?: Activity;
  private header!: Notice;
  /** Tool calls of the last reply that have not started yet (their permission checks are running). */
  private checking: ToolCall[] = [];
  private current?: AssistantView;
  private currentSpacer?: Spacer;
  private tools = new Map<string, ToolView>();
  /** Sent while the agent works, until it reaches the model: `content` is what the plugins made of it. */
  private queued: { text: string; content?: UserContent[] }[] = [];
  /** Images of a message put back into the input (fork, interrupt), behind [Image #n] markers there. */
  private held = new Map<number, { name: string; image: UserContent }>();
  private totalCost = 0;
  private contextTokens = 0;
  private usage = new UsageTally();
  private runStarted = 0;
  private lastCtrlC = 0;
  /** A bracketed paste still arriving (onKey). */
  private pasted?: string;
  private foldNoticeShown = false;
  private warnedContext = new Set<string>();
  private exit?: (code: number) => void;
  private ready?: Promise<void>;
  private approvals: Promise<unknown> = Promise.resolve();

  constructor(
    private host: TuiHost,
    private terminal: Terminal,
  ) {
    // Full screen by default: the transcript scrolls above a fixed input line, and on exit the
    // terminal comes back as it was, with a short summary instead of the whole conversation.
    const fullscreen = host.config.tui?.altScreen !== false;
    this.tui = fullscreen
      ? new TuiAltScreen(terminal, false, undefined, {
          mouse: true,
          wheelScrollLines: 3,
          // Drag-selected text is copied on release; OSC 52 alone does nothing in many terminals.
          copySelection: async (text) => (await copyToClipboard(text), true),
          scrollToEndIndicator: () => c.inverse(` ${t("↓ 最新へ (ctrl+end)")} `),
        })
      : new TuiMainScreen(terminal);
    // home/end stay with the input line; the transcript jumps with ctrl+home/end.
    if (fullscreen) getKeybindings().setUserBindings({ "tui.altScreen.top": "ctrl+home", "tui.altScreen.bottom": "ctrl+end" });
    setAltScreenSearchText(searchText());
    this.editor = new Editor(this.tui, editorTheme, { paddingX: 1 });
    this.editor.onSubmit = (text) => this.submit(text);
    for (const h of this.loadHistory()) this.editor.addToHistory(h);

    const header = (this.header = new Notice(this.headerText()));
    for (const w of host.warnings) this.chat.addChild(new Notice(t("警告: {warning}", { warning: w }), c.yellow));
    const body = new Container();
    body.addChild(header);
    body.addChild(this.chat);
    // One margin for everything, as a terminal block would give it: the transcript, the input
    // (its rules end short of the edges) and the footer share the same left and right edge, with a
    // blank line above the transcript and below the footer.
    const transcript = new Padded(body, MARGIN, MARGIN, fullscreen ? 1 : 0);
    const status = new Padded(this.status, MARGIN, MARGIN);
    const input = new Padded(this.inputSlot, MARGIN, MARGIN);
    const footer = new Padded(this.footer, MARGIN, MARGIN, 0, fullscreen ? 1 : 0);
    if (this.tui instanceof TuiAltScreen) {
      const dock = new VStack([
        { component: status, shrink: 1, minSize: 0 },
        { component: input, shrink: 1, minSize: 3 },
        { component: footer, shrink: 1, minSize: 0 },
      ]);
      for (const part of [transcript, status, input, footer]) this.tui.addChild(part);
      this.tui.setLayoutRoot(
        new VStack([
          { component: new ScrollView(transcript, { follow: "end", primary: true, overscroll: "chain", scrollbar: "auto" }), basis: 0, grow: 1, shrink: 1, minSize: 1 },
          { component: dock, basis: "auto", grow: 0, shrink: 1, minSize: 1 },
        ]),
      );
    } else for (const part of [transcript, status, input, footer]) this.tui.addChild(part);
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
    await matchAmbiguousWidth();
    this.tui.start();
    // Plugins and MCP servers load in the background so input is never blocked (NFR).
    this.ready = this.host.loadPlugins(this.bridge()).catch((e) => this.notify(t("プラグインの読み込みに失敗: {error}", { error: e.message }), c.red));
    // Ask the providers for their models in the background: /model is instant, and the current
    // model learns its real context window.
    void this.host.listModels().then(
      () => {
        this.updateFooter();
        void this.warnContext();
      },
      () => {},
    );
    if (initialPrompt.trim()) this.submit(initialPrompt);
    return new Promise((resolve) => {
      this.exit = (code) => {
        void this.host.shutdown().finally(() => {
          // Full screen: leave without copying the transcript onto the normal screen (that is the point).
          this.tui.stop({ preserveScreen: this.tui instanceof TuiAltScreen });
          const s = this.host.agent.session;
          // What was used, and the way back (the session is gone from the screen now).
          process.stdout.write(`\r\x1b[2K${exitSummary(this.usage, s && existsSync(s.path) ? s.id : undefined)}`);
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
          { value: "yes", label: t("はい") },
          { value: "no", label: t("いいえ") },
        ])) === "yes",
      select: (title, options: SelectOption[]) => this.pick(title, options),
      input: (title, options) => this.ask((done) => new InputDialog(title, options, done)),
      selectMany: (title, options, opts) => this.ask((done) => new CheckList(title, options, opts.selected ?? [], done)),
      showText: (options) => this.showText(options),
    };
  }

  // ── input ──────────────────────────────────────────────────────────

  private onKey(data: string): { consume: boolean } | undefined {
    const agent = this.host.agent;
    // The editor may be shown while something else has the keys: the conversation search box, say.
    const editing = this.inputSlot.children[0] === this.editor && this.editor.focused;
    if (editing) {
      const start = data.indexOf("\x1b[200~");
      if (this.pasted !== undefined || start >= 0) {
        // A bracketed paste may come in several chunks: gather it, then let the plugins look first.
        if (this.pasted === undefined) {
          if (start > 0) this.editor.handleInput(data.slice(0, start));
          data = data.slice(start + 6);
        }
        this.pasted = (this.pasted ?? "") + data;
        const end = this.pasted.indexOf("\x1b[201~");
        if (end < 0) return { consume: true };
        const text = this.pasted.slice(0, end);
        const rest = this.pasted.slice(end + 6);
        this.pasted = undefined;
        // Inserted after the key handler returned: render then, or it shows only on the next key.
        void this.paste(text).then(() => {
          if (rest) this.editor.handleInput(rest);
          this.tui.requestRender();
        });
        return { consume: true };
      }
      if (this.deleteMarker(data)) return { consume: true };
      // An image on the clipboard pastes nothing in most terminals; ctrl+v asks for it.
      if (matchesKey(data, "ctrl+v") && this.host.host.pasteHandlers.length) {
        void this.paste("").then(() => this.tui.requestRender());
        return { consume: true };
      }
    }
    // The pager owns its navigation keys, including Esc; it must not stop an active agent.
    if (this.inputSlot.children[0] instanceof TextViewer && !matchesKey(data, "ctrl+c")) return undefined;
    if (
      matchesKey(data, "escape") &&
      agent.isRunning &&
      editing &&
      !this.editor.isShowingAutocomplete()
    ) {
      this.interrupt();
      return { consume: true };
    }
    if (matchesKey(data, "ctrl+c")) {
      if (agent.isRunning) this.interrupt();
      else if (this.editor.getText()) this.editor.setText("");
      else if (Date.now() - this.lastCtrlC < 1500) this.exit?.(0);
      else {
        this.lastCtrlC = Date.now();
        this.notify(t("もう一度 ctrl+c で終了します"));
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
    if (matchesKey(data, "shift+tab") && editing) {
      const modes = PERMISSION_MODES;
      this.setMode(modes[(modes.indexOf(agent.permissions.mode) + 1) % modes.length]!);
      return { consume: true };
    }
    return undefined;
  }

  /**
   * Backspace or delete next to (or inside) an "[Image #1]" marker removes all of it, as the editor
   * does for its own "[paste #1 +12 lines]": a marker with a character missing is no longer one.
   */
  private deleteMarker(data: string): boolean {
    const back = matchesKey(data, "backspace");
    if (!back && !matchesKey(data, "delete")) return false;
    const { line, col } = this.editor.getCursor();
    for (const m of (this.editor.getLines()[line] ?? "").matchAll(/\[Image #\d+\]/g)) {
      const start = m.index!;
      const end = start + m[0].length;
      if (back ? col <= start || col > end : col < start || col >= end) continue;
      // Through the editor's own keys, so undo and the cursor stay right.
      const keys = back ? "\x1b[C".repeat(end - col) + "\x7f".repeat(m[0].length) : "\x1b[D".repeat(col - start) + "\x1b[3~".repeat(m[0].length);
      for (const k of keys.match(/\x1b\[C|\x1b\[D|\x1b\[3~|\x7f/g)!) this.editor.handleInput(k);
      return true;
    }
    return false;
  }

  /** Pasted text (or "" for ctrl+v) offered to the plugins; unclaimed text goes to the editor as a paste. */
  private async paste(text: string): Promise<void> {
    for (const handle of this.host.host.pasteHandlers) {
      const out = await Promise.resolve(handle(text, this.editor.getText())).catch(() => undefined);
      if (out !== undefined) {
        this.editor.insertTextAtCursor(out);
        return;
      }
    }
    if (!text) return;
    // The editor folds a paste over 10 lines into "[paste #1 +12 lines]" (sent in full), which reads
    // like something else was pasted. A paste that fits on screen is shown as it is.
    const clean = text.replace(/\r\n?/g, "\n").replace(/\t/g, "    ").replace(/[\x00-\x09\x0b-\x1f\x7f]/g, "");
    if (clean.split("\n").length <= SHOWN_PASTE_LINES && clean.length <= SHOWN_PASTE_CHARS) {
      this.editor.insertTextAtCursor(clean);
      return;
    }
    this.editor.handleInput(`\x1b[200~${text}\x1b[201~`);
    if (!this.foldNoticeShown) {
      this.foldNoticeShown = true;
      this.notify(t("長い貼り付けは [paste #番号 +行数] とまとめて表示します。送信すると全文が送られます"));
    }
  }

  /** The editor hands over text with paste markers already expanded, and clears itself. */
  private submit(text: string): void {
    if (!text.trim()) return;
    this.editor.addToHistory(text);
    this.saveHistory(text);
    // Whatever comes back appears at the end: follow it again if the user had scrolled up.
    if (this.tui instanceof TuiAltScreen) this.tui.scrollToBottom();
    if (text.startsWith("/")) {
      void this.runCommand(text);
      return;
    }
    const agent = this.host.agent;
    const entry: (typeof this.queued)[number] = { text };
    if (agent.isRunning) {
      this.queued.push(entry);
      this.renderStatus();
    }
    const input = this.withHeld(text);
    void (this.ready ?? Promise.resolve()).then(() =>
      agent.prompt(input, (content) => {
        if (content) entry.content = content;
        else if (this.queued.includes(entry)) {
          // A plugin took it as a command: it will not reach the model.
          this.queued.splice(this.queued.indexOf(entry), 1);
          this.renderStatus();
        }
      }),
    );
  }

  // ── agent events ───────────────────────────────────────────────────

  private onEvent(e: AgentEvent): void {
    switch (e.type) {
      case "agent_start":
        this.runStarted = Date.now();
        this.activity = new Activity(this.tui, c.cyan, c.gray);
        this.renderStatus();
        break;
      case "message_start":
        this.checking = [];
        this.activity?.newReply();
        this.current = new AssistantView((call, view) => this.trackTool(call, view), this.host.agent.cwd);
        this.currentSpacer = new Spacer(1);
        this.chat.addChild(this.currentSpacer);
        this.chat.addChild(this.current);
        break;
      case "usage":
        // Subagents and side requests: in the cost and the summary, not in the context size.
        this.totalCost += e.usage.cost;
        this.usage.addUsage(e.model, e.usage, true);
        this.updateFooter();
        break;
      case "message_discarded":
        // A plugin asked for this response to be generated again.
        if (this.current) this.chat.removeChild(this.current);
        if (this.currentSpacer) this.chat.removeChild(this.currentSpacer);
        this.activity?.discardReply();
        this.current = undefined;
        if (e.message.stopReason === "error") this.notify(t("接続が応答の途中で切れたため、やり直します"), c.yellow);
        break;
      case "tool_repaired": {
        const v = this.tools.get(e.call.id);
        if (v) v.repairNote = e.note;
        break;
      }
      case "message_update":
        this.current?.update(e.message);
        if (e.event.type === "toolcall_delta") this.current?.receiving(e.event.index, e.event.delta.length);
        if (e.event.type === "text_delta" || e.event.type === "thinking_delta" || e.event.type === "toolcall_delta") this.activity?.streamed(e.event.delta);
        break;
      case "message_end": {
        const m = e.message;
        if (m.role === "user") {
          // Delivered: the message holds the very blocks the plugins made of it (text matching would miss
          // one whose text a plugin changed, an image path for one).
          const qi = this.queued.findIndex((q) => q.content?.length && q.content.every((b) => m.content.includes(b)));
          if (qi >= 0) this.queued.splice(qi, 1);
          this.chat.addChild(new Spacer(1));
          this.chat.addChild(new UserView(m.content));
          this.renderStatus();
        } else if (m.role === "assistant") {
          this.current?.update(m);
          for (const b of m.content) if (b.type === "tool_call") this.describeTool(b);
          // Until each call starts, permission checks run unseen: an agent-mode judge or jev-guard asks a model.
          this.checking = m.stopReason === "tool_use" ? m.content.flatMap((b) => (b.type === "tool_call" ? [b] : [])) : [];
          this.showChecking();
          this.activity?.replyDone(m.usage.output);
          this.totalCost += m.usage.cost;
          this.usage.add(m);
          const ctx = m.usage.input + m.usage.cacheRead + m.usage.cacheWrite + m.usage.output;
          if (ctx) this.contextTokens = ctx;
          if (m.stopReason === "refusal") this.chat.addChild(new Notice(t("モデルが応答を拒否しました"), c.yellow));
          if (m.stopReason === "max_tokens") this.chat.addChild(new Notice(t("出力が max_tokens に達しました"), c.yellow));
          this.current = undefined;
          this.updateFooter();
        } else {
          const view = this.tools.get(m.toolCallId);
          if (view && view.status !== "done" && view.status !== "error") {
            view.output = textOf(m.content);
            view.status = m.isError ? "error" : "done";
          }
          // A call refused or interrupted before it started has no tool_end.
          const waiting = this.checking.length;
          this.checking = this.checking.filter((b) => b.id !== m.toolCallId);
          if (this.checking.length !== waiting) this.showChecking();
        }
        break;
      }
      case "tool_start": {
        let v = this.tools.get(e.call.id);
        if (!v && e.parentCallId) {
          v = new ToolView(e.call);
          this.tools.set(e.call.id, v);
          this.chat.addChild(v);
        }
        if (v) {
          v.status = "running";
          if (e.summary) v.summary = e.summary;
        }
        this.checking = this.checking.filter((b) => b.id !== e.call.id);
        this.activity?.setDetail(t("{tool} を実行中…", { tool: e.call.name }));
        break;
      }
      case "tool_update": {
        const v = this.tools.get(e.call.id);
        if (v) v.live = (v.live + e.text).slice(-20_000);
        break;
      }
      case "tool_end": {
        let v = this.tools.get(e.call.id);
        if (!v && e.parentCallId) {
          v = new ToolView(e.call);
          this.tools.set(e.call.id, v);
          this.chat.addChild(v);
        }
        if (v) {
          v.output = textOf(e.result.content);
          v.diff = typeof e.result.details?.diff === "string" ? e.result.details.diff : undefined;
          v.result = e.result;
          v.status = e.result.isError ? "error" : "done";
        }
        this.showChecking();
        break;
      }
      case "error":
        this.chat.addChild(new Notice(t("エラー: {error}", { error: plain(e.error) }), c.red));
        break;
      case "plugin_error":
        this.chat.addChild(new Notice(t("プラグイン {plugin} の {hook} ハンドラでエラー: {error}", { plugin: e.plugin, hook: e.hook, error: e.error }), c.red));
        break;
      case "messages_replaced":
        this.resetView();
        break;
      case "agent_end":
        this.activity?.stop();
        this.activity = undefined;
        // A run long enough to switch windows ends with the terminal bell (a badge or sound, per terminal).
        if (this.host.config.tui?.bell !== false && e.cause !== "aborted" && Date.now() - this.runStarted >= BELL_AFTER_MS) process.stdout.write("\x07");
        if (e.cause === "aborted") this.chat.addChild(new Notice(t("中断しました"), c.yellow));
        if (e.cause === "max_turns") void this.offerToContinue();
        // Only when compaction (the context_limit hook) could not make room: the event itself comes before that.
        if (e.cause === "context_limit")
          this.chat.addChild(new Notice(t("コンテキストの上限に達したため停止しました。/compact で要約するか、/clear で新しいセッションを始めてください。"), c.yellow));
        if (e.cause === "stopped" && e.stopped) this.chat.addChild(new Notice(t("{plugin} が停止しました: {reason}", { plugin: e.stopped.plugin, reason: e.stopped.reason }), c.yellow));
        this.renderStatus();
        break;
    }
    this.tui.requestRender();
  }

  /** At the turn limit, the user decides whether the run goes on for another round of turns. */
  private async offerToContinue(): Promise<void> {
    const agent = this.host.agent;
    await agent.waitForIdle();
    const more = await this.pick(t("最大ターン数（{n}）に達しました。続けますか？", { n: agent.maxTurns ?? 0 }), [
      { value: "yes", label: t("続ける（さらに {n} ターンまで）", { n: agent.maxTurns ?? 0 }) },
      { value: "no", label: t("ここで止める") },
    ]);
    if (more === "yes") void agent.continue();
  }

  /** What the run waits for: the next call's permission check, or the model. */
  private showChecking(): void {
    const next = this.checking[0];
    this.activity?.setDetail(next ? t("{tool} の実行前の確認中…", { tool: next.name }) : undefined);
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
    if (req.signal?.aborted) return Promise.resolve({ decision: "deny" });
    return new Promise((resolve) => {
      let summary = "";
      try { summary = req.tool.summary?.(req.args) ?? ""; } catch {}
      let settled = false;
      const done = (answer: ApprovalAnswer) => {
        if (settled) return;
        settled = true;
        req.signal?.removeEventListener("abort", onAbort);
        this.showEditor();
        resolve(answer);
      };
      const onAbort = () => done({ decision: "deny" });
      // Esc stops the run, as anywhere else while the agent works; the call itself is refused first.
      const stop = () => {
        done({ decision: "deny" });
        this.interrupt();
      };
      const dialog = new ApprovalDialog(req, summary, done, stop, () => this.inputRows());
      this.showInput(dialog);
      req.signal?.addEventListener("abort", onAbort, { once: true });
      if (req.signal?.aborted) onAbort();
    });
  }

  // ── commands ───────────────────────────────────────────────────────

  private coreCommands(): CommandDefinition[] {
    return [
      { name: "help", description: t("コマンドとキー操作を表示"), run: () => this.help() },
      {
        name: "model",
        description: t("モデルを切り替える（プロバイダーから取得した一覧）"),
        argumentHint: "<provider/model> | --refresh",
        run: ({ args }) => this.modelCommand(args),
        complete: async (prefix) =>
          // An exact match is left out, as /language does: enter then runs the command instead of completing it.
          (await this.modelChoices())
            .filter((o) => o.value.toLowerCase().includes(prefix.toLowerCase()) && o.value.toLowerCase() !== prefix.toLowerCase())
            .slice(0, 50),
      },
      {
        name: "effort",
        description: t("推論の深さ（thinking）を切り替える"),
        argumentHint: EFFORTS.join("|"),
        run: ({ args }) => this.effortCommand(args),
        complete: async (prefix) => EFFORTS.filter((e) => e.startsWith(prefix) && e !== prefix).map((e) => ({ value: e, label: e, description: t(EFFORT_LABELS[e]) })),
      },
      { name: "copy", description: t("直前の応答をクリップボードにコピー"), run: () => this.copyCommand() },
      { name: "resume", description: t("過去のセッションを再開"), run: () => this.resumeCommand() },
      { name: "clear", description: t("新しいセッションを始める"), run: () => this.clearCommand() },
      { name: "permission", description: t("権限モードを切り替える"), argumentHint: PERMISSION_MODES.join("|"), run: ({ args }) => this.permissionCommand(args) },
      { name: "fork", description: t("過去のメッセージから会話を分岐する"), run: () => this.forkCommand() },
      { name: "session", description: t("このセッションの ID と保存先"), run: () => this.sessionCommand() },
      {
        name: "language",
        description: t("表示言語を切り替える（日本語 / English）"),
        argumentHint: "ja|en",
        run: ({ args }) => this.languageCommand(args),
        // Nothing left to complete once it is typed out: enter then runs the command, not the completion.
        complete: (prefix) => LANGUAGES.filter((o) => o.value.startsWith(prefix) && o.value !== prefix),
      },
      { name: "exit", description: t("終了する"), run: () => this.exit?.(0) },
      { name: "quit", description: t("終了する（/exit と同じ）"), run: () => this.exit?.(0) },
    ];
  }

  private async runCommand(text: string): Promise<void> {
    const [name = "", ...rest] = text.slice(1).split(/\s+/);
    const cmd = this.host.host.commands.find((c) => c.name === name);
    if (!cmd) {
      this.notify(t("不明なコマンド: /{name}（/help で一覧）", { name }), c.yellow);
      return;
    }
    try {
      await cmd.run({ args: rest.join(" ").trim() });
    } catch (e) {
      this.notify(t("/{name} が失敗しました: {error}", { name, error: (e as Error).message }), c.red);
    }
  }

  private help(): void {
    const lines = this.host.host.commands.map((cmd) => `  /${cmd.name}${cmd.argumentHint ? ` ${c.gray(cmd.argumentHint)}` : ""}  ${c.gray(cmd.description)}`);
    this.notify(
      [
        c.bold(t("コマンド")),
        ...lines,
        c.bold(t("キー")),
        `  ${t("enter 送信 · shift/alt+enter 改行 · ↑↓ 履歴 · tab 補完")}`,
        `  ${t("esc 中断 · ctrl+o 詳細表示 · shift+tab 権限モード · /exit・ctrl+c×2・ctrl+d 終了")}`,
        ...(this.tui instanceof TuiAltScreen
          ? [
              `  ${t("pageup/pagedown・ホイール スクロール · ctrl+↑↓ 前後の入力へ · ctrl+home/end 先頭・最新へ")}`,
              `  ${t("ctrl+shift+f 会話内を検索 · ドラッグで選択してコピー（端末の選択は option/alt を押しながら）")}`,
            ]
          : []),
        c.gray(`  ${t("実行中に送ったメッセージは次のターンでモデルに届きます")}`),
      ].join("\n"),
      (s) => s,
    );
  }

  private headerText(): string {
    return `${c.bold("sasacode")} ${c.gray(this.host.agent.cwd)}\n${c.gray(t("/help でコマンド一覧 · esc で中断 · ctrl+o で詳細表示 · shift+tab で権限モード切替"))}`;
  }

  /** Switches everything that reads the language now; text already in the transcript stays as it was. */
  private async languageCommand(args: string): Promise<void> {
    const named = args.trim().toLowerCase();
    const lang = named ? LANGUAGE_NAMES[named] : ((await this.pick(t("表示言語"), LANGUAGES)) as Lang | undefined);
    if (!lang) {
      if (named) this.notify(t("使い方: /language ja|en"), c.yellow);
      return;
    }
    setLang(lang);
    setAltScreenSearchText(searchText());
    this.host.host.opts.lang = lang;
    for (const cmd of this.host.host.commands) cmd.description = retranslate(cmd.description);
    this.refreshCommands();
    this.header.setText(this.headerText());
    this.updateFooter();
    let saved = true;
    try {
      this.host.saveLang?.(lang);
    } catch (e) {
      saved = false;
      this.notify(t("設定に保存できませんでした: {error}", { error: (e as Error).message }), c.yellow);
    }
    this.notify(t("表示言語: {name}", { name: LANGUAGES.find((o) => o.value === lang)!.label }));
    // SASACODE_LANG wins over the setting at the next start.
    const env = (process.env.SASACODE_LANG ?? "").toLowerCase();
    if (saved && (env === "ja" || env === "en") && env !== lang) this.notify(t("次回の起動では環境変数 SASACODE_LANG={env} が優先されます", { env }), c.yellow);
  }

  /** Current model, the ones in config, then everything the providers list. */
  private async modelChoices(refresh = false): Promise<SelectOption[]> {
    const agent = this.host.agent;
    const current = `${agent.model.provider}/${agent.model.id}`;
    const pending = this.host.listModels(refresh).catch((e: Error) => {
      this.notify(t("モデル一覧を取得できませんでした: {error}", { error: e.message }), c.yellow);
      return [];
    });
    const quick = await Promise.race([pending, Bun.sleep(300).then(() => undefined)]);
    if (!quick) this.notify(t("プロバイダーからモデル一覧を取得しています…"));
    const listed = quick ?? (await pending);
    const size = new Map(
      listed.map((m) => [m.spec, m.contextWindow ? `${fmtTokens(m.contextWindow)} ctx` : m.maxContext ? t("最大 {size}（num_ctx 未設定）", { size: fmtTokens(m.maxContext) }) : ""]),
    );
    const specs = [...new Set([current, ...(this.host.config.models ?? []), ...listed.map((m) => m.spec)])];
    return specs.map((s) => ({
      value: s,
      label: s,
      description: [s === current ? t("現在") : "", size.get(s) ?? ""].filter(Boolean).join(" · ") || undefined,
    }));
  }

  private async modelCommand(args: string): Promise<void> {
    const agent = this.host.agent;
    let spec = args === "--refresh" ? "" : args;
    if (!spec) {
      const picked = await this.pick(t("モデルを選択（一覧にないモデルは /model <provider/model>）"), await this.modelChoices(args === "--refresh"), 12);
      if (!picked) return;
      spec = picked;
    }
    agent.setModel(this.host.resolve(spec));
    this.notify(t("モデル: {spec}", { spec }));
    this.updateFooter();
    await this.warnContext();
  }

  /**
   * Ollama cuts an input longer than its own default short without a word when the model has no
   * num_ctx, while sasacode plans for the model's full size: say so, once for each model.
   */
  private async warnContext(): Promise<void> {
    const { provider, id } = this.host.agent.model;
    const spec = `${provider}/${id}`;
    if (this.warnedContext.has(spec)) return;
    const listed = (await this.host.listModels().catch(() => [])).find((m) => m.spec === spec);
    if (!listed || listed.contextWindow || !listed.maxContext) return;
    this.warnedContext.add(spec);
    this.notify(t("{spec} には num_ctx が設定されていません。Ollama を OLLAMA_CONTEXT_LENGTH で起動していなければ、Ollama の既定の長さ（数千トークン）を超えた入力は黙って切り詰められます。Modelfile に PARAMETER num_ctx 32768 などを書くか、OLLAMA_CONTEXT_LENGTH を設定してください（このモデルは最大 {max}）。", { spec, max: fmtTokens(listed.maxContext) }), c.yellow);
  }

  private async resumeCommand(): Promise<void> {
    const sessions = listSessions(this.host.sessionsDir, this.host.agent.cwd).filter((s) => s.path !== this.host.agent.session?.path);
    if (!sessions.length) {
      this.notify(t("このディレクトリの過去セッションはありません"));
      return;
    }
    const picked = await this.pick(
      t("再開するセッション"),
      // All of them: the filter searches the whole list, and an old session must stay reachable.
      sessions.map((s) => ({
        value: s.path,
        label: clip((s.firstPrompt || t("(空)")).replace(/\s+/g, " "), 60),
        description: `${s.modifiedAt.toLocaleString()} · ${s.messageCount} msgs`,
      })),
    );
    if (!picked) return;
    await this.host.agent.halt();
    await this.host.loadSession(picked);
    this.resetView();
    this.notify(t("セッションを再開しました"));
  }

  private async clearCommand(): Promise<void> {
    await this.host.agent.halt();
    await this.host.newSession();
    this.resetView();
    this.contextTokens = 0;
    this.totalCost = 0;
    this.updateFooter();
    this.notify(t("新しいセッションを開始しました"));
  }

  private sessionCommand(): void {
    const s = this.host.agent.session;
    if (!s) {
      this.notify(t("このセッションは保存していません（--no-session）"));
      return;
    }
    const saved = existsSync(s.path);
    this.notify(
      [`session ${s.id}`, saved ? s.path : t("（最初のメッセージを送ると保存されます）"), t("再開: sasacode -r {id}", { id: s.id })].join("\n"),
      (x) => x,
    );
  }

  private async forkCommand(): Promise<void> {
    const agent = this.host.agent;
    const users = agent.messages
      .map((m, i) => ({ m, i }))
      .filter(({ m }) => m.role === "user" && !textOf(m.content).startsWith("[Summary of"));
    if (!users.length) {
      this.notify(t("分岐できるメッセージがありません"));
      return;
    }
    const picked = await this.pick(
      t("どのメッセージから分岐しますか（その直前までの会話で新しいセッションを作り、入力欄に戻します）"),
      users.reverse().map(({ m, i }) => ({ value: String(i), label: clip(textOf(m.content).replace(/\s+/g, " "), 70) })),
    );
    if (picked === undefined) return;
    await agent.halt();
    const index = Number(picked);
    const content = agent.messages[index]!.content as UserContent[];
    await this.host.fork(index);
    this.resetView();
    this.editor.setText(this.toDraft(content));
    this.notify(t("分岐しました。編集して送信してください"));
  }

  private async permissionCommand(args: string): Promise<void> {
    let mode = args as PermissionMode;
    if (!mode) {
      const current = this.host.agent.permissions.mode;
      const picked = await this.pick(
        t("権限モード"),
        PERMISSION_MODES.map((m) => ({ value: m, label: `${m}  ${t(PERMISSION_MODE_LABELS[m])}`, description: m === current ? t("現在") : undefined })),
      );
      if (!picked) return;
      mode = picked as PermissionMode;
    }
    if (!PERMISSION_MODES.includes(mode)) throw new Error(t("モードは {modes} のいずれかです", { modes: PERMISSION_MODES.join(", ") }));
    this.setMode(mode);
  }

  /** The last response's text, to the system clipboard (pbcopy / wl-copy / xclip, else OSC 52). */
  private async copyCommand(): Promise<void> {
    const last = [...this.host.agent.messages].reverse().find((m) => m.role === "assistant" && textOf(m.content).trim());
    const text = last ? textOf(last.content).trim() : "";
    if (!text) return this.notify(t("コピーする応答がありません"), c.yellow);
    const how = await copyToClipboard(text);
    this.notify(how === "terminal" ? t("直前の応答をコピーしました（端末経由・{n} 文字）", { n: text.length }) : t("直前の応答をコピーしました（{n} 文字）", { n: text.length }));
  }

  private async effortCommand(args: string): Promise<void> {
    const agent = this.host.agent;
    let level = args.trim() as ThinkingLevel;
    if (!level) {
      const picked = await this.pick(
        agent.model.reasoning ? t("推論の深さ") : t("推論の深さ（{model} は推論に対応していないため、変えても効果はありません）", { model: agent.model.id }),
        EFFORTS.map((e) => ({ value: e, label: `${e}  ${t(EFFORT_LABELS[e])}`, description: e === agent.thinking ? t("現在") : undefined })),
      );
      if (!picked) return;
      level = picked as ThinkingLevel;
    }
    if (!EFFORTS.includes(level)) throw new Error(t("推論の深さは {levels} のいずれかです", { levels: EFFORTS.join(", ") }));
    agent.thinking = level;
    this.updateFooter();
    this.tui.requestRender();
  }

  private setMode(mode: PermissionMode): void {
    const agent = this.host.agent;
    agent.permissions.mode = mode;
    agent.session?.append({ type: "permission_mode", mode });
    if (mode === "auto") this.notify(t("権限モード auto: すべてのツールを確認なしで実行します（deny・ask のルールは効きます）"), c.red);
    this.updateFooter();
    this.tui.requestRender();
  }

  // ── view helpers ───────────────────────────────────────────────────

  private showText(options: ShowTextOptions): Promise<void> {
    const next = this.approvals.then(() => new Promise<void>((resolve) => {
      this.showInput(new TextViewer(options, () => { this.showEditor(); resolve(); }, copyToClipboard,
        () => this.tui.requestRender(), () => Math.max(1, Math.min(24, this.terminal.rows - 12)), this.host.host.api("core-commands").ui.lang));
    }));
    this.approvals = next.catch(() => {});
    return next;
  }

  /** Show a dialog after any earlier one closes; resolves with what it reports. */
  private ask<T>(make: (done: (value: T) => void) => Component): Promise<T> {
    const next = this.approvals.then(() => new Promise<T>((resolve) => {
      this.showInput(make((value) => {
        this.showEditor();
        resolve(value);
      }));
    }));
    this.approvals = next.catch(() => {});
    return next;
  }

  private pick(title: string, items: SelectOption[], maxVisible = 10): Promise<string | undefined> {
    const next = this.approvals.then(() => new Promise<string | undefined>((resolve) => {
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
    }));
    this.approvals = next.catch(() => {});
    return next;
  }

  /** The rows a dialog in the input slot can have: the screen less the status, the footer and a line of the conversation. */
  private inputRows(): number {
    const width = Math.max(1, this.terminal.columns - 2 * MARGIN);
    return this.terminal.rows - this.status.render(width).length - this.footer.render(width).length - 3;
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
    if (this.activity) this.status.addChild(this.activity);
    for (const q of this.queued) this.status.addChild(new Notice(t("↳ 次のターンで送信: {text}", { text: q.text.split("\n")[0]! })));
    this.tui.requestRender();
  }

  /** Esc / ctrl+c while running: stop, and give back what was queued for the next turn, to edit or send. */
  private interrupt(): void {
    // What the plugins made of it, images included; text alone if they are still at it.
    const back = this.queued.splice(0).map((q) => (q.content ? this.toDraft(q.content) : q.text));
    void this.host.agent.halt();
    // After the draft, not inside it at the cursor; setText would forget the editor's folded pastes.
    if (back.length && this.editor.getText()) {
      this.editor.moveToEnd();
      this.editor.insertTextAtCursor(`\n${back.join("\n")}`);
    } else if (back.length) this.editor.setText(back.join("\n"));
    this.renderStatus();
  }

  /**
   * A user message back as a draft: its text, with each image held behind an [Image #n] marker (as a
   * pasted one is) and sent again with the next message while the marker is kept.
   */
  private toDraft(content: UserContent[]): string {
    const images = content.filter((b) => b.type === "image");
    const taken = new Set([...this.held.keys(), ...[...this.editor.getText().matchAll(IMAGE_MARKER)].map((m) => Number(m[1]))]);
    const used = new Set<number>();
    const hold = (name: string, image: UserContent) => {
      let n = 1;
      while (taken.has(n)) n++;
      taken.add(n);
      this.held.set(n, { name, image });
      return `[Image #${n}]`;
    };
    const text = content
      .flatMap((b) => (b.type === "text" && !b.text.startsWith(INTERRUPTED) ? [b.text] : []))
      .join("\n")
      .replace(IMAGE_LABEL, (label, i: string, name: string) => {
        const image = images[Number(i) - 1];
        if (!image || used.has(Number(i))) return label;
        used.add(Number(i));
        return hold(name, image);
      });
    const rest = images.filter((_, i) => !used.has(i + 1)).map((image) => hold("image", image));
    return [text, ...rest].filter(Boolean).join(" ");
  }

  /** The draft's held images, labeled as the image plugin labels its own, go with it; the rest are dropped. */
  private withHeld(text: string): string | UserContent[] {
    if (!this.held.size) return text;
    const images: UserContent[] = [];
    const out = text.replace(IMAGE_MARKER, (marker, n: string) => {
      const h = this.held.get(Number(n));
      if (!h) return marker;
      images.push(h.image);
      return `[Image ${images.length}: ${h.name}]`;
    });
    this.held.clear();
    return images.length ? [{ type: "text", text: out }, ...images] : text;
  }

  private resetView(): void {
    this.queued = [];
    this.held.clear();
    this.renderStatus();
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
        const view = new AssistantView((call, v) => this.trackTool(call, v), this.host.agent.cwd);
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
    // auto runs everything without asking: it stands out in red, so it is never on unnoticed.
    const mode = t("権限: {mode}", { mode: t(PERMISSION_MODE_LABELS[a.permissions.mode]) });
    const parts = [
      c.gray(`${a.model.provider}/${a.model.id}`),
      c.gray(t("推論: {level}", { level: a.model.reasoning ? a.thinking : t("なし") })),
      a.permissions.mode === "auto" ? c.red(c.bold(mode)) : c.gray(mode),
      c.gray(`ctx ${fmtTokens(this.contextTokens)}/${fmtTokens(a.model.contextWindow)} (${pct}%)`),
    ];
    if (a.session) parts.push(c.gray(`session ${a.session.id}`));
    if (this.totalCost > 0) parts.push(c.gray(`$${this.totalCost.toFixed(3)}`));
    const plugins = [...this.host.host.status.values()];
    this.footer.setText(parts.join(c.gray(" · ")) + (plugins.length ? `\n${c.cyan(plugins.join(" · "))}` : ""));
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
      appendFileSync(this.host.historyPath, `${JSON.stringify(text)}\n`, { mode: 0o600 });
    } catch {}
  }
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
