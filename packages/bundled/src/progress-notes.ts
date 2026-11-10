import { harnessNote } from "@sasacode/host";
import type { Plugin } from "@sasacode/plugin-api";

/**
 * The language of a prompt, named when its script tells it. Asked for "the user's language" in an
 * English system prompt, DeepSeek V4.1 Flash wrote its progress lines in English to a Japanese user,
 * and its final answer drifted into Chinese.
 */
export function promptLanguage(text: string): string | undefined {
  if (/[\u3040-\u30ff]/.test(text)) return "Japanese";
  if (/[\uac00-\ud7af]/.test(text)) return "Korean";
  if (/[\u4e00-\u9fff]/.test(text)) return "Chinese";
  return undefined;
}

const guidance = (language: string) =>
  [
    "Keep the user informed while you work. They see the text you write, not your reasoning and not every tool's output:",
    "- Before your first tool call, say in one sentence what you are going to do.",
    "- Then, beside your tool calls, write a short line (one or two sentences) when you find something that matters, finish a step, or change direction.",
    `- Keep these lines brief and write them, like your final answer, in ${language}. Do not describe every call.`,
  ].join("\n");

/**
 * The text a model writes beside its tool calls is all the user sees of its work, and most models
 * write none unless told (in the session logs, Qwen3.8 Flash Next wrote text with 3% of 740 tool-call
 * replies, DeepSeek V4.1 Flash with 15%). The system prompt asks for short progress lines, and after
 * `remindAfter` tool-call replies in a row without text a reminder follows the tool results. It stays in
 * the history: a note on one request only made the next request differ from it, so a local server read
 * everything after it again (and a chat template that keeps reasoning only after the last user turn,
 * as Qwen's do, changed the history before it too).
 *
 * settings: { remindAfter?: number (default 5, 0 = no reminder) }
 */
const progressNotes: Plugin = (api) => {
  const remindAfter = typeof api.settings.remindAfter === "number" ? api.settings.remindAfter : 5;
  /** Replies of the main agent in a row that called tools and said nothing. */
  let silent = 0;
  /** From the user's latest prompt that tells it; a short "ok" keeps the one before. */
  let language: string | undefined;
  const named = () => (language ? `${language}, the language the user writes in` : "the language the user writes in");
  // Subagents report to the model that started them, not to the user.
  api.on("system_prompt", ({ prompt }, ctx) => (ctx.agent ? undefined : { prompt: `${prompt}\n\n${guidance(named())}` }));
  api.on("user_prompt", ({ content }) => {
    silent = 0;
    const said = promptLanguage(content.map((b) => (b.type === "text" ? b.text : "")).join(""));
    // Kanji alone (続行, 了解) is Japanese as much as Chinese: it keeps the language already known, and is Japanese in a Japanese interface (#91).
    language = said === "Chinese" ? (language ?? (api.ui.lang === "ja" ? "Japanese" : said)) : (said ?? language);
  });
  api.on("turn_end", ({ message }, ctx) => {
    if (ctx.agent) return;
    const said = message.content.some((b) => b.type === "text" && b.text.trim());
    silent = said || !message.content.some((b) => b.type === "tool_call") ? 0 : silent + 1;
    if (!remindAfter || silent < remindAfter) return;
    const replies = silent;
    silent = 0;
    return {
      inject: harnessNote(
        `Your last ${replies} replies were tool calls with no text, so the user cannot tell what you are doing. Keep working, and with your next tool call write one or two sentences in ${language ?? "the user's language"}: what you have found so far and what you are doing next.`,
      ),
    };
  });
};
export default progressNotes;
