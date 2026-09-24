import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import { text, type ToolDefinition } from "@sasacode/plugin-api";
import { lineDiff, resolvePath } from "./util.ts";

interface Args {
  path: string;
  content: string;
}

export const writeTool: ToolDefinition<Args> = {
  name: "write",
  description: "Create a file or overwrite it entirely. Parent directories are created as needed. Prefer edit for changes to existing files.",
  parameters: {
    type: "object",
    properties: {
      path: { type: "string", description: "File path, absolute or relative to the working directory" },
      content: { type: "string", description: "Full file contents" },
    },
    required: ["path", "content"],
    additionalProperties: false,
  },
  kind: "edit",
  paths: (a, cwd) => [resolvePath(a.path, cwd)],
  summary: (a) => a.path,
  async execute(args, ctx) {
    const path = resolvePath(args.path, ctx.cwd);
    const before = await readFile(path, "utf8").catch(() => undefined);
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, args.content);
    const lines = args.content.split("\n").length;
    return {
      content: [text(`${before === undefined ? "Created" : "Overwrote"} ${path} (${lines} lines).`)],
      details: { path, created: before === undefined, diff: before === undefined ? undefined : lineDiff(before, args.content) },
    };
  },
};
