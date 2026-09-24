import type { Plugin, ToolResult } from "@sasacode/plugin-api";

interface Step {
  call: string;
  result: string;
  isError: boolean;
  /** The call changed files, so repeating earlier calls afterwards is legitimate progress. */
  changes: boolean;
}

/** Stable text for comparing calls and results (key order does not matter). */
function stable(v: unknown): string {
  if (Array.isArray(v)) return `[${v.map(stable).join(",")}]`;
  if (v && typeof v === "object")
    return `{${Object.keys(v)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${stable((v as Record<string, unknown>)[k])}`)
      .join(",")}}`;
  return JSON.stringify(v);
}

const resultText = (r: ToolResult) => r.content.map((c) => (c.type === "text" ? c.text : `[${c.mediaType}]`)).join("\n");

/** Longest cycle looked for: A A A …, A B A B …, A B C A B C …. */
const MAX_PERIOD = 3;

/**
 * How many times the last `period` steps occur back to back at the end of `history` (same calls,
 * same results, no file changed in between). 1 means no repeat.
 */
function repeats(history: Step[], period: number): number {
  const n = history.length;
  if (n < period) return 0;
  let same = 0;
  for (let i = n - 1; i - period >= 0; i--) {
    const [a, b] = [history[i]!, history[i - period]!];
    if (a.changes || b.changes || a.call !== b.call || a.result !== b.result) break;
    same++;
  }
  return 1 + Math.floor(same / period);
}

/** Consecutive identical errors at the end of `history`, stopping at a file change. */
function errorStreak(history: Step[], result: string): number {
  let n = 0;
  for (let i = history.length - 1; i >= 0; i--) {
    const s = history[i]!;
    if (s.isError && s.result === result) n++;
    else if (s.changes) break;
  }
  return n;
}

/**
 * Catches the loops a text-repetition check cannot see: the same tool calls with the same results
 * over and over (one call, or a short cycle like read A → read B → read A …), the same error again
 * and again, and the same call twice in one response. The model is told on the `noteAt`-th
 * repeat; the `blockAt`-th is not run. Calls rejected before running (invalid arguments, denied)
 * count too. Ending a run that keeps getting refused is the core's job (no_progress).
 *
 * settings: { noteAt?: number (default 3), blockAt?: number (default 5) }
 */
const loopGuard: Plugin = (api) => {
  const noteAt = typeof api.settings.noteAt === "number" ? api.settings.noteAt : 3;
  const blockAt = typeof api.settings.blockAt === "number" ? api.settings.blockAt : 5;
  let history: Step[] = [];
  /** Calls seen in the current response. */
  let thisTurn = new Set<string>();
  /** Calls this plugin refused: their results say so and must not break a detected cycle. */
  const refused = new Set<string>();
  api.on("user_prompt", () => {
    history = [];
    thisTurn = new Set();
  });
  // Fires for every response (subagents' too, unlike turn_end), before its calls are checked.
  api.on("assistant_message", () => {
    thisTurn = new Set();
  });

  api.on("tool_call", ({ call, args }) => {
    const sig = `${call.name}:${stable(args)}`;
    const deny = (reason: string) => {
      refused.add(call.id);
      return { decision: "deny" as const, reason: `loop-guard: ${reason}` };
    };
    if (thisTurn.has(sig)) return deny(`the same ${call.name} call appears earlier in this response; it is not run twice`);
    thisTurn.add(sig);
    for (let p = 1; p <= Math.min(MAX_PERIOD, history.length); p++) {
      if (history[history.length - p]!.call !== sig) continue;
      const n = repeats(history, p);
      if (n + 1 >= blockAt)
        return deny(
          p === 1
            ? `the same ${call.name} call returned the same result ${n} times in a row; change the approach`
            : `the same ${p} calls have repeated ${n} times with the same results; change the approach`,
        );
    }
  });

  api.on("tool_result", ({ call, result }) => {
    if (refused.delete(call.id)) return;
    const tool = api.agent.tools().find((t) => t.name === call.name);
    const step: Step = {
      call: `${call.name}:${stable(call.input)}`,
      result: resultText(result),
      isError: !!result.isError,
      changes: tool?.kind === "edit" && !result.isError,
    };
    const sameError = step.isError ? errorStreak(history, step.result) + 1 : 0;
    history.push(step);
    if (history.length > 50) history = history.slice(-50);
    let cycle = 0;
    let period = 1;
    for (let p = 1; p <= MAX_PERIOD && !cycle; p++) {
      const n = repeats(history, p);
      if (n >= noteAt) [cycle, period] = [n, p];
    }
    let note: string | undefined;
    if (cycle)
      note =
        period === 1
          ? `[harness] This is call ${cycle} of ${call.name} with the same arguments and the same result, with no file changed in between. Repeating it will not help; try a different approach.`
          : `[harness] The same ${period} tool calls have now repeated ${cycle} times with the same results, with no file changed in between. Repeating them will not help; try a different approach.`;
    else if (sameError >= noteAt)
      note = `[harness] The same error has now occurred ${sameError} times in a row. Try a different approach instead of repeating it.`;
    if (note) return { result: { ...result, content: [...result.content, { type: "text", text: note }] } };
  });
};
export default loopGuard;
