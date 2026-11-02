import { t } from "@sasacode/host";
import { existsSync } from "node:fs";
import { childEnv, errorResult, type Plugin, type ToolDefinition } from "@sasacode/plugin-api";
import { installRustxng, managedRustxng, RUSTXNG_RELEASES } from "./rustxng-install.ts";
import { htmlToText, readable, readCapped, USER_AGENT } from "./web-fetch.ts";

const DEFAULT_PORT = 47888;
/** Seconds without a request before the sidecar exits (RUSTXNG_IDLE_TIMEOUT). */
const DEFAULT_IDLE = 600;
const DEFAULT_RESULTS = 8;
/** Page text kept per result when include_text is on. */
const TEXT_CHARS = 4_000;
/** Results whose page include_text reads, at most: the first ones that match the query. */
const TEXT_PAGES = 3;
/** Snippet kept per result: some engines send whole paragraphs. */
const SNIPPET_CHARS = 300;

interface SearxResult {
  title?: string;
  url?: string;
  content?: string;
  /** The page's main text, read for include_text; or why it was not. */
  text?: string;
  textNote?: string;
}
interface SearxResponse {
  results?: SearxResult[];
  answers?: (string | { answer?: string })[];
  corrections?: string[];
  suggestions?: string[];
}

async function healthy(base: string): Promise<boolean> {
  try {
    const res = await fetch(`${base}/healthz`, { signal: AbortSignal.timeout(1_000) });
    await res.body?.cancel().catch(() => {});
    return res.ok;
  } catch {
    return false;
  }
}

const words = (s: string) => s.normalize("NFKC").toLowerCase().replace(/[\s\-_・.]+/g, "");

/**
 * Whether a result looks like it is about the query: enough of the query's words appear in its title
 * and snippet, compared without case, width, spaces or hyphens. Enough is two (or all of a shorter
 * query), and one for the engine's first result.
 */
export function matchesQuery(query: string, r: SearxResult, first = false): boolean {
  const terms = query.split(/\s+/).map(words).filter(Boolean);
  const hay = words(`${r.title ?? ""} ${r.content ?? ""}`);
  return terms.filter((t) => hay.includes(t)).length >= (first ? 1 : Math.min(2, terms.length));
}

/** A result page's main text (furniture and menus left out), at most TEXT_CHARS + 1 characters. */
async function pageText(url: string, signal: AbortSignal): Promise<string> {
  const res = await fetch(url, { signal: AbortSignal.any([signal, AbortSignal.timeout(15_000)]), headers: { "user-agent": USER_AGENT, accept: "text/html,text/plain;q=0.9" } });
  const type = res.headers.get("content-type") ?? "";
  if (!res.ok) throw new Error(`${res.status}`);
  if (!/html|text\/plain/.test(type)) throw new Error(`not a text page (${type || "no type"})`);
  const { text: raw } = await readCapped(res, 5_000_000);
  // The links' targets are left out here: the agent opens a page with web_fetch to follow them.
  const text = /html/.test(type) ? htmlToText(readable(raw), res.url).text.replace(/\[([^\]]*)\]\[\d+\]/g, "$1") : raw;
  return text.trim().slice(0, TEXT_CHARS + 1);
}

export function formatResults(query: string, data: SearxResponse): string {
  const results = (data.results ?? []).filter((r) => r.url);
  if (!results.length) {
    const other = [...(data.corrections ?? []), ...(data.suggestions ?? [])].slice(0, 3);
    return `No results for "${query}". Try different words${other.length ? ` (perhaps: ${other.join(", ")})` : ""}.`;
  }
  const lines: string[] = [];
  const answers = (data.answers ?? []).map((a) => (typeof a === "string" ? a : a.answer)).filter(Boolean);
  if (answers.length) lines.push(`Answer: ${answers.join(" / ")}`, "");
  results.forEach((r, i) => {
    lines.push(`${i + 1}. ${(r.title ?? "").trim() || r.url}`, `   ${r.url}`);
    const snippet = (r.content ?? "").replace(/\s+/g, " ").trim();
    if (snippet) lines.push(`   ${snippet.length > SNIPPET_CHARS ? `${snippet.slice(0, SNIPPET_CHARS)}…` : snippet}`);
    if (r.text) lines.push(`   --- page text (read${r.text.length > TEXT_CHARS ? `, first ${TEXT_CHARS} characters` : ""}) ---`, r.text.slice(0, TEXT_CHARS), "   ---");
    else if (r.textNote) lines.push(`   (${r.textNote})`);
  });
  lines.push("", "A snippet is the search engine's excerpt, not the page: read a page (page text above, or web_fetch) before citing it.");
  return lines.join("\n");
}

