import type { Message } from "@sasacode/plugin-api";

/**
 * A note for the model on this one request, never stored: a user turn after the history. Put in a
 * tool's output, a reminder was read as part of that output and passed over (17 todo reminders, one
 * update). Notes from several plugins share the one message.
 */
export function withReminder(messages: Message[], text: string): Message[] {
  const block = { type: "text" as const, text: `[Reminder from sasacode, not from the user] ${text}` };
  const last = messages.at(-1);
  if (last?.role === "user") return [...messages.slice(0, -1), { ...last, content: [...last.content, block] }];
  return [...messages, { role: "user", content: [block], timestamp: Date.now() }];
}
