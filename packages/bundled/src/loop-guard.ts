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

/** Consecutive steps at the end of `history` (since the last file change) equal to `pred`. */
function streak(history: Step[], pred: (s: Step) => boolean): number {
  let n = 0;
  for (let i = history.length - 1; i >= 0; i--) {
    const s = history[i]!;
    if (pred(s)) n++;
    else if (s.changes) break;
  }
  return n;
}

/**
 * Catches the loops a text-repetition check cannot see: the same tool called with the same
 * arguments and getting the same result, or the same error again and again, with no file
 * changed in between. The model is told on the `noteAt`-th repeat; the `blockAt`-th is not run.
 *
 * settings: { noteAt?: number (default 3), blockAt?: number (default 5) }
 */
const loopGuard: Plugin = (api) => {
  const noteAt = typeof api.settings.noteAt === "number" ? api.settings.noteAt : 3;
  const blockAt = typeof api.settings.blockAt === "number" ? api.settings.blockAt : 5;
  let history: Step[] = [];
  api.on("user_prompt", () => {
    history = [];
  });

  api.on("tool_call", ({ call, args }) => {
    const sig = `${call.name}:${stable(args)}`;
    const last = history.at(-1);
    if (!last || last.call !== sig) return;
    const same = streak(history, (s) => s.call === sig && s.result === last.result);
    if (same + 1 >= blockAt)
      return {
        decision: "deny",
        reason: `loop-guard: the same ${call.name} call returned the same result ${same} times in a row; change the approach`,
      };
  });

  api.on("tool_result", ({ call, result }) => {
    const tool = api.agent.tools().find((t) => t.name === call.name);
    const step: Step = {
      call: `${call.name}:${stable(call.input)}`,
      result: resultText(result),
      isError: !!result.isError,
      changes: tool?.kind === "edit" && !result.isError,
    };
    const sameCall = streak(history, (s) => s.call === step.call && s.result === step.result) + 1;
    const sameError = step.isError ? streak(history, (s) => s.isError && s.result === step.result) + 1 : 0;
    history.push(step);
    if (history.length > 50) history = history.slice(-50);
    let note: string | undefined;
    if (sameCall >= noteAt)
      note = `[harness] This is call ${sameCall} of ${call.name} with the same arguments and the same result, with no file changed in between. Repeating it will not help; try a different approach.`;
    else if (sameError >= noteAt)
      note = `[harness] The same error has now occurred ${sameError} times in a row. Try a different approach instead of repeating it.`;
    if (note) return { result: { ...result, content: [...result.content, { type: "text", text: note }] } };
  });
};
export default loopGuard;
