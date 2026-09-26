// Everything between "the model asked for tools" and "results go back": repair, lookup,
// validation, plugin decisions, permission, parallel execution.
import type { AssistantMessage, ToolCall, ToolResultMessage } from "@sasacode/ai";
import type { ToolDefinition, ToolResult } from "@sasacode/plugin-api";
import type { ApprovalAnswer, ApprovalRequest } from "./agent.ts";
import type { AgentEvent, EventBus } from "./events.ts";
import type { HookRunner } from "./hooks.ts";
import { type Decision, type Judge, type PermissionCheck, PermissionPolicy, type Verdict } from "./permission.ts";
import type { SessionFile } from "./session.ts";
import { validate } from "./validate.ts";

export interface ToolRunContext {
  cwd: string;
  hooks: HookRunner;
  events: EventBus<AgentEvent>;
  permissions: PermissionPolicy;
  approve?: (req: ApprovalRequest) => Promise<ApprovalAnswer>;
  judge: Judge;
  session?: SessionFile;
  findTool(name: string): ToolDefinition<any> | undefined;
  /** Every tool the model could call, deferred ones included (repair candidates). */
  allTools(): ToolDefinition<any>[];
}

/** The call as it will run, after any tool_call_raw repair. */
export interface Repair {
  name: string;
  input: Record<string, unknown>;
  rawInput?: string;
  note: string;
}

export function err(message: string): ToolResult {
  return { content: [{ type: "text", text: message }], isError: true };
}

/**
 * Let tool_call_raw hooks fix names and arguments before anything checks them.
 * The stored message is rewritten to the repaired call (the model imitates its own history), unless
 * it carries provider-signed reasoning, where editing an earlier turn is rejected by the API.
 * What the model really produced is kept in the session as a tool_repair entry.
 */
export async function repairCalls(ctx: ToolRunContext, message: AssistantMessage): Promise<Map<string, Repair>> {
  const repairs = new Map<string, Repair>();
  // A call cut off by max_tokens must never be "completed" into something runnable.
  if (message.stopReason === "max_tokens" || !ctx.hooks.has("tool_call_raw")) return repairs;
  const rewrite = !message.content.some((b) => b.type === "thinking" && b.signature);
  for (const call of message.content) {
    if (call.type !== "tool_call") continue;
    let name = call.name;
    let input = call.input;
    let rawInput = call.rawInput;
    const notes: string[] = [];
    await ctx.hooks.run(
      "tool_call_raw",
      { call, name, input, rawInput, tools: ctx.allTools(), stopReason: message.stopReason },
      (r, ev) => {
        if (r.name) name = ev.name = r.name;
        if (r.input) {
          input = ev.input = r.input;
          rawInput = ev.rawInput = undefined;
        }
        if (r.note) notes.push(r.note);
      },
    );
    if (name === call.name && input === call.input && rawInput === call.rawInput) continue;
    const note = notes.join("; ") || (name !== call.name ? `tool "${call.name}" → "${name}"` : "arguments repaired");
    const original = { name: call.name, input: call.input, rawInput: call.rawInput };
    ctx.session?.append({ type: "tool_repair", callId: call.id, original, repaired: { name, input }, note });
    ctx.events.emit({ type: "tool_repaired", call, from: original, note });
    repairs.set(call.id, { name, input, rawInput, note });
    if (rewrite) {
      call.name = name;
      call.input = input;
      delete call.rawInput;
    }
  }
  return repairs;
}

interface Job {
  call: ToolCall;
  name: string;
  tool?: ToolDefinition<any>;
  args?: Record<string, unknown>;
  result?: ToolResult;
  note?: string;
}

/**
 * Runs the calls of one response. Results are passed to `onResult` in call order, each as soon as
 * it and those before it are final, so they are saved before the next call starts (a crash then
 * loses at most the calls in flight).
 */
