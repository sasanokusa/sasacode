// agent-judge — the "agent" permission mode: a model decides whether a call the mode would ask the
// user about is safe to run unasked. The core asks for every call but reads inside the working
// directory; this permission hook judges the rest. Safe calls run, risky ones and a judge that
// fails still go to the user, as does a call text-tool-calls picked up from the reply that is not a
// read. Rules the user wrote are never loosened (the hook only gets `lowest`).
// The judging model is `judgeModel` in config, else the current one.
import type { HookMap, Plugin } from "@sasacode/plugin-api";
import { asksFirst } from "./text-tool-calls.ts";

const QUESTION = (cwd: string, tool: string, args: unknown) => `A coding agent working in ${cwd} wants to run this tool call:
tool: ${tool}
arguments: ${JSON.stringify(args)}

Is it safe to run without asking the user? Safe means read-only or confined to the working directory, reversible, and not touching credentials, the network in a harmful way, or system settings.
Answer with one line of JSON: {"safe": true|false, "reason": "<short reason>"}`;

/** What the judge decided about one call: "allow" runs it unasked, "ask" leaves it to the user (risky, or the judge failed). */
export interface Judged {
  callId: string;
  tool: string;
  verdict: "allow" | "ask";
  reason: string;
}

const listeners = new Set<(judged: Judged) => void>();

/** Hear every verdict the judge gives in this process (`--control stdio` reports them as `judged` lines). Returns the way to stop. */
export function onJudged(listener: (judged: Judged) => void): () => void {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

const agentJudge: Plugin = (api) => {
  const model = typeof api.settings.model === "string" ? api.settings.model : undefined;
  type Decided = { decision: "allow" | "ask"; reason: string } | undefined;
  const judge = async ({ call, tool, args, cwd, mode, verdict, lowest }: HookMap["permission"]["event"]): Promise<Decided> => {
    // Only what the mode itself left to the user; never a rule's or another plugin's decision. A
    // call text-tool-calls picked up from the reply may be an example, which a judge cannot tell.
    if (mode !== "agent" || verdict.source !== "mode" || verdict.decision !== "ask" || lowest !== "allow" || asksFirst(call, tool)) return;
    try {
      const text = await api.agent.complete({
        model,
        system: "You review tool calls for safety.",
        maxTokens: 2048,
        messages: [{ role: "user", timestamp: Date.now(), content: [{ type: "text", text: QUESTION(cwd, tool.name, args) }] }],
      });
      const m = /\{[\s\S]*\}/.exec(text);
      if (!m) throw new Error("judge gave no verdict");
      const v = JSON.parse(m[0]) as { safe?: unknown; reason?: unknown };
      const safe = v.safe === true;
      return { decision: safe ? "allow" : "ask", reason: `agent judged ${safe ? "safe" : "risky"}: ${String(v.reason ?? "")}` };
    } catch (e) {
      return { decision: "ask", reason: `judge failed: ${(e as Error).message}` };
    }
  };
  api.on("permission", async (e) => {
    const decided = await judge(e);
    if (decided) for (const l of listeners) l({ callId: e.call.id, tool: e.tool.name, verdict: decided.decision, reason: decided.reason });
    return decided;
  });
};
export default agentJudge;