/**
 * web_search: search the web through RustXNG (https://github.com/sahenjp/RustXNG), a keyless
 * SearXNG-compatible metasearch server, so the agent finds URLs instead of guessing them. Nothing
 * stays resident: the first search starts `rustxng` as a sidecar on 127.0.0.1 that exits by itself
 * after RUSTXNG_IDLE_TIMEOUT seconds without a request, and sessions that run meanwhile share it.
 * With `url` set, that server (RustXNG or SearXNG) is used and nothing is started. While browsr's
 * search tools are connected, web_search steps aside so the model is not given two ways to search.
 * Inactive until rustxng is installed (on PATH, or by `/web-search install` / `sasacode web-search
 * install` into ~/.sasacode/bin) or a url is set; an install while running takes effect at once.
 *
 * settings: { command?: string, url?: string, port?: number, idleTimeout?: number, results?: number }
 */
const webSearch: Plugin = (api) => {
  const url = (api.settings.url as string | undefined)?.replace(/\/+$/, "");
  const configured = api.settings.command as string | undefined;
  const find = (): string | undefined => {
    if (url) return;
    if (configured) return Bun.which(configured) ?? (existsSync(configured) ? configured : undefined);
    // PATH as it is now: Bun.which alone keeps reading the one the process started with.
    return Bun.which("rustxng", { PATH: process.env.PATH ?? "" }) ?? (existsSync(managedRustxng()) ? managedRustxng() : undefined);
  };
  let command = find();
  const base = url ?? `http://127.0.0.1:${Number(api.settings.port ?? DEFAULT_PORT)}`;
  const idle = Number(api.settings.idleTimeout ?? DEFAULT_IDLE);
  const count = Number(api.settings.results ?? DEFAULT_RESULTS);

  api.registerCommand({
    name: "web-search",
    description: t("web_search（RustXNG）の状態。install で RustXNG を入れる"),
    argumentHint: "[install [tag]]",
    complete: (prefix) => ("install".startsWith(prefix) ? [{ value: "install", label: t("RustXNG を入れる・更新する") }] : []),
    run: async ({ args }) => {
      const [sub, tag] = (args ?? "").trim().split(/\s+/);
      if (sub === "install") {
        try {
          const done = await installRustxng({ tag, log: (m) => api.ui.notify(m) });
          command = find();
          sync();
          api.ui.notify(
            command === done.path
              ? t("{version} を {path} に入れました。web_search が使えます。", done)
              : t("{version} を {path} に入れました（ただし web_search は {command} を使います）。", { ...done, command: command ?? "" }),
          );
        } catch (e) {
          api.ui.notify(t("RustXNG を入れられませんでした: {error}", { error: (e as Error).message }), "error");
        }
        return;
      }
      if (!url && !command)
        return api.ui.notify(
          configured
            ? t("web-search: {command} が見つかりません", { command: configured })
            : t("rustxng が見つかりません。/web-search install（または sasacode web-search install）で入れるか、{releases} から入れて PATH に置いてください。", { releases: RUSTXNG_RELEASES }),
        );
      const up = await healthy(base);
      api.ui.notify(
        url
          ? `web_search: ${url} · ${up ? t("応答あり") : t("応答なし")}`
          : `web_search: ${command} · ${base} · ${up ? t("起動中") : t("停止中（次の検索で起動、{idle} 秒使わなければ終了）", { idle })}`,
      );
    },
  });
  if (!url && !command && configured) api.ui.notify(t("web-search: {command} が見つかりません", { command: configured }), "warning");

  /** One start at a time: concurrent searches wait for the same sidecar. */
  let starting: Promise<void> | undefined;
  async function ensure(signal: AbortSignal): Promise<void> {
    if (url || (await healthy(base))) return;
    starting ??= (async () => {
      const port = new URL(base).port;
      // Detached and unreferenced: it outlives this session and ends on its own idle timeout. (Not on
      // Windows, where Bun's detached children still end with the parent: there it is per session.)
      const proc = Bun.spawn([command!, "--bind", "127.0.0.1", "--port", port], {
        env: childEnv({ RUSTXNG_IDLE_TIMEOUT: String(idle), RUST_LOG: process.env.RUST_LOG ?? "warn" }),
        stdio: ["ignore", "ignore", "ignore"],
        detached: true,
      });
      proc.unref();
      const deadline = Date.now() + 20_000;
      while (Date.now() < deadline && !signal.aborted) {
        if (await healthy(base)) return;
        // Exited without answering: another session's sidecar may have taken the port first.
        if (proc.exitCode !== null && !(await healthy(base)))
          throw new Error(`rustxng exited (${proc.exitCode}) without answering on ${base}; is the port in use? Set plugins.settings.web-search.port`);
        await Bun.sleep(150);
      }
      if (!(await healthy(base))) throw new Error(`rustxng did not answer on ${base}`);
    })().finally(() => (starting = undefined));
    await starting;
  }

  const tool: ToolDefinition = {
    name: "web_search",
    description:
      "Search the web. Use it to find the right URL instead of guessing one, then read the page with web_fetch. Returns titles, URLs and snippets; include_text also reads the main text of the first results that match the query (slower).",
    parameters: {
      type: "object",
      properties: {
        query: { type: "string", description: "Search words" },
        max_results: { type: "integer", minimum: 1, maximum: 20, description: `Number of results (default ${count})` },
        include_text: { type: "boolean", description: `Also read the main text of the first ${TEXT_PAGES} results that match the query` },
      },
      required: ["query"],
      additionalProperties: false,
    },
    kind: "other",
    concurrent: true,
    matchTarget: (a) => a.query,
    summary: (a) => a.query,
    async execute(args: { query: string; max_results?: number; include_text?: boolean }, ctx) {
      if (!args.query.trim()) return errorResult("The query is empty.");
      try {
        await ensure(ctx.signal);
      } catch (e) {
        return errorResult(`web_search is unavailable: ${(e as Error).message}`);
      }
      const n = args.max_results ?? count;
      const params = new URLSearchParams({ q: args.query, format: "json", max_results: String(n) });
      let data: SearxResponse;
      try {
        const res = await fetch(`${base}/search?${params}`, {
          signal: AbortSignal.any([ctx.signal, AbortSignal.timeout(30_000)]),
        });
        if (!res.ok) return errorResult(`Search failed: ${res.status} ${res.statusText}`);
        data = (await res.json()) as SearxResponse;
        data.results = data.results?.slice(0, n); // SearXNG itself has no max_results
      } catch (e) {
        return errorResult(`Search failed: ${(e as Error).message}`);
      }
      if (args.include_text) {
        // Read here rather than by the server: its page text keeps menus, inline CSS and scripts.
        const chosen: SearxResult[] = [];
        for (const [i, r] of (data.results ?? []).entries()) {
          if (chosen.length >= TEXT_PAGES || !r.url) break;
          if (matchesQuery(args.query, r, i === 0)) chosen.push(r);
          else r.textNote = "page not read: its title and snippet share little with the query";
        }
        await Promise.all(
          chosen.map(async (r) => {
            try {
              r.text = await pageText(r.url!, ctx.signal);
              if (!r.text) r.textNote = "page read, but it has no text";
            } catch (e) {
              r.textNote = `page could not be read: ${(e as Error).message}`;
            }
          }),
        );
      }
      return { content: [{ type: "text", text: formatResults(args.query, data) }] };
    },
  };
  let registered = false;
  /** Offered when there is something to search with and browsr's search is not connected. */
  function sync() {
    command ??= find(); // installed meanwhile (sasacode web-search install from a shell)
    const browsr = api.agent.tools().some((t) => t.name === "search" || t.name === "web");
    const offer = Boolean(url || command) && !browsr;
    if (offer && !registered) api.registerTool(tool);
    else if (!offer && registered) api.unregisterTool("web_search");
    registered = offer;
  }
  sync();
  // Checked before each request, as browsr connects in the background and can drop.
  api.on("system_prompt", sync);
};
export default webSearch;
