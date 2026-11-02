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

export const USER_AGENT = "sasacode (+https://github.com/sasanokusa/sasacode)";

const ENTITIES: Record<string, string> = {
  amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " ", copy: "©", reg: "®", trade: "™", hellip: "…", middot: "·", bull: "•",
  mdash: "—", ndash: "–", lsquo: "‘", rsquo: "’", ldquo: "“", rdquo: "”", laquo: "«", raquo: "»", lsaquo: "‹", rsaquo: "›", times: "×",
  deg: "°", euro: "€", pound: "£", yen: "¥", szlig: "ß",
};
// Accented letters: &auml; &Eacute; &ccedil; …
for (const [mark, combining] of Object.entries({ uml: "\u0308", acute: "\u0301", grave: "\u0300", circ: "\u0302", tilde: "\u0303", ring: "\u030a", cedil: "\u0327" }))
  for (const letter of "aeiouyncAEIOUYNC") {
    const composed = (letter + combining).normalize("NFC");
    if (composed.length === 1) ENTITIES[letter + mark] = composed;
  }

/** "#65" or "#x41" (or "#X41") as its character; one past the last code point (or not a number) is left as written. */
function codePoint(ref: string): string | undefined {
  const hex = ref[1] === "x" || ref[1] === "X";
  const n = Number.parseInt(ref.slice(hex ? 2 : 1), hex ? 16 : 10);
  return Number.isInteger(n) && n >= 0 && n <= 0x10ffff ? String.fromCodePoint(n) : undefined;
}

/** Page furniture: never the content an agent reads a page for (menus, language lists, inline code, banners). */
const FURNITURE = [
  "script", "style", "noscript", "template", "svg", "iframe", "object", "canvas", "button", "select", "dialog", "nav", "aside",
  "[role=navigation]", "[role=banner]", "[role=contentinfo]", "[role=complementary]", "[role=search]", "[role=dialog]",
  "[hidden]", "[aria-hidden=true]", "body > header", "body > footer", "#header", "#footer", ".site-header", ".site-footer",
  // MediaWiki (Wikipedia): edit links, tables of contents, navigation boxes, maintenance notices, tools
  ".mw-editsection", ".mw-jump-link", "#toc", ".toc", ".vector-toc", ".navbox", ".vertical-navbox", ".ambox", ".mbox-small",
  ".metadata", ".noprint", ".catlinks", ".printfooter", "#siteSub", ".mw-indicators", ".vector-page-toolbar", ".vector-menu", ".mw-portlet",
].join(", ");

/** Where a page keeps its content, most specific first: the first of these with real text is what is read. */
const CONTENT = ["#mw-content-text", "[role=main]", "main", "article", "#content", "#main", ".entry-content", ".post-content", ".article-body"];

/** Text characters in some HTML, roughly: what tells a content region from an empty frame. */
const textLength = (html: string) => html.replace(/<!--[\s\S]*?-->|<[^>]+>/g, "").replace(/\s+/g, " ").length;

/** The share of the page's text a content region must hold: less is a card or a comment, not the content. */
const CONTENT_SHARE = 0.2;

/**
 * The part of a page worth reading: furniture removed, and only the content region when the page
 * marks one (main, article, MediaWiki's content) that holds most of its text. What htmlToText then reads.
 */
export function readable(html: string): string {
  let n = 0;
  const marks = new HTMLRewriter()
    .on(FURNITURE, { element: (e) => void e.remove() })
    // An attribute holding < or > (MediaWiki's data-mw) would end a tag early for htmlToText's patterns.
    .on("*", {
      element(e) {
        for (const [name, value] of [...e.attributes]) if (/[<>]/.test(value)) e.removeAttribute(name);
      },
    });
  CONTENT.forEach((selector, i) =>
    marks.on(selector, {
      element(e) {
        const k = n++;
        e.prepend(`<!--sx${i}.${k}-->`, { html: true });
        e.append(`<!--/sx${i}.${k}-->`, { html: true });
      },
    }),
  );
  let cleaned: string;
  try {
    cleaned = marks.transform(html);
  } catch {
    return html; // not HTML lol-html can read: leave it to htmlToText as it is
  }
  const whole = textLength(cleaned.replace(/<head[\s\S]*?<\/head>/i, ""));
  for (let i = 0; i < CONTENT.length; i++) {
    // Each match on its own (a page can have many <article>s: cards, comments): the one with the most text.
    let best = "";
    let bestLength = 0;
    for (const m of cleaned.matchAll(new RegExp(`<!--sx${i}\\.(\\d+)-->`, "g"))) {
      const end = cleaned.indexOf(`<!--/sx${i}.${m[1]}-->`, m.index);
      const region = cleaned.slice(m.index, end > 0 ? end : undefined);
      const length = textLength(region);
      if (length > bestLength) [best, bestLength] = [region, length];
    }
    if (bestLength < 200 || bestLength < whole * CONTENT_SHARE) continue;
    // What the region leaves behind and the reading needs: the base for relative links, the page's title.
    const base = /<base\s[^>]*>/i.exec(html)?.[0] ?? "";
    const title = /<title[^>]*>([\s\S]*?)<\/title>/i.exec(html)?.[1]?.trim();
    return `${base}${title && !/<h1[\s>]/i.test(best) ? `<h1>${title}</h1>` : ""}${best}`;
  }
  return cleaned;
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
    .replace(/^ ?(?:- ?)+$/gm, "") // the bullets of emptied list items
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
      const { text: body, links } = /html/.test(type) ? htmlToText(readable(raw), res.url) : { text: raw, links: [] };
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
