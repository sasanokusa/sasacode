import { readFile } from "node:fs/promises";
import type { Plugin } from "@sasacode/plugin-api";

/**
 * read's line-number prefix: the number padded with spaces, then a tab (gone on a blank line whose trailing tab was trimmed).
 * The padding is required: a TSV whose first column counts 1, 2, 3 … is not read's output.
 */
const PREFIX = /^ +(\d+)(?:\t| *(?=\r?$))/;

const rows = (s: string) => (s.endsWith("\n") ? s.slice(0, -1) : s).split("\n");

/** content without read's line numbers, when every line has one and they count up one by one; else undefined. */
export function stripLineNumbers(content: string): string | undefined {
  const lines = rows(content), first = Number(PREFIX.exec(lines[0]!)?.[1]);
  if (!lines.every((l, i) => Number(PREFIX.exec(l)?.[1]) === first + i)) return undefined;
  return lines.map((l) => l.replace(PREFIX, "")).join("\n") + (content.endsWith("\n") ? "\n" : "");
}

/** Lines as read and write count them: a final newline ends the last line, it does not start one. */
const count = (s: string) => (s ? rows(s).length : 0);

/**
 * Guards write against the damage small models did in the Gemma4 measurement (#68):
 * - content copied from read with its line numbers: removed when every line has one and they count up one by one (the model
 *   is told); refused when most lines have one but not like that (lines were dropped or added among them);
 * - an existing file of `minLines` lines or more replaced by less than half as many (a fragment meant as an edit): refused.
 * A refused write goes through when the model sends the same one again, so a file that really looks like that can be written.
 *
 * settings: { lineNumbers?: boolean, shrink?: boolean (both default true), minLines?: number (default 20) }
 */
const writeGuard: Plugin = (api) => {
  const on = (key: string) => api.settings[key] !== false;
  const minLines = typeof api.settings.minLines === "number" ? api.settings.minLines : 20;
  /** Per agent: the write refused last, which goes through if it comes again. */
  const refused = new Map<string, string>();
  api.on("user_prompt", () => refused.clear());

  // After tool-repair (load order): the name and arguments are the repaired ones.
  api.on("tool_call_raw", ({ name, input }) => {
    if (name !== "write" || !on("lineNumbers") || typeof input.content !== "string") return;
    const plain = stripLineNumbers(input.content);
    if (plain === undefined) return;
    const n = count(input.content);
    return { input: { ...input, content: plain }, note: `removed read's line numbers from content (${n} line${n === 1 ? "" : "s"}); write takes the file's text alone` };
  });

  api.on("tool_call", async ({ tool, args }, ctx) => {
    if (tool.name !== "write" || typeof args.content !== "string" || typeof args.path !== "string") return;
    const content = args.content, key = `${args.path}\0${content}`, agent = ctx.agent ?? "";
    if (refused.get(agent) === key) return void refused.delete(agent);
    const deny = (reason: string) => {
      refused.set(agent, key);
      return { decision: "deny" as const, reason: `write-guard: ${reason}; if you really mean it, send the same write again` };
    };
    const lines = rows(content), numbered = lines.filter((l) => /^ +\d+\t/.test(l)).length;
    if (on("lineNumbers") && numbered >= 2 && numbered * 2 > lines.length)
      return deny(`${numbered} of content's ${lines.length} lines start with a line number as read shows it, but not every line has one counting up by one, so they were not removed and nothing was written. Send the file's text without line numbers`);
    const path = tool.paths?.(args, api.cwd)[0];
    const before = on("shrink") && path ? await readFile(path, "utf8").catch(() => undefined) : undefined;
    if (before === undefined) return;
    const [was, now] = [count(before), count(content)];
    if (was >= minLines && now * 2 < was)
      return deny(`${args.path} has ${was} lines and this content has ${now}; write replaces the whole file, so the rest would be lost and nothing was written. To change part of the file, use edit`);
  });
};
export default writeGuard;
