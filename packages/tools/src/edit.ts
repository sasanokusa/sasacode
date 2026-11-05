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
    const before = await readFile(path, "utf8").catch(() => undefined);
    if (before === undefined) return errorResult(`File not found: ${path}. Use write to create it.`);
    if (args.old_string === args.new_string) return errorResult("old_string and new_string are identical.");
    if (!args.old_string) return errorResult("old_string is empty. Use write to replace the whole file.");
    const count = before.split(args.old_string).length - 1;
    if (count > 1 && !args.replace_all)
      return errorResult(`old_string appears ${count} times in ${path}. Add surrounding context to make it unique, or set replace_all.`);
    const loose = count ? undefined : loosely(path, before, args.old_string, args.new_string);
    if (typeof loose === "string") return errorResult(loose);
    const after =
      loose?.after ??
      (args.replace_all ? before.split(args.old_string).join(args.new_string) : before.replace(args.old_string, () => args.new_string));
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
 * old_string is not in the file as given (#65): look again without read's line numbers (if every line has one), then also
 * ignoring each line's trailing whitespace and CR, then its indentation, for a single place only, replace_all or not.
 * new_string takes the file's line endings, and its lines starting with old_string's first indentation take the place's.
 */
function loosely(path: string, file: string, old: string, neu: string): string | { after: string; note: string } {
  const numbered = /^ *\d+\t/m.test(old) && old.split("\n").every((l) => !l.trim() || /^ *\d+\t/.test(l));
  if (numbered) [old, neu] = [old.replace(/^ *\d+\t/gm, ""), neu.replace(/^ *\d+\t/gm, "")];
  let i = 0, hits = file.matchAll(new RegExp(RegExp.escape(old), "g")).take(2).toArray();
  for (; !hits.length && i < 2; i++) {
    const lines = old.split("\n").map((l) => l.replace(i ? /^[ \t]+|[ \t\r]+$/g : /[ \t\r]+$/, ""));
    hits = file.matchAll(new RegExp(lines.map((l) => (i && l ? "^[ \\t]*" : "") + RegExp.escape(l)).join("[ \\t]*\\r?\\n"), "gm")).take(2).toArray();
  }
  const ignoring = `ignoring ${[numbered && "line-number prefixes", ...["trailing whitespace and line endings", "indentation"].slice(0, i)].filter(Boolean).join(", ")}`;
  if (!hits.length) return `old_string not found in ${path}, even ${ignoring}. Re-read the file and match it exactly.`;
  if (hits.length > 1) return `old_string appears more than once in ${path}, ${ignoring}. Add surrounding context to make it unique.`;
  const [m] = hits, [from, to] = [old, m[0]].map((s) => /^[ \t]*(?=\S)/m.exec(s)?.[0] ?? "");
  neu = neu.replace(/^[ \t]*(?=\S)/gm, (w) => (w.startsWith(from) ? to + w.slice(from.length) : w)).replace(/\r?\n/g, file.includes("\r\n") ? "\r\n" : "\n");
  return { after: file.slice(0, m.index) + neu + file.slice(m.index + m[0].length), note: `[harness matched old_string ${ignoring}]\n` };
}
