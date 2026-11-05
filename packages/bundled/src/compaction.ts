import { contentChars, responseRoom } from "@sasacode/agent";
import { t } from "@sasacode/host";
import type { Message, ModelInfo, Plugin, PluginAPI } from "@sasacode/plugin-api";

const SYSTEM =
  "You write handoff summaries of coding sessions so the work can continue in a fresh context. Include: the user's goals and constraints, decisions made, files read or changed (paths), commands run and their outcomes, the current state, and the next steps. Be specific and concise.";

/** Characters outside ASCII: local tokenizers (Qwen, Gemma, Llama) make about a token of each, not a quarter of one. */
const wide = (s: string) => s.replace(/[\x00-\x7f]/g, "").length;

/**
 * Tokens of a message, weighed as summaryRequest weighs them: at 4 characters a token, the tail kept of a Japanese session was 3 times its budget.
 * The wide characters are looked for in what contentChars counts, without an image's base64: going through megabytes of it took 0.6 s.
 */
function estimate(m: Message): number {
  const text = JSON.stringify(m.content, (k, v) => (k === "data" && typeof v === "string" && v.length > 64 ? "" : v));
  return Math.ceil((contentChars(m) + 3 * wide(text)) / 4);
}

/** Plain-text transcript, so tool call/result pairing does not matter to the summarizer. Tool results and call arguments are clipped. */
export function transcript(messages: readonly Message[], resultChars = 2000, callChars = 500): string {
  const clip = (s: string, n: number) => (s.length > n ? `${s.slice(0, n)}… [${s.length - n} more chars]` : s);
  return messages
    .map((m) => {
      if (m.role === "user") return `USER: ${m.content.map((c) => (c.type === "text" ? c.text : "[image]")).join("\n")}`;
      if (m.role === "tool")
        return `TOOL RESULT (${m.toolName}${m.isError ? ", error" : ""}): ${clip(m.content.map((c) => (c.type === "text" ? c.text : "[image]")).join("\n"), resultChars)}`;
      return `ASSISTANT: ${m.content
        .map((c) => (c.type === "text" ? c.text : c.type === "tool_call" ? `[call ${c.name} ${clip(JSON.stringify(c.input), callChars)}]` : ""))
        .filter(Boolean)
        .join("\n")}`;
    })
    .join("\n\n");
}

const LEFT_OUT = "[The start of the session is left out here, so that this request fits the context window.]\n\n";

/**
 * The summary request, made to fit the window: servers such as vLLM and llama.cpp reject one whose
 * input plus max_tokens is over it, and a fixed 8,192-token summary broke compaction in the small
 * windows that need it most (#60). The summary gets about 15% of the window (as responseRoom gives
 * a reply), at most 8,192 tokens; the input gets the rest, estimated as the core sizes replies
 * (about 4 characters a token, plus 10%), except that a character outside ASCII counts as a token:
 * local tokenizers make about one of each Japanese character, and at a quarter of a token a Japanese
 * history went over the window. A transcript too long for that (/compact sends the whole history)
 * has its tool results and calls clipped harder first, which keeps every step; if that is not
 * enough, its oldest part is dropped, since the latest context matters most for carrying on.
 * Instructions that then take more than half the room throw, and the history stays as it was:
 * a summary of the session's last few lines would otherwise replace all of it.
 */
function summaryRequest(model: ModelInfo, head: readonly Message[], instructions: string): { maxTokens: number; message: Message } {
  const maxTokens = Math.min(8192, responseRoom(model));
  const maxChars = Math.floor(((model.contextWindow - maxTokens) / 1.1) * 4) - SYSTEM.length - 8;
  const ask = `\n\n---\nSummarize the session above.${instructions ? ` ${instructions}` : ""}`;
  const message = (text: string): Message => ({ role: "user", timestamp: Date.now(), content: [{ type: "text", text: text + ask }] });
  const size = (text: string) => contentChars(message(text)) + 3 * wide(text + ask);
  let text = transcript(head);
  if (size(text) > maxChars) text = transcript(head, 200, 100);
  if (size(text) > maxChars) {
    if (size(LEFT_OUT) > maxChars / 2) throw new Error("the /compact instructions leave too little room for the session in the context window; the history was left as it was");
    // The longest recent part that fits, found by halving, since what a character weighs varies (code, Japanese).
    const full = text;
    let keep = 0;
    for (let most = full.length; keep < most; ) {
      const mid = Math.ceil((keep + most) / 2);
      if (size(LEFT_OUT + full.slice(full.length - mid)) > maxChars) most = mid - 1;
      else keep = mid;
    }
    text = LEFT_OUT + full.slice(full.length - keep);
  }
  return { maxTokens, message: message(text) };
}

/**
 * Index where the kept tail starts, with the tail under `keepTokens`: a user message, or, when the
 * tail from one would keep less than half of that, the start of an assistant turn further back (its
 * tool calls and their results stay together, after it). A long run of tool calls after one prompt
 * has no user message within the budget: all of it was summarized, nothing of the latest steps was
 * kept as it was, and a small model then made up what those steps had done (the files it had written).
 */
export function splitPoint(messages: readonly Message[], keepTokens: number): number {
  let tokens = 0;
  let user = messages.length;
  let userTokens = 0;
  let turn = messages.length;
  for (let i = messages.length - 1; i >= 0; i--) {
    tokens += estimate(messages[i]!);
    if (tokens > keepTokens) break;
    if (messages[i]!.role === "user") {
      user = i;
      userTokens = tokens;
    } else if (messages[i]!.role === "assistant") turn = i;
  }
  return user < messages.length && userTokens >= keepTokens / 2 ? user : Math.min(user, turn);
}

/** `manual` (/compact) summarizes everything; automatic compaction keeps a recent tail verbatim. */
export async function compact(api: PluginAPI, instructions = "", manual = false): Promise<boolean> {
  const messages = api.session.messages();
  const model = api.agent.model();
  const keep = Math.min(40_000, Math.floor(model.contextWindow * 0.15));
  const split = manual ? messages.length : splitPoint(messages, keep);
  const head = messages.slice(0, split);
  if (head.length < 2) return false;
  // One long model call with nothing streaming to the screen: say that it is under way.
  api.ui.setStatus("compaction", t("履歴を要約中（{n} 件）…", { n: head.length }));
  let summary: string;
  try {
    const { maxTokens, message } = summaryRequest(model, head, instructions);
    summary = await api.agent.complete({ system: SYSTEM, maxTokens, messages: [message] });
  } finally {
    api.ui.setStatus("compaction", undefined);
  }
  // An empty answer (the output spent on reasoning, a cut-off stream) must not replace the history.
  if (!summary.trim()) throw new Error("the model returned an empty summary; the history was left as it was");
  api.session.replaceMessages([
    { role: "user", timestamp: Date.now(), content: [{ type: "text", text: `[Summary of the earlier conversation, compacted to save context]\n\n${summary}` }] },
    ...messages.slice(split),
  ]);
  api.ui.notify(t("コンテキストを圧縮しました（{n} 件のメッセージを要約）", { n: head.length }));
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
    description: t("会話履歴を要約してコンテキストを空ける"),
    argumentHint: t("[要約への指示]"),
    async run({ args }) {
      if (!(await compact(api, args, true))) api.ui.notify(t("圧縮するほどの履歴がありません"));
    },
  });
};
export default compaction;
