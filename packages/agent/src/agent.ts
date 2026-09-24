import {
  type AssistantMessage,
  ContextOverflowError,
  getProvider,
  type Message,
  type ModelInfo,
  type SamplingOptions,
  textOf,
  type ThinkingLevel,
  type ToolCall,
  type UserContent,
  type UserMessage,
} from "@sasacode/ai";
import type { CompleteOptions, HookName, StopCause, SubagentOptions, ToolDefinition } from "@sasacode/plugin-api";
import { type AgentEvent, EventBus } from "./events.ts";
import { HookRunner } from "./hooks.ts";
import { type PermissionCheck, PermissionPolicy } from "./permission.ts";
import type { SessionFile } from "./session.ts";
import { loadedFromHistory, makeToolSearchTool, shouldDefer, type ToolSearchConfig } from "./tool-search.ts";
import { executeTools, type Repair, repairCalls, type ToolRunContext } from "./tool-exec.ts";

export interface ApprovalRequest extends PermissionCheck {
  call: ToolCall;
  reason: string;
}

export type ApprovalAnswer = { decision: "allow" | "always" | "deny"; feedback?: string };

export interface AgentOptions {
  model: ModelInfo;
  cwd: string;
  systemPrompt: string;
  tools?: ToolDefinition<any>[];
  messages?: Message[];
  thinking?: ThinkingLevel;
  permissions?: PermissionPolicy;
  /** Asks the user about a tool call. Without one, calls that need approval are denied. */
  approve?: (req: ApprovalRequest) => Promise<ApprovalAnswer>;
  getApiKey?: (provider: string) => Promise<string | undefined>;
  /** Turns "<provider>/<model>" into ModelInfo, for subagents and complete() with another model. */
  resolveModel?: (spec: string) => ModelInfo;
  session?: SessionFile;
  hooks?: HookRunner;
  toolSearch?: ToolSearchConfig;
  /** Unlimited unless set (A7). */
  maxTurns?: number;
  maxRetries?: number;
}

const INTERRUPTED_NOTE = "[The user interrupted the previous response.]";
/** Hooks that belong to the user's session, not to subagents. */
const SESSION_HOOKS: HookName[] = ["session_start", "session_end", "user_prompt", "turn_end", "agent_end", "context_limit"];

export class Agent {
  readonly events = new EventBus<AgentEvent>();
  readonly hooks: HookRunner;
  messages: Message[];
  model: ModelInfo;
  thinking: ThinkingLevel;
  cwd: string;
  systemPrompt: string;
  permissions: PermissionPolicy;
  session?: SessionFile;
  maxTurns?: number;
  approve?: AgentOptions["approve"];
  toolSearch: ToolSearchConfig;
  private tools = new Map<string, ToolDefinition<any>>();
  private loadedTools = new Set<string>();
  private searchTool: ToolDefinition<any>;
  private opts: AgentOptions;
  private abortController?: AbortController;
  private queued: UserContent[][] = [];
  private interrupted = false;
  private running?: Promise<StopCause>;
  private isSubagent = false;
  /** Bumped by replaceMessages, so usage figures from before a compaction are not trusted. */
  private generation = 0;

  constructor(opts: AgentOptions) {
    this.opts = opts;
    this.model = opts.model;
    this.cwd = opts.cwd;
    this.systemPrompt = opts.systemPrompt;
    this.messages = opts.messages ?? [];
    this.thinking = opts.thinking ?? "high";
    this.permissions = opts.permissions ?? new PermissionPolicy();
    this.session = opts.session;
    this.maxTurns = opts.maxTurns;
    this.approve = opts.approve;
    this.toolSearch = opts.toolSearch ?? {};
    this.hooks = opts.hooks ?? new HookRunner();
    this.hooks.onError ??= (plugin, hook, error) =>
      this.events.emit({ type: "plugin_error", plugin, hook, error: error instanceof Error ? error.message : String(error) });
    this.searchTool = makeToolSearchTool(
      () => this.deferredTools(),
      (names) => names.forEach((n) => this.loadedTools.add(n)),
    );
    for (const t of opts.tools ?? []) this.setTool(t);
  }

