import { t } from "@sasacode/host";
import { errorResult, type Plugin } from "@sasacode/plugin-api";

const RELEASES = "https://github.com/sahenjp/RustXNG/releases";
const DEFAULT_PORT = 47888;
/** Seconds without a request before the sidecar exits (RUSTXNG_IDLE_TIMEOUT). */
const DEFAULT_IDLE = 600;
const DEFAULT_RESULTS = 8;
/** Page text kept per result when include_text is on. */
const TEXT_CHARS = 4_000;
/** Snippet kept per result: some engines send whole paragraphs. */
const SNIPPET_CHARS = 300;

interface SearxResult {
  title?: string;
  url?: string;
  content?: string;
  raw_content?: string;
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
    const text = (r.raw_content ?? "").trim();
    if (text) lines.push(`   --- page text${text.length > TEXT_CHARS ? ` (first ${TEXT_CHARS} characters)` : ""} ---`, text.slice(0, TEXT_CHARS), "   ---");
  });
  lines.push("", "Open a result with web_fetch to read it.");
  return lines.join("\n");
}

/**
 * web_search: search the web through RustXNG (https://github.com/sahenjp/RustXNG), a keyless
 * SearXNG-compatible metasearch server, so the agent finds URLs instead of guessing them. Nothing
 * stays resident: the first search starts `rustxng` as a sidecar on 127.0.0.1 that exits by itself
 * after RUSTXNG_IDLE_TIMEOUT seconds without a request, and sessions that run meanwhile share it.
 * With `url` set, that server (RustXNG or SearXNG) is used and nothing is started.
 * Inactive when rustxng is not installed and no url is set.
 *
 * settings: { command?: string, url?: string, port?: number, idleTimeout?: number, results?: number }
 */
const webSearch: Plugin = (api) => {
  const url = (api.settings.url as string | undefined)?.replace(/\/+$/, "");
  const configured = api.settings.command as string | undefined;
  const command = url ? undefined : (Bun.which(configured ?? "rustxng") ?? undefined);
  const base = url ?? `http://127.0.0.1:${Number(api.settings.port ?? DEFAULT_PORT)}`;
  const idle = Number(api.settings.idleTimeout ?? DEFAULT_IDLE);
  const count = Number(api.settings.results ?? DEFAULT_RESULTS);

  api.registerCommand({
    name: "web-search",
    description: t("web_search（RustXNG）の状態"),
    run: async () => {
      if (!url && !command)
        return api.ui.notify(configured ? t("web-search: {command} が見つかりません", { command: configured }) : t("rustxng が見つかりません。{releases} から入れて PATH に置くか、plugins.settings.web-search.command を指定してください。", { releases: RELEASES }));
      const up = await healthy(base);
      api.ui.notify(
        url
          ? `web_search: ${url} · ${up ? t("応答あり") : t("応答なし")}`
          : `web_search: ${command} · ${base} · ${up ? t("起動中") : t("停止中（次の検索で起動、{idle} 秒使わなければ終了）", { idle })}`,
      );
    },
  });
  if (!url && !command) {
    if (configured) api.ui.notify(t("web-search: {command} が見つかりません", { command: configured }), "warning");
    return;
  }

  /** One start at a time: concurrent searches wait for the same sidecar. */
  let starting: Promise<void> | undefined;
  async function ensure(signal: AbortSignal): Promise<void> {
    if (url || (await healthy(base))) return;
    starting ??= (async () => {
      const port = new URL(base).port;
      // Detached and unreferenced: it outlives this session and ends on its own idle timeout. (Not on
      // Windows, where Bun's detached children still end with the parent: there it is per session.)
      const proc = Bun.spawn([command!, "--bind", "127.0.0.1", "--port", port], {
        env: { ...process.env, RUSTXNG_IDLE_TIMEOUT: String(idle), RUST_LOG: process.env.RUST_LOG ?? "warn" },
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

  api.registerTool({
    name: "web_search",
    description:
      "Search the web. Use it to find the right URL instead of guessing one, then read the page with web_fetch. Returns titles, URLs and snippets; include_text also returns the main text of the top pages (slower).",
    parameters: {
      type: "object",
      properties: {
        query: { type: "string", description: "Search words" },
        max_results: { type: "integer", minimum: 1, maximum: 20, description: `Number of results (default ${count})` },
        include_text: { type: "boolean", description: "Also return the main text of the top pages" },
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
      if (args.include_text) params.set("include_raw_content", "1");
      let data: SearxResponse;
      try {
        const res = await fetch(`${base}/search?${params}`, {
          signal: AbortSignal.any([ctx.signal, AbortSignal.timeout(args.include_text ? 60_000 : 30_000)]),
        });
        if (!res.ok) return errorResult(`Search failed: ${res.status} ${res.statusText}`);
        data = (await res.json()) as SearxResponse;
        data.results = data.results?.slice(0, n); // SearXNG itself has no max_results
      } catch (e) {
        return errorResult(`Search failed: ${(e as Error).message}`);
      }
      return { content: [{ type: "text", text: formatResults(args.query, data) }] };
    },
  });
};
export default webSearch;
