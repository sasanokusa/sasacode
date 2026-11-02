import { errorResult, type Plugin } from "@sasacode/plugin-api";

const DEFAULT_MAX = 50_000;
/** Read at most this much of a response: a huge page must not fill memory. */
const MAX_BYTES = 10_000_000;

/** Read at most `limit` bytes of a response, saying whether the document went on past them. */
export async function readCapped(res: Response, limit: number): Promise<{ text: string; cut: boolean }> {
  if (!res.body) return { text: "", cut: false };
  const reader = res.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  while (size < limit) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    size += value.length;
  }
  // Two ways the limit can be reached, and only one of them is a cut: a chunk that overran it took
  // the rest of the document with it, while a document that ends exactly at the limit needs one
  // more read to tell "it ended here" from "there is more" (the note in the tool would otherwise
  // claim a 10 MB page was cut when it was read whole).
  const cut = size > limit || (size === limit && !(await reader.read().then((r) => r.done, () => false)));
  await reader.cancel().catch(() => {});
  return { text: new TextDecoder().decode(Buffer.concat(chunks).subarray(0, limit)), cut };
}

const USER_AGENT = "sasacode (+https://github.com/sasanokusa/sasacode)";

const ENTITIES: Record<string, string> = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " " };

/** "#65" or "#x41" (or "#X41") as its character; one past the last code point (or not a number) is left as written. */
function codePoint(ref: string): string | undefined {
  const hex = ref[1] === "x" || ref[1] === "X";
  const n = Number.parseInt(ref.slice(hex ? 2 : 1), hex ? 16 : 10);
  return Number.isInteger(n) && n >= 0 && n <= 0x10ffff ? String.fromCodePoint(n) : undefined;
}

