import type { UserContent } from "@sasacode/ai";

/**
 * How a bundled plugin's note to the model starts: a user turn kept in the history (turn_end's
 * inject), marked as not the user's. The interface shows it as a dim line, not as a prompt.
 */
export const NOTE_PREFIX = "[Reminder from sasacode, not from the user] ";

export function harnessNote(text: string): string {
  return NOTE_PREFIX + text;
}

/** The note's text without the mark, when every block of a user message is such a note. */
export function noteText(content: readonly UserContent[]): string | undefined {
  if (!content.length || !content.every((b) => b.type === "text" && b.text.startsWith(NOTE_PREFIX))) return undefined;
  return content.map((b) => (b.type === "text" ? b.text.slice(NOTE_PREFIX.length) : "")).join(" ");
}
