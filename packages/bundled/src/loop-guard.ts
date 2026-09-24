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
 * Catches the loops a text-repetition check cannot see, and ends runs that stopped making progress:
 * - the same tool calls with the same results over and over (one call, or a short cycle like
 *   read A → read B → read A …): the model is told on the `noteAt`-th repeat; from the
 *   `blockAt`-th, every call of that loop is refused until a file changes or the user writes again;
 * - the same error again and again (calls rejected before running count too);
 * - the same call twice in one response with nothing in between that could change its result;
 * - `noProgressTurns` turns in a row in which no tool call ran: the run is stopped (turn_end stop).
 *   The core leaves such decisions to plugins (principle A6).
 *
 * settings: { noteAt?: number (default 3), blockAt?: number (default 5), noProgressTurns?: number (default 5, 0 = off) }
 */
const loopGuard: Plugin = (api) => {
  const num = (k: string, d: number) => (typeof api.settings[k] === "number" ? (api.settings[k] as number) : d);
  const noteAt = num("noteAt", 3);
  const blockAt = num("blockAt", 5);
  const noProgressTurns = num("noProgressTurns", 5);
  let history: Step[] = [];
  /** Calls in the current response since the last one that could change state. */
  let thisTurn = new Set<string>();
  /** The calls of a loop that was stopped: refused until something changes. */
  let locked = new Set<string>();
  /** Calls this plugin refused: their results say so and must not break a detected cycle. */
  const refused = new Set<string>();
  /** For the no-progress check: calls answered this turn, how many of them ran, turns without any. */
  let calls = 0;
  let ran = 0;
  let idle = 0;

  api.on("user_prompt", () => {
    history = [];
    thisTurn = new Set();
    locked = new Set();
    idle = 0;
  });
  // Fires for every response (subagents' too, unlike turn_end), before its calls are checked.
  api.on("assistant_message", () => {
    thisTurn = new Set();
  });

  api.on("tool_call", ({ call, tool, args }) => {
    const sig = `${call.name}:${stable(args)}`;
    const deny = (reason: string) => {
      refused.add(call.id);
      return { decision: "deny" as const, reason: `loop-guard: ${reason}` };
    };
    if (locked.has(sig)) return deny(`this ${call.name} call belongs to a loop that was stopped, and no file has changed since; change the approach`);
    if (thisTurn.has(sig)) return deny(`the same ${call.name} call appears earlier in this response with nothing in between that could change its result; it is not run twice`);
    // After a call that may change things, repeating an earlier check is legitimate (read → write → read).
    if (tool.kind !== "read") thisTurn = new Set();
    thisTurn.add(sig);
    for (let p = 1; p <= Math.min(MAX_PERIOD, history.length); p++) {
      if (history[history.length - p]!.call !== sig) continue;
      const n = repeats(history, p);
      if (n + 1 < blockAt) continue;
      locked = new Set(history.slice(-p).map((s) => s.call));
      return deny(
        p === 1
          ? `the same ${call.name} call returned the same result ${n} times in a row; change the approach`
          : `the same ${p} calls have repeated ${n} times with the same results; change the approach`,
      );
    }
  });

  api.on("tool_result", ({ call, result, ran: didRun }) => {
    calls++;
    if (didRun) ran++;
    if (refused.delete(call.id)) return;
    const tool = api.agent.tools().find((t) => t.name === call.name);
    const step: Step = {
      call: `${call.name}:${stable(call.input)}`,
      result: resultText(result),
      isError: !!result.isError,
      changes: tool?.kind === "edit" && !result.isError,
    };
    if (step.changes) locked = new Set();
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

  api.on("turn_end", () => {
    if (calls) idle = ran ? 0 : idle + 1;
    calls = ran = 0;
    if (noProgressTurns && idle >= noProgressTurns) return { stop: `${idle} turns in a row in which no tool call could run` };
  });
};
export default loopGuard;