/** A link's target as an absolute URL; undefined for one that goes nowhere else (same page, script, mail). */
function linkTarget(href: string, base: string | undefined): string | undefined {
  const raw = href.trim().replace(/&amp;/gi, "&");
  if (!raw || raw.startsWith("#") || /^(javascript|mailto|tel|data):/i.test(raw)) return;
  let url: URL;
  try {
    url = new URL(raw, base);
  } catch {
    return;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return;
  if (base && url.href.split("#")[0] === base.split("#")[0]) return;
  return url.href;
}

/**
 * HTML as plain text. Links become markdown references ("[Docs][3]") into `links`, absolute URLs
 * resolved against `base` (or the page's own <base href>), so the agent can follow a link instead
 * of putting a URL together from a relative path or from memory.
 */
export function htmlToText(html: string, base?: string): { text: string; links: string[] } {
  const declared = /<base\s[^>]*href\s*=\s*["']([^"']+)["']/i.exec(html)?.[1];
  if (declared) base = linkTarget(declared, base) ?? base;
  const links: string[] = [];
  const numbers = new Map<string, number>();
  const text = html
    .replace(/<(script|style|noscript|svg|head)[\s\S]*?<\/\1>/gi, "")
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/<a\s([^>]*)>([\s\S]*?)<\/a>/gi, (_, attrs: string, inner: string) => {
      const href = /\bhref\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/i.exec(attrs);
      const target = href && linkTarget(href[1] ?? href[2] ?? href[3] ?? "", base);
      const label = inner.replace(/<[^>]+>/g, "").replace(/\s+/g, " ").trim();
      if (!target || !label) return inner;
      let n = numbers.get(target);
      if (n === undefined) numbers.set(target, (n = links.push(target)));
      return `[${label}][${n}]`;
    })
    .replace(/<(br|\/p|\/div|\/li|\/h[1-6]|\/tr|\/section|\/article)[^>]*>/gi, "\n")
    .replace(/<li[^>]*>/gi, "- ")
    .replace(/<[^>]+>/g, "")
    .replace(/&(#x?[0-9a-f]+|\w+);/gi, (m, e: string) => (e.startsWith("#") ? codePoint(e) : ENTITIES[e]) ?? m)
    .replace(/[ \t]+/g, " ")
    .replace(/\n\s*\n\s*\n+/g, "\n\n")
    .trim();
  return { text, links };
}

/** Links listed under one part of a page at most (a site's navigation can have hundreds). */
const MAX_LINKS = 200;

/** The link list for the references that appear in `text` ("[3]: https://…"), in order. */
function linkList(text: string, links: string[]): string | undefined {
  const shown = new Set<number>();
  for (const m of text.matchAll(/\]\[(\d+)\]/g)) if (Number(m[1]) <= links.length) shown.add(Number(m[1]));
  if (!shown.size) return;
  const listed = [...shown].sort((a, b) => a - b);
  const more = listed.length - MAX_LINKS;
  return [...listed.slice(0, MAX_LINKS).map((n) => `[${n}]: ${links[n - 1]}`), ...(more > 0 ? [`(${more} more links not listed)`] : [])].join("\n");
}

/** Whether `url` answers with a document (a quick look, the body is not read). */
async function exists(url: string, signal: AbortSignal): Promise<boolean> {
  try {
    const res = await fetch(url, { signal: AbortSignal.any([signal, AbortSignal.timeout(5_000)]), headers: { "user-agent": USER_AGENT } });
    await res.body?.cancel().catch(() => {});
    return res.ok && !/html/.test(res.headers.get("content-type") ?? "");
  } catch {
    return false;
  }
}

/** web_fetch: read a URL as text. Network access, so it goes through approval like bash. */
const webFetch: Plugin = (api) => {
  api.registerTool({
    name: "web_fetch",
    description: `Fetch a URL over HTTP(S) and return it as text (HTML is converted to plain text, its links listed as absolute URLs to follow). Returns up to max_chars characters (default ${DEFAULT_MAX}) starting at offset.`,
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
      // A URL the agent made up is the usual reason a fetch fails: point it at a way to find the right one.
      const search = api.agent.tools().find((t) => ["web_search", "search", "web"].includes(t.name))?.name;
      const instead = search ? `search with ${search}` : "start from a page you have fetched and follow its links";
      let res: Response;
      try {
        res = await fetch(url, {
          signal: AbortSignal.any([ctx.signal, AbortSignal.timeout(30_000)]),
          headers: { "user-agent": USER_AGENT, accept: "text/html,text/plain,application/json,*/*" },
        });
      } catch (e) {
        return errorResult(`Fetch failed: ${e instanceof Error ? e.message : String(e)}${ctx.signal.aborted ? "" : `\n[Do not guess another URL: ${instead}.]`}`);
      }
      const type = res.headers.get("content-type") ?? "";
      if (!/text|json|xml|javascript/.test(type) && type) return errorResult(`${res.status} ${type}: not a text document.`);
      const { text: raw, cut } = await readCapped(res, MAX_BYTES);
      const { text: body, links } = /html/.test(type) ? htmlToText(raw, res.url) : { text: raw, links: [] };
      const start = args.offset ?? 0;
      const max = args.max_chars ?? DEFAULT_MAX;
      const slice = body.slice(start, start + max);
      const parts = [`[${res.status} ${res.url}${cut ? ` — only the first ${MAX_BYTES / 1_000_000} MB was read` : ""}]`, slice];
      const list = linkList(slice, links);
      if (list) parts.push(`Links:\n${list}`);
      if (start + max < body.length)
        parts.push(`[Showing characters ${start}-${start + slice.length} of ${body.length}. Use offset=${start + slice.length} to continue.]`);
      if (res.status === 404 || res.status === 410) {
        const indexes = ["/llms.txt", "/sitemap.xml"].map((p) => new URL(p, res.url).href).filter((u) => u !== res.url);
        const found = (await Promise.all(indexes.map(async (u) => ((await exists(u, ctx.signal)) ? u : undefined)))).filter(Boolean);
        parts.push(
          `[This page does not exist. Do not guess another URL: ${found.length ? `fetch ${found.join(" or ")} (the site's own index) or ` : ""}${instead}.]`,
        );
      }
      return { content: [{ type: "text", text: parts.join("\n") }], isError: !res.ok };
    },
  });
};
export default webFetch;