  setTool(tool: ToolDefinition<any>): void {
    this.tools.set(tool.name, tool);
  }

  removeTool(name: string): void {
    this.tools.delete(name);
  }

  getTools(): ToolDefinition<any>[] {
    return [...this.tools.values()];
  }

  get isRunning(): boolean {
    return !!this.running;
  }

  setModel(model: ModelInfo): void {
    this.model = model;
    this.session?.append({ type: "model", model: `${model.provider}/${model.id}` });
  }

  /** Send user input. While a run is in progress it is queued and delivered at the next turn (FR-L05). */
  async prompt(input: string | UserContent[]): Promise<StopCause> {
    let content: UserContent[] = typeof input === "string" ? [{ type: "text", text: input }] : input;
    let handled = false;
    await this.hooks.run("user_prompt", { content }, (r) => {
      if (r.content) content = r.content;
      if (r.handled) handled = true;
      return !handled;
    });
    if (handled) return "done";
    if (this.running) {
      this.queued.push(content);
      return this.running;
    }
    this.pushUser(content);
    return this.start();
  }

  /** Continue without new input, e.g. after resuming a session. */
  async continue(): Promise<StopCause> {
    if (this.running) return this.running;
    return this.start();
  }

  /** Add a user message from a plugin: "next" waits for the next turn or prompt, "now" also starts a run. */
  inject(content: UserContent[], when: "next" | "now" = "next"): void {
    if (this.running || when === "next") this.queued.push(content);
    else {
      this.pushUser(content);
      void this.start();
    }
  }

  /** Swap the conversation (compaction, fork). Persisted as a replace entry. */
  replaceMessages(messages: Message[]): void {
    this.messages = messages;
    this.generation++;
    this.session?.append({ type: "replace", messages });
    this.events.emit({ type: "messages_replaced" });
  }

  abort(): void {
    this.abortController?.abort();
  }

  async waitForIdle(): Promise<void> {
    while (this.running) await this.running;
  }

  private start(): Promise<StopCause> {
    this.running = this.run().finally(() => {
      this.running = undefined;
      // Input that arrived as the run was finishing starts the next run.
      if (this.drainQueue()) void this.start();
    });
    return this.running;
  }

  private pushUser(content: UserContent[]): void {
    if (this.interrupted) {
      content = [{ type: "text", text: INTERRUPTED_NOTE }, ...content];
      this.interrupted = false;
    }
    this.pushMessage({ role: "user", content, timestamp: Date.now() } satisfies UserMessage);
  }

  private pushMessage(m: Message): void {
    this.messages.push(m);
    this.session?.append({ type: "message", message: m });
    this.events.emit({ type: "message_end", message: m });
  }

  private drainQueue(): boolean {
    if (!this.queued.length) return false;
    for (const c of this.queued.splice(0)) this.pushUser(c);
    return true;
  }

