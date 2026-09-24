import { errorResult, type Plugin } from "@sasacode/plugin-api";

const DEFAULT_MAX = 50_000;

const ENTITIES: Record<string, string> = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " " };

export function htmlToText(html: string): string {
  return html
    .replace(/<(script|style|noscript|svg|head)[\s\S]*?<\/\1>/gi, "")
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/<a\s[^>]*href="([^"]*)"[^>]*>([\s\S]*?)<\/a>/gi, (_, href, t) => `${t} (${href})`)
    .replace(/<(br|\/p|\/div|\/li|\/h[1-6]|\/tr|\/section|\/article)[^>]*>/gi, "\n")
    .replace(/<li[^>]*>/gi, "- ")
    .replace(/<[^>]+>/g, "")
    .replace(/&(#x?[0-9a-f]+|\w+);/gi, (m, e: string) =>
      e.startsWith("#") ? String.fromCodePoint(Number.parseInt(e.slice(e[1] === "x" ? 2 : 1), e[1] === "x" ? 16 : 10)) : (ENTITIES[e] ?? m),
    )
    .replace(/[ \t]+/g, " ")
    .replace(/\n\s*\n\s*\n+/g, "\n\n")
    .trim();
}

/** web_fetch: read a URL as text. Network access, so it goes through approval like bash. */
const webFetch: Plugin = (api) => {
  api.registerTool({
    name: "web_fetch",
    description: `Fetch a URL over HTTP(S) and return it as text (HTML is converted to plain text). Returns up to max_chars characters (default ${DEFAULT_MAX}) starting at offset.`,
    parameters: {
      type: "object",
      properties: {
        url: { type: "string", description: "http(s) URL" },
        offset: { type: "integer", minimum: 0, description: "Character offset to start from (for long pages)" },
        max_chars: { type: "integer", minimum: 1 },
      },
      required: ["url"],
      additionalProperties: false,
    },
    kind: "other",
    concurrent: true,
    matchTarget: (a) => a.url,
    summary: (a) => a.url,
    async execute(args: { url: string; offset?: number; max_chars?: number }, ctx) {
      let url: URL;
      try {
        url = new URL(args.url);
      } catch {
        return errorResult(`Invalid URL: ${args.url}`);
      }
      if (url.protocol !== "http:" && url.protocol !== "https:") return errorResult("Only http and https URLs are supported.");
      let res: Response;
      try {
        res = await fetch(url, {
          signal: AbortSignal.any([ctx.signal, AbortSignal.timeout(30_000)]),
          headers: { "user-agent": "sasacode (+https://github.com/sasanokusa/sasacode)", accept: "text/html,text/plain,application/json,*/*" },
        });
      } catch (e) {
        return errorResult(`Fetch failed: ${e instanceof Error ? e.message : String(e)}`);
      }
      const type = res.headers.get("content-type") ?? "";
      if (!/text|json|xml|javascript/.test(type) && type) return errorResult(`${res.status} ${type}: not a text document.`);
      const raw = await res.text();
      const body = /html/.test(type) ? htmlToText(raw) : raw;
      const start = args.offset ?? 0;
      const max = args.max_chars ?? DEFAULT_MAX;
      const slice = body.slice(start, start + max);
      const notes = [`[${res.status} ${res.url}]`];
      if (start + max < body.length)
        notes.push(`[Showing characters ${start}-${start + slice.length} of ${body.length}. Use offset=${start + slice.length} to continue.]`);
      return { content: [{ type: "text", text: `${notes[0]}\n${slice}${notes[1] ? `\n${notes[1]}` : ""}` }], isError: !res.ok };
    },
  });
};
export default webFetch;
