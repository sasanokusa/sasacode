import {
  type AssistantMessage,
  ContextOverflowError,
  getProvider,
  type Message,
  type ModelInfo,
  type ThinkingLevel,
  type ToolCall,
  type ToolResultMessage,
  type UserContent,
  type UserMessage,
} from "@sasacode/ai";
import type { ToolDefinition, ToolResult } from "@sasacode/plugin-api";
import { type AgentEvent, EventBus, type StopCause } from "./events.ts";
import { type PermissionCheck, PermissionPolicy } from "./permission.ts";
import type { SessionFile } from "./session.ts";
import { validate } from "./validate.ts";

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
  session?: SessionFile;
  /** Unlimited unless set (A7). */
  maxTurns?: number;
  maxRetries?: number;
}

const INTERRUPTED_NOTE = "[The user interrupted the previous response.]";

export class Agent {
  readonly events = new EventBus<AgentEvent>();
  messages: Message[];
  model: ModelInfo;
  thinking: ThinkingLevel;
  cwd: string;
  systemPrompt: string;
  permissions: PermissionPolicy;
  session?: SessionFile;
  maxTurns?: number;
  approve?: AgentOptions["approve"];
  private tools = new Map<string, ToolDefinition<any>>();
  private opts: AgentOptions;
  private abortController?: AbortController;
  private queued: UserContent[][] = [];
  private interrupted = false;
  private running?: Promise<StopCause>;

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
    for (const t of opts.tools ?? []) this.setTool(t);
  }

  setTool(tool: ToolDefinition<any>): void {
    this.tools.set(tool.name, tool);
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
    const content: UserContent[] = typeof input === "string" ? [{ type: "text", text: input }] : input;
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

  private start(): Promise<StopCause> {
    this.running = this.run().finally(() => {
      this.running = undefined;
      // Input that arrived as the run was finishing starts the next run.
      if (this.drainQueue()) void this.start();
    });
    return this.running;
  }

  abort(): void {
    this.abortController?.abort();
  }

  async waitForIdle(): Promise<void> {
    await this.running;
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
    try {
      while (true) {
        if (this.maxTurns && turn >= this.maxTurns) {
          cause = "max_turns";
          break;
        }
        turn++;
        this.events.emit({ type: "turn_start", turn });
        const msg = await this.streamAssistant(ac.signal);
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
          const results = await this.executeTools(calls, msg.stopReason === "max_tokens", ac.signal);
          for (const r of results) this.pushMessage(r);
        }
        this.events.emit({ type: "turn_end", turn });
        if (ac.signal.aborted) {
          this.interrupted = true;
          cause = "aborted";
          break;
        }
        const used = msg.usage.input + msg.usage.cacheRead + msg.usage.cacheWrite + msg.usage.output;
        if (used >= this.model.contextWindow - Math.min(this.model.maxOutput, 16_384)) {
          this.events.emit({ type: "context_limit", tokens: used, contextWindow: this.model.contextWindow });
          cause = "context_limit";
          break;
        }
        if (!this.drainQueue() && !calls.length) break;
      }
    } catch (e) {
      if (e instanceof ContextOverflowError) {
        this.events.emit({ type: "context_limit", tokens: -1, contextWindow: this.model.contextWindow });
        cause = "context_limit";
      } else {
        this.events.emit({ type: "error", error: e instanceof Error ? e.message : String(e) });
        cause = "error";
      }
    } finally {
      this.abortController = undefined;
    }
    this.events.emit({ type: "agent_end", cause });
    return cause;
  }

  private async streamAssistant(signal: AbortSignal): Promise<AssistantMessage> {
    const provider = getProvider(this.model.api);
    const apiKey = await this.opts.getApiKey?.(this.model.provider);
    this.session?.addSecret(apiKey);
    let final: AssistantMessage | undefined;
    for await (const ev of provider.stream({
      model: this.model,
      apiKey,
      system: this.systemPrompt,
      messages: this.messages,
      tools: this.getTools().map((t) => ({ name: t.name, description: t.description, parameters: t.parameters })),
      thinking: this.thinking,
      maxRetries: this.opts.maxRetries,
      signal,
    })) {
      if (ev.type === "start") this.events.emit({ type: "message_start", message: ev.partial });
      else if (ev.type === "done") final = ev.message;
      else this.events.emit({ type: "message_update", message: ev.partial, event: ev });
    }
    if (!final) throw new Error("provider stream ended without a final message");
    if (final.stopReason === "aborted") {
      // Half-streamed tool calls never ran; keep only what the model finished saying.
      final.content = final.content.filter((c) => c.type === "text" || (c.type === "thinking" && c.signature));
      this.interrupted = true;
      if (!final.content.length) {
        this.events.emit({ type: "message_end", message: final });
        return final;
      }
    }
    this.pushMessage(final);
    return final;
  }

  private async executeTools(calls: ToolCall[], truncated: boolean, signal: AbortSignal): Promise<ToolResultMessage[]> {
    type Job = { call: ToolCall; tool?: ToolDefinition<any>; result?: ToolResult };
    const jobs: Job[] = [];
    // Validation and permission prompts run one at a time, in order.
    for (const call of calls) {
      const job: Job = { call };
      jobs.push(job);
      if (signal.aborted) {
        job.result = err("Interrupted by the user before this tool ran.");
        continue;
      }
      const tool = this.tools.get(call.name);
      if (!tool) {
        job.result = err(`Unknown tool "${call.name}". Available: ${[...this.tools.keys()].join(", ")}`);
        continue;
      }
      if (truncated) {
        job.result = err("The response hit max_tokens before this tool call was complete, so it was not run. Retry with smaller input.");
        continue;
      }
      if (call.rawInput !== undefined) {
        job.result = err(`Tool input was not valid JSON, so it was not run: ${JSON.stringify({ INVALID_JSON: call.rawInput })}`);
        continue;
      }
      const problems = validate(tool.parameters, call.input);
      if (problems.length) {
        job.result = err(`Invalid arguments: ${problems.join("; ")}`);
        continue;
      }
      const denied = await this.authorize(tool, call);
      if (denied) job.result = denied;
      else job.tool = tool;
    }

    // Runs of concurrency-safe tools execute in parallel; everything else in order.
    let i = 0;
    while (i < jobs.length) {
      const batch: Job[] = [];
      const first = jobs[i]!;
      if (first.tool?.concurrent) {
        while (i < jobs.length && (jobs[i]!.result || jobs[i]!.tool?.concurrent)) batch.push(jobs[i++]!);
      } else batch.push(jobs[i++]!);
      await Promise.all(batch.map((j) => (j.tool && !j.result ? this.runTool(j.tool, j.call, signal).then((r) => (j.result = r)) : undefined)));
    }

    return jobs.map(({ call, result }) => ({
      role: "tool",
      toolCallId: call.id,
      toolName: call.name,
      content: result!.content,
      isError: !!result!.isError,
      timestamp: Date.now(),
    }));
  }

  /** Returns an error result when the call may not run. */
  private async authorize(tool: ToolDefinition<any>, call: ToolCall): Promise<ToolResult | undefined> {
    const check: PermissionCheck = { tool, args: call.input, cwd: this.cwd };
    const verdict = await this.permissions.check(check, (c) => this.judge(c));
    if (verdict.decision === "allow") return;
    if (verdict.decision === "deny") return err(`Permission denied (${verdict.reason}).`);
    if (!this.approve)
      return err(
        `This call needs user approval (${verdict.reason}), but no user is available to approve it (non-interactive run). It was not executed.`,
      );
    const answer = await this.approve({ ...check, call, reason: verdict.reason });
    if (answer.decision === "always") this.permissions.addRules({ allow: [PermissionPolicy.ruleFor(check)] });
    if (answer.decision === "deny")
      return err(`The user denied this tool call.${answer.feedback ? ` User feedback: ${answer.feedback}` : ""}`);
  }

  private async runTool(tool: ToolDefinition<any>, call: ToolCall, signal: AbortSignal): Promise<ToolResult> {
    this.events.emit({ type: "tool_start", call, summary: tool.summary?.(call.input) ?? "" });
    let result: ToolResult;
    try {
      result = await tool.execute(call.input, {
        cwd: this.cwd,
        signal,
        onUpdate: (text) => this.events.emit({ type: "tool_update", call, text }),
      });
    } catch (e) {
      result = err(`Tool failed: ${e instanceof Error ? e.message : String(e)}`);
    }
    this.events.emit({ type: "tool_end", call, result });
    return result;
  }

  /** "agent" permission mode: ask the current model whether a call is safe. */
  private async judge(c: PermissionCheck): Promise<{ safe: boolean; reason: string }> {
    const provider = getProvider(this.model.api);
    const prompt = `A coding agent working in ${c.cwd} wants to run this tool call:
tool: ${c.tool.name}
arguments: ${JSON.stringify(c.args)}

Is it safe to run without asking the user? Safe means read-only or confined to the working directory, reversible, and not touching credentials, the network in a harmful way, or system settings.
Answer with one line of JSON: {"safe": true|false, "reason": "<short reason>"}`;
    let text = "";
    for await (const ev of provider.stream({
      model: this.model,
      apiKey: await this.opts.getApiKey?.(this.model.provider),
      system: "You review tool calls for safety.",
      messages: [{ role: "user", content: [{ type: "text", text: prompt }], timestamp: Date.now() }],
      tools: [],
      thinking: "low",
      maxTokens: 2048,
    })) {
      if (ev.type === "done") text = ev.message.content.map((b) => (b.type === "text" ? b.text : "")).join("");
    }
    const m = /\{[\s\S]*\}/.exec(text);
    if (!m) throw new Error("judge gave no verdict");
    const v = JSON.parse(m[0]);
    return { safe: v.safe === true, reason: String(v.reason ?? "") };
  }
}

function err(message: string): ToolResult {
  return { content: [{ type: "text", text: message }], isError: true };
}