  private async run(): Promise<StopCause> {
    const ac = new AbortController();
    this.abortController = ac;
    this.events.emit({ type: "agent_start" });
    let cause: StopCause = "done";
    let turn = 0;
    let limitRetries = 0;
    let stopped: { plugin: string; reason: string } | undefined;
    try {
      while (true) {
        if (this.maxTurns && turn >= this.maxTurns) {
          cause = "max_turns";
          break;
        }
        turn++;
        this.events.emit({ type: "turn_start", turn });
        let turnResult: { message: AssistantMessage; repairs: Map<string, Repair> };
        try {
          turnResult = await this.respond(ac.signal);
        } catch (e) {
          if (!(e instanceof ContextOverflowError)) throw e;
          if (await this.contextLimit(-1, limitRetries++)) continue;
          cause = "context_limit";
          break;
        }
        const { message: msg, repairs } = turnResult;
        const calls = msg.content.filter((c): c is ToolCall => c.type === "tool_call");
        if (msg.stopReason === "aborted") {
          cause = "aborted";
          break;
        }
        if (msg.stopReason === "refusal") {
          cause = "refusal";
          break;
        }
        if (calls.length) {
          await executeTools(this.toolContext(), calls, {
            truncated: msg.stopReason === "max_tokens",
            signal: ac.signal,
            repairs,
            onResult: (r) => this.pushMessage(r),
          });
        }
        this.events.emit({ type: "turn_end", turn });
        if (ac.signal.aborted) {
          this.interrupted = true;
          cause = "aborted";
          break;
        }
        let injected = false;
        const gen = this.generation;
        await this.hooks.run("turn_end", { turn, message: msg }, (r, _ev, plugin) => {
          if (r.stop) {
            stopped = { plugin, reason: r.stop };
            return false;
          }
          if (r.inject) {
            this.queued.push([{ type: "text", text: r.inject }]);
            injected = true;
          }
        });
        if (stopped) {
          cause = "stopped";
          break;
        }
        const used = msg.usage.input + msg.usage.cacheRead + msg.usage.cacheWrite + msg.usage.output;
        if (gen === this.generation && used >= this.model.contextWindow - Math.min(this.model.maxOutput, 16_384)) {
          if (await this.contextLimit(used, limitRetries++)) continue;
          cause = "context_limit";
          break;
        }
        limitRetries = 0;
        if (!this.drainQueue() && !calls.length && !injected) break;
      }
    } catch (e) {
      this.events.emit({ type: "error", error: e instanceof Error ? e.message : String(e) });
      cause = "error";
    } finally {
      this.abortController = undefined;
    }
    await this.hooks.run("agent_end", { cause, stopped }, (r) => {
      if (r.inject) this.queued.push([{ type: "text", text: r.inject }]);
    });
    this.events.emit({ type: "agent_end", cause, stopped });
    return cause;
  }

  /** Emits the event and lets plugins free up context. True when the loop should continue. */
  private async contextLimit(tokens: number, retries: number): Promise<boolean> {
    this.events.emit({ type: "context_limit", tokens, contextWindow: this.model.contextWindow });
    if (retries >= 2) return false;
    let retry = false;
    await this.hooks.run("context_limit", { tokens, contextWindow: this.model.contextWindow }, (r) => {
      if (r.retry) retry = true;
    });
    return retry;
  }

  // ── requests ───────────────────────────────────────────────────────

  private deferredTools(): ToolDefinition<any>[] {
    return this.getTools().filter((t) => !t.alwaysLoad);
  }

  /** Tools sent with a request: everything, or the always-loaded ones plus tool_search when deferring (5.4). */
  requestTools(): ToolDefinition<any>[] {
    const all = this.getTools();
    if (!shouldDefer(this.deferredTools(), this.model, this.toolSearch)) return all;
    const loaded = loadedFromHistory(this.messages);
    for (const n of this.loadedTools) loaded.add(n);
    return [...all.filter((t) => t.alwaysLoad || loaded.has(t.name)), this.searchTool];
  }

