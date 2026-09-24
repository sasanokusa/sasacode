import { readFile, stat } from "node:fs/promises";
import { extname } from "node:path";
import { errorResult, text, type ToolDefinition } from "@sasacode/plugin-api";
import { resolvePath } from "./util.ts";

const DEFAULT_LIMIT = 2000;
const MAX_LINE = 2000;
const IMAGE_TYPES: Record<string, string> = {
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".gif": "image/gif",
  ".webp": "image/webp",
};

interface Args {
  path: string;
  offset?: number;
  limit?: number;
}

export const readTool: ToolDefinition<Args> = {
  name: "read",
  description: `Read a file. Text is returned with line numbers (cat -n style), up to ${DEFAULT_LIMIT} lines per call; use offset/limit for more. Images (png, jpg, gif, webp) are returned as images.`,
  parameters: {
    type: "object",
    properties: {
      path: { type: "string", description: "File path, absolute or relative to the working directory" },
      offset: { type: "integer", minimum: 1, description: "1-based line to start from" },
      limit: { type: "integer", minimum: 1, description: "Maximum number of lines" },
    },
    required: ["path"],
    additionalProperties: false,
  },
  kind: "read",
  concurrent: true,
  paths: (a, cwd) => [resolvePath(a.path, cwd)],
  summary: (a) => a.path + (a.offset ? `:${a.offset}` : ""),
  async execute(args, ctx) {
    const path = resolvePath(args.path, ctx.cwd);
    const st = await stat(path).catch(() => undefined);
    if (!st) return errorResult(`File not found: ${path}`);
    if (st.isDirectory()) return errorResult(`${path} is a directory. List it with bash (ls).`);
    const imageType = IMAGE_TYPES[extname(path).toLowerCase()];
    if (imageType) {
      const data = await readFile(path);
      return { content: [text(`Image ${path} (${data.length} bytes)`), { type: "image", mediaType: imageType, data: data.toString("base64") }] };
    }
    const buf = await readFile(path);
    if (buf.subarray(0, 8192).includes(0)) return errorResult(`${path} looks like a binary file (${st.size} bytes).`);
    const lines = buf.toString("utf8").split("\n");
    if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
    const start = (args.offset ?? 1) - 1;
    if (start >= lines.length && lines.length > 0)
      return errorResult(`offset ${args.offset} is past the end of the file (${lines.length} lines).`);
    const end = Math.min(lines.length, start + (args.limit ?? DEFAULT_LIMIT));
    let cut = 0;
    const body = lines
      .slice(start, end)
      .map((l, i) => {
        if (l.length > MAX_LINE) {
          cut++;
          l = `${l.slice(0, MAX_LINE)}… [line truncated, ${l.length} chars]`;
        }
        return `${String(start + i + 1).padStart(6)}\t${l}`;
      })
      .join("\n");
    const notes: string[] = [];
    if (end < lines.length || start > 0)
      notes.push(`[Showing lines ${start + 1}-${end} of ${lines.length}.${end < lines.length ? ` Use offset=${end + 1} to continue.` : ""}]`);
    if (cut) notes.push(`[${cut} long line(s) truncated at ${MAX_LINE} chars; use bash (e.g. cut, sed) to see them in full.]`);
    if (!lines.length || (lines.length === 1 && lines[0] === "")) return { content: [text(`${path} is empty.`)] };
    return { content: [text([body, ...notes].join("\n"))] };
  },
};
