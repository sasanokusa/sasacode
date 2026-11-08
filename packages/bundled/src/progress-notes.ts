import { harnessNote } from "@sasacode/host";
import type { Plugin } from "@sasacode/plugin-api";

const GUIDANCE = [
  "Keep the user informed while you work. They see the text you write, not your reasoning and not every tool's output:",
  "- Before your first tool call, say in one sentence what you are going to do.",
  "- Then, beside your tool calls, write a short line (one or two sentences) when you find something that matters, finish a step, or change direction.",
  "- Keep these lines brief and in the user's language; do not describe every call.",
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
  // Subagents report to the model that started them, not to the user.
  api.on("system_prompt", ({ prompt }, ctx) => (ctx.agent ? undefined : { prompt: `${prompt}\n\n${GUIDANCE}` }));
  api.on("user_prompt", () => void (silent = 0));
  api.on("turn_end", ({ message }, ctx) => {
    if (ctx.agent) return;
    const said = message.content.some((b) => b.type === "text" && b.text.trim());
    silent = said || !message.content.some((b) => b.type === "tool_call") ? 0 : silent + 1;
    if (!remindAfter || silent < remindAfter) return;
    const replies = silent;
    silent = 0;
    return {
      inject: harnessNote(
        `Your last ${replies} replies were tool calls with no text, so the user cannot tell what you are doing. Keep working, and with your next tool call write one or two sentences: what you have found so far and what you are doing next.`,
      ),
    };
  });
};
export default progressNotes;
