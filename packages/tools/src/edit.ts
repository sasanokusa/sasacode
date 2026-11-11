import { isUtf8 } from "node:buffer";
import { readFile, writeFile } from "node:fs/promises";
import { errorResult, text, type ToolDefinition } from "@sasacode/plugin-api";
import { lineDiff, resolvePath } from "./util.ts";

interface Args {
  path: string;
  old_string: string;
  new_string: string;
  replace_all?: boolean;
}

export const editTool: ToolDefinition<Args> = {
  name: "edit",
  description:
    "Replace an exact string in a file. old_string must match the file exactly (including whitespace) and be unique unless replace_all is true.",
  parameters: {
    type: "object",
    properties: {
      path: { type: "string", description: "File path, absolute or relative to the working directory" },
      old_string: { type: "string", description: "Exact text to replace" },
      new_string: { type: "string", description: "Replacement text" },
      replace_all: { type: "boolean", description: "Replace every occurrence (default false)" },
    },
    required: ["path", "old_string", "new_string"],
    additionalProperties: false,
  },
  kind: "edit",
  alwaysLoad: true,
  paths: (a, cwd) => [resolvePath(a.path, cwd)],
  summary: (a) => a.path,
  async execute(args, ctx) {
    const path = resolvePath(args.path, ctx.cwd);
    const bytes = await readFile(path).catch(() => undefined);
    if (bytes === undefined) return errorResult(`File not found: ${path}. Use write to create it.`);
    // As UTF-8, Shift_JIS or Latin-1 bytes become U+FFFD: written back, lines the edit never touched would be lost (#89).
    if (!isUtf8(bytes))
      return errorResult(`${path} is not valid UTF-8 (e.g. Shift_JIS, EUC-JP or Latin-1); edit would corrupt the bytes it cannot decode, so nothing was changed. Edit it with bash instead (e.g. iconv to UTF-8 and back, or sed on its own encoding).`);
    const before = bytes.toString("utf8");
    if (!args.old_string) return errorResult("old_string is empty. Use write to replace the whole file.");
    const count = before.split(args.old_string).length - 1;
    if (count > 1 && !args.replace_all)
      return errorResult(`old_string appears ${count} times in ${path}. Add surrounding context to make it unique, or set replace_all.`);
    const loose = count ? undefined : loosely(path, before, args.old_string, args.new_string);
    if (typeof loose === "string") return errorResult(loose);
    const after = loose?.after ?? (args.replace_all ? before.split(args.old_string).join(args.new_string) : before.replace(args.old_string, () => args.new_string));
    if (after === before) return errorResult("old_string and new_string are identical, as matched in the file; nothing was changed.");
    if (ctx.signal.aborted) return errorResult("Interrupted by the user; the file was not changed.");
    await writeFile(path, after);
    const diff = lineDiff(before, after);
    return {
      content: [text(`${loose?.note ?? ""}Edited ${path} (${count || 1} replacement${count > 1 ? "s" : ""}).`)],
      details: { path, changed: true, diff },
    };
  },
};

/**
 * old_string is not in the file as given (#65): look again without read's "N\t" line numbers (refused if a bare line of digits,
 * a number without its tab or text, is among them), then ignoring trailing whitespace and CR (a trailing CR or indentation-only last
 * line is the next line's), then indentation; one place only, overlaps counted. Each indentation in new_string becomes the file's for
 * that old_string indentation, else the one shift all lines share if two depths of old_string or unmoved lines show it, else refused.
 */
function loosely(path: string, file: string, old: string, neu: string): string | { after: string; note: string } {
  // read pads its numbers to 6 columns: a TSV row's "123\t" is the file's text, not a line number (#84); nor is one past the file's end (#130).
  const LN = String.raw`^(?:(?=[ \d]{6}\t) *\d+|\d{7,})\t`, ln = new RegExp(LN, "gm");
  const numbered = new RegExp(LN, "m").test(old) && old.split("\n").every((l) => !l.trim() || new RegExp(`${LN}|^ *\\d+ *\\r?$`).test(l)) && old.match(ln)!.reduce((a, n) => Math.max(a, +n), 0) <= file.split("\n").length, bare = numbered ? old.replace(ln, "") : old, digits = numbered && /^ *\d+ *\r?$/m.exec(`${old}\n${neu}`);
  if (digits) return `${digits.index > old.length ? "new" : "old"}_string's line ${JSON.stringify(digits[0])} is only digits among read's line numbers: a blank line's number without its tab, or text, so the line-number form is ambiguous there. Nothing was changed; give old_string and new_string as the file's exact text, without line numbers.`;
  [old, neu] = [bare, numbered ? neu.replace(ln, "") : neu].map((s) => s.replace(/[ \t\r]+$/, ""));
  let i = 0, find = (re: string) => file.matchAll(new RegExp(`(?=(${re}))`, "gm")).take(2).toArray(), hits = find(RegExp.escape(old));
  for (; !hits.length && i < 2; i++) {
    const lines = old.split("\n").map((l) => l.replace(i ? /^[ \t]+|[ \t\r]+$/g : /[ \t\r]+$/, ""));
    // A blank first line may start only where a run of blanks does: scanning the run from each of its places is quadratic.
    hits = find((lines[0] ? "" : "(?<![ \\t])") + lines.map((l) => (i && l ? "^[ \\t]*" : "") + RegExp.escape(l)).join("[ \\t]*\\r?\\n"));
  }
  const ignoring = `ignoring ${[numbered && "line-number prefixes", ...["trailing whitespace and line endings", "indentation"].slice(0, i || +(old !== bare))].filter(Boolean).join(", ")}`;
  if (!hits.length) return `old_string not found in ${path}, even ${ignoring}. Re-read the file and match it exactly.`;
  if (hits.length > 1) return `old_string appears more than once in ${path}, ${ignoring}. Add surrounding context to make it unique.`;
  const [{ index, 1: at = "" }] = hits, ws = (l = "") => /^[ \t]*/.exec(l)![0], fl = at.split("\n");
  // Per non-blank line pair: [, a, tail, b] with old_string's indentation a + tail and the matched line's b + tail.
  const sh = old.split("\n").flatMap((l, k) => (l.trim() ? [/^(.*?)(.*)\n(.*?)\2$/.exec(`${ws(l)}\n${ws(fl[k])}`)!] : [])), deep = new Set(sh.map((s) => s[1] + s[2])).size > 1;
  const to = (w: string, own = sh.filter((s) => s[1] + s[2] === w)) => [...new Set((own.length ? own : sh).map((s) => (w.startsWith(s[1]) && (own.length || s[1] === s[3] || deep) ? s[3] + w.slice(s[1].length) : "\n")))];
  // Once per indentation, not per line: both strings may have thousands of lines.
  const map = Object.fromEntries([...new Set(neu.match(/^[ \t]*(?=\S)/gm))].map((w) => [w, to(w)]));
  if (Object.values(map).some((t) => t.length > 1 || t.includes("\n"))) return `old_string matched ${path} ${ignoring}, but new_string's indentation does not map onto the file's; use the file's exact indentation.`;
  neu = neu.replace(/^[ \t]*(?=\S)/gm, (w) => map[w][0] ?? w).replace(/\r?\n/g, file.includes("\r\n") ? "\r\n" : "\n");
  return { after: file.slice(0, index) + neu + file.slice(index + at.length), note: `[harness matched old_string ${ignoring}]\n` };
}