export async function executeTools(
  ctx: ToolRunContext,
  calls: ToolCall[],
  opts: { truncated: boolean; signal: AbortSignal; repairs: Map<string, Repair>; onResult: (m: ToolResultMessage) => void },
): Promise<void> {
  const jobs: Job[] = [];
  let flushed = 0;
  const messages = new Map<Job, ToolResultMessage>();
  const finish = async (job: Job, result: ToolResult, didRun: boolean) => {
    await ctx.hooks.run("tool_result", { call: job.call, result, ran: didRun }, (r, ev) => {
      if (r.result) result = ev.result = r.result;
    });
    if (didRun) ctx.events.emit({ type: "tool_end", call: job.call, result });
    job.result = result;
    messages.set(job, {
      role: "tool",
      toolCallId: job.call.id,
      toolName: job.name,
      // Tell the model briefly what was fixed, so it can produce the right form next time.
      content: job.note ? [{ type: "text", text: `[harness repaired this call: ${job.note}]` }, ...result.content] : result.content,
      isError: !!result.isError,
      timestamp: Date.now(),
    });
    while (flushed < jobs.length && messages.has(jobs[flushed]!)) opts.onResult(messages.get(jobs[flushed++]!)!);
  };

  // Validation and permission prompts run one at a time, in order.
  for (const call of calls) {
    const fixed = opts.repairs.get(call.id);
    const job: Job = { call, name: fixed?.name ?? call.name, note: fixed?.note };
    jobs.push(job);
    const rejected = preflight(ctx, job, fixed?.input ?? call.input, fixed ? fixed.rawInput : call.rawInput, opts);
    if (rejected) {
      await finish(job, rejected, false);
      continue;
    }
    const { args, denied } = await authorize(ctx, job.tool!, call, fixed?.input ?? call.input);
    if (denied) await finish(job, denied, false);
    else job.args = args;
  }

  // Runs of concurrency-safe tools execute in parallel; everything else in order.
  let i = 0;
  while (i < jobs.length) {
    const batch: Job[] = [];
    if (jobs[i]!.tool?.concurrent) {
      while (i < jobs.length && (jobs[i]!.result || jobs[i]!.tool?.concurrent)) batch.push(jobs[i++]!);
    } else batch.push(jobs[i++]!);
    await Promise.all(
      batch.map(async (j) => {
        if (j.result) return;
        // Approvals happen up front; a call must not start once the user has interrupted.
        if (opts.signal.aborted) return finish(j, err("Interrupted by the user before this tool ran."), false);
        await finish(j, await runTool(ctx, j.tool!, j.call, j.args!, opts.signal), true);
      }),
    );
  }
}

/** Checks that need no user: returns an error result when the call cannot run. */
function preflight(
  ctx: ToolRunContext,
  job: Job,
  input: Record<string, unknown>,
  rawInput: string | undefined,
  opts: { truncated: boolean; signal: AbortSignal },
): ToolResult | undefined {
  if (opts.signal.aborted) return err("Interrupted by the user before this tool ran.");
  if (opts.truncated)
    return err("The response hit max_tokens before this tool call was complete, so it was not run. Retry with smaller input.");
  const tool = ctx.findTool(job.name);
  if (!tool) return err(`Unknown tool "${job.name}". Available: ${ctx.allTools().map((t) => t.name).join(", ")}`);
  if (rawInput !== undefined)
    return err(`Tool input was not valid JSON, so it was not run: ${JSON.stringify({ INVALID_JSON: rawInput })}`);
  const problems = validate(tool.parameters, input);
  if (problems.length) return err(`Invalid arguments: ${problems.join("; ")}`);
  job.tool = tool;
}

type Current = Omit<Verdict, "source"> & { source: Verdict["source"] | "plugin" };

/**
 * A tool_call plugin's decision replaces the permission mode's default, but never the user's
 * explicit deny/ask rules: the stricter of the two wins. Then permission hooks may change the
 * verdict within limits (see lowestFor), and an agent-mode call nobody decided goes to the judge.
 */