  /**
   * One model response, settled: streamed (stream_delta may stop it), passed through
   * assistant_message (which may rewrite it, ask again, or add a follow-up), its tool calls
   * repaired (tool_call_raw), and stored.
   */
  private async respond(signal: AbortSignal): Promise<{ message: AssistantMessage; repairs: Map<string, Repair> }> {
    for (let attempt = 0; ; attempt++) {
      const { message, stopped } = await this.stream(signal);
      const userAborted = message.stopReason === "aborted";
      if (userAborted || stopped) {
        // Half-streamed tool calls never ran and unsigned thinking cannot be replayed; keep what was finished.
        message.content = message.content.filter((c) => c.type === "text" || (c.type === "thinking" && c.signature));
      }
      if (userAborted) {
        this.interrupted = true;
        if (message.content.length) this.pushMessage(message);
        else this.events.emit({ type: "message_end", message });
        return { message, repairs: new Map() };
      }
      let final = message;
      let retry = false;
      let inject: string | undefined;
      await this.hooks.run("assistant_message", { message, stopped }, (r, ev) => {
        if (r.message) final = ev.message = r.message;
        if (r.retry) retry = true;
        if (r.inject) inject = r.inject;
      });
      if (retry && attempt < 2 && !signal.aborted) {
        this.events.emit({ type: "message_discarded", message: final });
        continue;
      }
      const repairs = await repairCalls(this.toolContext(), final);
      if (final.content.length) this.pushMessage(final);
      else this.events.emit({ type: "message_end", message: final });
      if (inject) this.queued.push([{ type: "text", text: inject }]);
      return { message: final, repairs };
    }
  }

  /** Stream one response. A stream_delta handler can end it early; that is reported as `stopped`. */
  private async stream(signal: AbortSignal): Promise<{ message: AssistantMessage; stopped?: { plugin: string; reason: string } }> {
    const provider = getProvider(this.model.api);
    const apiKey = await this.opts.getApiKey?.(this.model.provider);
    this.session?.addSecret(apiKey);
    let system = this.systemPrompt;
    await this.hooks.run("system_prompt", { prompt: system }, (r, ev) => {
      if (r.prompt !== undefined) system = ev.prompt = r.prompt;
    });
    let messages = this.messages;
    let sampling: SamplingOptions = {};
    await this.hooks.run("before_request", { messages: [...messages], model: this.model, sampling }, (r, ev) => {
      if (r.messages) messages = ev.messages = r.messages;
      if (r.sampling) sampling = ev.sampling = { ...sampling, ...r.sampling };
    });

    // The request has its own controller so a plugin can stop it without it counting as a user abort.
    const request = new AbortController();
    const forward = () => request.abort();
    signal.addEventListener("abort", forward, { once: true });
    const watchDeltas = this.hooks.has("stream_delta");
    const toolJson = new Map<number, string>();
    let stopped: { plugin: string; reason: string } | undefined;
    let final: AssistantMessage | undefined;
    try {
      for await (const ev of provider.stream({
        model: this.model,
        apiKey,
        system,
        messages,
        tools: this.requestTools().map((t) => ({ name: t.name, description: t.description, parameters: t.parameters })),
        thinking: this.thinking,
        maxRetries: this.opts.maxRetries,
        sampling,
        signal: request.signal,
      })) {
        if (ev.type === "start") this.events.emit({ type: "message_start", message: ev.partial });
        else if (ev.type === "done") final = ev.message;
        else {
          this.events.emit({ type: "message_update", message: ev.partial, event: ev });
          if (watchDeltas && !stopped && ev.type !== "toolcall_start") {
            const block = ev.partial.content[ev.index];
            const kind = ev.type === "text_delta" ? "text" : ev.type === "thinking_delta" ? "thinking" : "toolcall";
            if (kind === "toolcall") toolJson.set(ev.index, (toolJson.get(ev.index) ?? "") + ev.delta);
            const text = block?.type === "text" ? block.text : block?.type === "thinking" ? block.thinking : (toolJson.get(ev.index) ?? "");
            this.hooks.runSync("stream_delta", { kind, index: ev.index, delta: ev.delta, text, message: ev.partial }, (r, _e, owner) => {
              if (!r.stop) return;
              stopped = { plugin: owner, reason: r.stop };
              request.abort();
              return false;
            });
          }
        }
      }
    } finally {
      signal.removeEventListener("abort", forward);
    }
    if (!final) throw new Error("provider stream ended without a final message");
    if (stopped && !signal.aborted) final.stopReason = "stopped";
    else stopped = undefined;
    return { message: final, stopped };
  }

  private findTool(name: string): ToolDefinition<any> | undefined {
    return this.tools.get(name) ?? (name === this.searchTool.name ? this.searchTool : undefined);
  }

