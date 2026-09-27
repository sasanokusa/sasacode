import { createReadStream } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import { extname } from "node:path";
import { createInterface } from "node:readline";
import { errorResult, text, type ToolDefinition } from "@sasacode/plugin-api";
import { resolvePath } from "./util.ts";

const DEFAULT_LIMIT = 2000;
const MAX_LINE = 2000;
/** Larger files are read line by line up to what is shown, not loaded whole. */
const WHOLE_FILE_BYTES = 10_000_000;
/** The providers' limit for one image (Anthropic's is 5 MB); a larger one would fail every request after it. */
const MAX_IMAGE_BYTES = 5_000_000;
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
  alwaysLoad: true,
  concurrent: true,
  paths: (a, cwd) => [resolvePath(a.path, cwd)],
  summary: (a) => a.path + (a.offset ? `:${a.offset}` : ""),
  async execute(args, ctx) {
    const path = resolvePath(args.path, ctx.cwd);
    const st = await stat(path).catch(() => undefined);
    if (!st) return errorResult(`File not found: ${path}`);
    if (st.isDirectory()) return errorResult(`${path} is a directory. List it with bash (ls).`);
    // A FIFO or a device would block until someone writes to it, or never end.
    if (!st.isFile()) return errorResult(`${path} is not a regular file (a pipe, socket or device); use bash if you really need it.`);
    const imageType = IMAGE_TYPES[extname(path).toLowerCase()];
    if (imageType) {
      if (st.size > MAX_IMAGE_BYTES)
        return errorResult(`${path} is ${st.size} bytes, over the ${MAX_IMAGE_BYTES} bytes an image may have. Make a smaller copy with bash (e.g. sips -Z 1600 or magick -resize) and read that.`);
      const data = await readFile(path);
      return { content: [text(`Image ${path} (${data.length} bytes)`), { type: "image", mediaType: imageType, data: data.toString("base64") }] };
    }
    const head = await readHead(path, 8192);
    if (head.includes(0)) return errorResult(`${path} looks like a binary file (${st.size} bytes).`);
    const start = (args.offset ?? 1) - 1;
    const wanted = start + (args.limit ?? DEFAULT_LIMIT);
    const { lines, more } = st.size > WHOLE_FILE_BYTES ? await readLines(path, wanted, ctx.signal) : { lines: splitLines((await readFile(path)).toString("utf8")), more: false };
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
    if (more) notes.push(`[Showing lines ${start + 1}-${end} of a ${Math.round(st.size / 1_000_000)} MB file. Use offset=${end + 1} to continue.]`);
    else if (end < lines.length || start > 0)
      notes.push(`[Showing lines ${start + 1}-${end} of ${lines.length}.${end < lines.length ? ` Use offset=${end + 1} to continue.` : ""}]`);
    if (cut) notes.push(`[${cut} long line(s) truncated at ${MAX_LINE} chars; use bash (e.g. cut, sed) to see them in full.]`);
    if (!lines.length || (lines.length === 1 && lines[0] === "")) return { content: [text(`${path} is empty.`)] };
    return { content: [text([body, ...notes].join("\n"))] };
  },
};

function splitLines(text: string): string[] {
  const lines = text.split("\n");
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

async function readHead(path: string, bytes: number): Promise<Buffer> {
  const chunks: Buffer[] = [];
  for await (const c of createReadStream(path, { start: 0, end: bytes - 1 })) chunks.push(c as Buffer);
  return Buffer.concat(chunks);
}

/** The first `count` lines of a large file; `more` when the file goes on after them. */
async function readLines(path: string, count: number, signal: AbortSignal): Promise<{ lines: string[]; more: boolean }> {
  const stream = createReadStream(path, { encoding: "utf8", signal });
  const rl = createInterface({ input: stream, crlfDelay: Infinity });
  const lines: string[] = [];
  let more = false;
  try {
    for await (const line of rl) {
      if (lines.length >= count) {
        more = true;
        break;
      }
      lines.push(line);
    }
  } finally {
    rl.close();
    stream.destroy();
  }
  return { lines, more };
}
