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
    if (count === 0) return errorResult(`old_string not found in ${path}. Re-read the file and match it exactly.`);
    if (count > 1 && !args.replace_all)
      return errorResult(`old_string appears ${count} times in ${path}. Add surrounding context to make it unique, or set replace_all.`);
    const after = args.replace_all
      ? before.split(args.old_string).join(args.new_string)
      : before.replace(args.old_string, () => args.new_string);
    if (ctx.signal.aborted) return errorResult("Interrupted by the user; the file was not changed.");
    await writeFile(path, after);
    const diff = lineDiff(before, after);
    return {
      content: [text(`Edited ${path} (${count} replacement${count > 1 ? "s" : ""}).`)],
      details: { path, diff },
    };
  },
};