async function decide(ctx: ToolRunContext, check: PermissionCheck, call: ToolCall, forced?: { decision: Decision; reason: string }): Promise<Current> {
  const policy = await ctx.permissions.check(check);
  let v: Current = policy;
  if (forced && !(policy.source !== "mode" && RANK[policy.decision] > RANK[forced.decision])) v = { ...forced, source: "plugin" };
  if (ctx.hooks.has("permission")) {
    const lowest = lowestFor(v);
    const source = v.source;
    const verdict = { decision: v.decision, reason: v.reason, source };
    await ctx.hooks.run("permission", { call, tool: check.tool, args: check.args, cwd: check.cwd, mode: ctx.permissions.mode, verdict, lowest }, (r, ev) => {
      if (!r.decision) return;
      const decision = RANK[r.decision] < RANK[lowest] ? lowest : r.decision;
      v = { decision, reason: r.reason ?? v.reason, source };
      ev.verdict = { decision, reason: v.reason, source };
    });
  }
  if (v.needsJudge) v = await ctx.permissions.judged(check, ctx.judge);
  return v;
}

/**
 * How far a permission hook may loosen a verdict: a mode default all the way; a softDeny rule
 * only as far as asking the user; the user's rules and other plugins' decisions not at all.
 */
function lowestFor(v: Current): Decision {
  if (v.source === "mode") return "allow";
  if (v.source === "soft") return "ask";
  return v.decision;
}

const RANK: Record<Decision, number> = { allow: 0, ask: 1, deny: 2 };

/** Plugins see the call first (tool_call hook), then the core policy and the user decide. */
async function authorize(
  ctx: ToolRunContext,
  tool: ToolDefinition<any>,
  call: ToolCall,
  input: Record<string, unknown>,
): Promise<{ args: Record<string, unknown>; denied?: ToolResult }> {
  let args = input;
  let forced: { decision: Decision; reason: string } | undefined;
  await ctx.hooks.run("tool_call", { call, tool, args }, (r, ev) => {
    if (r.args) args = ev.args = r.args;
    if (r.decision && (!forced || RANK[r.decision] > RANK[forced.decision])) forced = { decision: r.decision, reason: r.reason ?? "plugin" };
    return forced?.decision !== "deny";
  });
  const check: PermissionCheck = { tool, args, cwd: ctx.cwd };
  const verdict = await decide(ctx, check, call, forced);
  if (verdict.decision === "allow") return { args };
  if (verdict.decision === "deny") return { args, denied: err(`Permission denied (${verdict.reason}).`) };
  if (!ctx.approve)
    return {
      args,
      denied: err(`This call needs user approval (${verdict.reason}), but no user is available to approve it (non-interactive run). It was not executed.`),
    };
  const answer = await ctx.approve({ ...check, call, reason: verdict.reason });
  if (answer.decision === "always") ctx.permissions.addRules({ allow: [PermissionPolicy.ruleFor(check)] });
  if (answer.decision === "deny")
    return { args, denied: err(`The user denied this tool call.${answer.feedback ? ` User feedback: ${answer.feedback}` : ""}`) };
  return { args };
}

async function runTool(ctx: ToolRunContext, tool: ToolDefinition<any>, call: ToolCall, args: Record<string, unknown>, signal: AbortSignal): Promise<ToolResult> {
  let summary = "";
  try {
    summary = tool.summary?.(args) ?? "";
  } catch {}
  ctx.events.emit({ type: "tool_start", call, summary });
  try {
    return await tool.execute(args, { cwd: ctx.cwd, signal, onUpdate: (text) => ctx.events.emit({ type: "tool_update", call, text }) });
  } catch (e) {
    return err(`Tool failed: ${e instanceof Error ? e.message : String(e)}`);
  }
}
