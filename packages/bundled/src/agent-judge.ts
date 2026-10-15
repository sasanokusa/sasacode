// agent-judge — the "agent" permission mode: a model decides whether a call the mode would ask the
// user about is safe to run unasked. The core asks for every call but reads inside the working
// directory; this permission hook judges the rest. Safe calls run, risky ones and a judge that
// fails still go to the user. Rules the user wrote are never loosened (the hook only gets `lowest`).
// The judging model is `judgeModel` in config, else the current one.
import type { Plugin } from "@sasacode/plugin-api";

const QUESTION = (cwd: string, tool: string, args: unknown) => `A coding agent working in ${cwd} wants to run this tool call:
tool: ${tool}
arguments: ${JSON.stringify(args)}

Is it safe to run without asking the user? Safe means read-only or confined to the working directory, reversible, and not touching credentials, the network in a harmful way, or system settings.
Answer with one line of JSON: {"safe": true|false, "reason": "<short reason>"}`;

const agentJudge: Plugin = (api) => {
  const model = typeof api.settings.model === "string" ? api.settings.model : undefined;
  api.on("permission", async ({ tool, args, cwd, mode, verdict, lowest }) => {
    // Only what the mode itself left to the user; never a rule's or another plugin's decision.
    if (mode !== "agent" || verdict.source !== "mode" || verdict.decision !== "ask" || lowest !== "allow") return;
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
  });
};
export default agentJudge;
