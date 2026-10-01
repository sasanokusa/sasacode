import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import { errorResult, text, type ToolDefinition } from "@sasacode/plugin-api";
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
  alwaysLoad: true,
  paths: (a, cwd) => [resolvePath(a.path, cwd)],
  summary: (a) => a.path,
  async execute(args, ctx) {
    // Checked again after every wait: an interrupt must stop changes that have not started yet.
    const interrupted = () => errorResult("Interrupted by the user; nothing was written.");
    if (ctx.signal.aborted) return interrupted();
    const path = resolvePath(args.path, ctx.cwd);
    const before = await readFile(path, "utf8").catch(() => undefined);
    if (ctx.signal.aborted) return interrupted();
    if (before === args.content) return { content: [text(`Unchanged ${path}; nothing was written.`)], details: { path, changed: false } };
    await mkdir(dirname(path), { recursive: true });
    if (ctx.signal.aborted) return interrupted();
    await writeFile(path, args.content);
    const lines = args.content.split("\n").length;
    return {
      content: [text(`${before === undefined ? "Created" : "Overwrote"} ${path} (${lines} lines).`)],
      details: { path, changed: true, created: before === undefined, diff: before === undefined ? undefined : lineDiff(before, args.content) },
    };
  },
};