  private toolContext(): ToolRunContext {
    return {
      cwd: this.cwd,
      hooks: this.hooks,
      events: this.events,
      permissions: this.permissions,
      approve: this.approve,
      judge: (c) => this.judge(c),
      session: this.session,
      findTool: (n) => this.findTool(n),
      allTools: () => [...this.getTools(), this.searchTool],
    };
  }

  // ── plugin services ────────────────────────────────────────────────

  /** One model call without tools. */
  async complete(o: CompleteOptions): Promise<string> {
    const model = o.model ? this.resolve(o.model) : this.model;
    let text = "";
    for await (const ev of getProvider(model.api).stream({
      model,
      apiKey: await this.opts.getApiKey?.(model.provider),
      system: o.system,
      messages: o.messages,
      tools: [],
      thinking: "low",
      maxTokens: o.maxTokens ?? 8192,
      maxRetries: this.opts.maxRetries,
      signal: o.signal,
    })) {
      if (ev.type === "done") {
        if (ev.message.stopReason === "aborted") throw new Error("aborted");
        text = textOf(ev.message.content);
      }
    }
    return text;
  }

  /** An independent loop sharing tools, permissions, approvals and tool hooks, with its own history. */
  async runSubagent(o: SubagentOptions): Promise<{ text: string; messages: Message[]; cause: StopCause }> {
    const exclude = new Set(o.excludeTools ?? []);
    const tools = this.getTools().filter((t) => (o.tools ? o.tools.includes(t.name) : !exclude.has(t.name)));
    const child = new Agent({
      ...this.opts,
      model: o.model ? this.resolve(o.model) : this.model,
      cwd: this.cwd,
      systemPrompt: o.systemPrompt ?? this.systemPrompt,
      tools,
      messages: [],
      thinking: this.thinking,
      permissions: this.permissions,
      approve: this.approve,
      session: undefined,
      hooks: this.hooks.without(SESSION_HOOKS),
      toolSearch: this.toolSearch,
    });
    child.isSubagent = true;
    child.events.on((e) => {
      if (e.type === "tool_start") o.onProgress?.(`${e.call.name}(${e.summary})`);
      if (e.type === "plugin_error") this.events.emit(e);
    });
    const onAbort = () => child.abort();
    o.signal?.addEventListener("abort", onAbort, { once: true });
    try {
      const cause = await child.prompt(o.prompt);
      const last = [...child.messages].reverse().find((m) => m.role === "assistant");
      return { text: last ? textOf(last.content) : "", messages: child.messages, cause };
    } finally {
      o.signal?.removeEventListener("abort", onAbort);
    }
  }

  get subagent(): boolean {
    return this.isSubagent;
  }

  private resolve(spec: string): ModelInfo {
    if (!this.opts.resolveModel) throw new Error("model resolution is not available");
    return this.opts.resolveModel(spec);
  }

  /** "agent" permission mode: ask the current model whether a call is safe. */
  private async judge(c: PermissionCheck): Promise<{ safe: boolean; reason: string }> {
    const text = await this.complete({
      system: "You review tool calls for safety.",
      maxTokens: 2048,
      messages: [
        {
          role: "user",
          timestamp: Date.now(),
          content: [
            {
              type: "text",
              text: `A coding agent working in ${c.cwd} wants to run this tool call:
tool: ${c.tool.name}
arguments: ${JSON.stringify(c.args)}

Is it safe to run without asking the user? Safe means read-only or confined to the working directory, reversible, and not touching credentials, the network in a harmful way, or system settings.
Answer with one line of JSON: {"safe": true|false, "reason": "<short reason>"}`,
            },
          ],
        },
      ],
    });
    const m = /\{[\s\S]*\}/.exec(text);
    if (!m) throw new Error("judge gave no verdict");
    const v = JSON.parse(m[0]);
    return { safe: v.safe === true, reason: String(v.reason ?? "") };
  }
}
