import type { Message, Plugin, PluginAPI } from "@sasacode/plugin-api";

const SYSTEM =
  "You write handoff summaries of coding sessions so the work can continue in a fresh context. Include: the user's goals and constraints, decisions made, files read or changed (paths), commands run and their outcomes, the current state, and the next steps. Be specific and concise.";

function estimate(m: Message): number {
  return Math.ceil(JSON.stringify(m.content).length / 4);
}

/** Plain-text transcript, so tool call/result pairing does not matter to the summarizer. */
export function transcript(messages: readonly Message[]): string {
  const clip = (s: string, n: number) => (s.length > n ? `${s.slice(0, n)}… [${s.length - n} more chars]` : s);
  return messages
    .map((m) => {
      if (m.role === "user") return `USER: ${m.content.map((c) => (c.type === "text" ? c.text : "[image]")).join("\n")}`;
      if (m.role === "tool")
        return `TOOL RESULT (${m.toolName}${m.isError ? ", error" : ""}): ${clip(m.content.map((c) => (c.type === "text" ? c.text : "[image]")).join("\n"), 2000)}`;
      return `ASSISTANT: ${m.content
        .map((c) => (c.type === "text" ? c.text : c.type === "tool_call" ? `[call ${c.name} ${clip(JSON.stringify(c.input), 500)}]` : ""))
        .filter(Boolean)
        .join("\n")}`;
    })
    .join("\n\n");
}

/** Index where the kept tail starts: a user message, with the tail under `keepTokens`. */
export function splitPoint(messages: readonly Message[], keepTokens: number): number {
  let tokens = 0;
  let split = messages.length;
  for (let i = messages.length - 1; i >= 0; i--) {
    tokens += estimate(messages[i]!);
    if (tokens > keepTokens) break;
    if (messages[i]!.role === "user") split = i;
  }
  return split;
}

/** `manual` (/compact) summarizes everything; automatic compaction keeps a recent tail verbatim. */
export async function compact(api: PluginAPI, instructions = "", manual = false): Promise<boolean> {
  const messages = api.session.messages();
  const keep = Math.min(40_000, Math.floor(api.agent.model().contextWindow * 0.15));
  const split = manual ? messages.length : splitPoint(messages, keep);
  const head = messages.slice(0, split);
  if (head.length < 2) return false;
  const summary = await api.agent.complete({
    system: SYSTEM,
    maxTokens: 8192,
    messages: [
      {
        role: "user",
        timestamp: Date.now(),
        content: [{ type: "text", text: `${transcript(head)}\n\n---\nSummarize the session above.${instructions ? ` ${instructions}` : ""}` }],
      },
    ],
  });
  api.session.replaceMessages([
    { role: "user", timestamp: Date.now(), content: [{ type: "text", text: `[Summary of the earlier conversation, compacted to save context]\n\n${summary}` }] },
    ...messages.slice(split),
  ]);
  api.ui.notify(`コンテキストを圧縮しました（${head.length} 件のメッセージを要約）`);
  return true;
}

/** Summarizes older history when the context fills up (or at `threshold` of the window), and on /compact. */
const compaction: Plugin = (api) => {
  const threshold = Number(api.settings.threshold ?? 0.8);
  api.on("context_limit", async () => ({ retry: await compact(api) }));
  api.on("turn_end", async ({ message }) => {
    const u = message.usage;
    if (u.input + u.cacheRead + u.cacheWrite + u.output > api.agent.model().contextWindow * threshold) await compact(api);
  });
  api.registerCommand({
    name: "compact",
    description: "会話履歴を要約してコンテキストを空ける",
    argumentHint: "[要約への指示]",
    async run({ args }) {
      if (!(await compact(api, args, true))) api.ui.notify("圧縮するほどの履歴がありません");
    },
  });
};
export default compaction;
