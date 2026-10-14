// The ChatGPT-plan backend (chatgpt.com/backend-api/codex) speaks the Responses API, so this
// provider hands requests to the core openai-responses implementation and only adapts the wire:
// the plan's token and account header, and the fields this backend rejects.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { getProvider, type Provider, type Request, type StreamEvent } from "@sasacode/ai";
import { sasacodeHome } from "@sasacode/plugin-api";
import { codexTokens, type CodexTokens } from "./auth.ts";

/**
 * The backend hides models from clients older than their release, so it is told the latest Codex
 * CLI version (npm @openai/codex), looked up once per process. When npm does not answer, the
 * version found last time is used, and this one only when there has never been an answer.
 */
export const CODEX_CLIENT_VERSION = "0.162.1";

/** Where the last version npm reported is kept. */
export const codexVersionPath = (): string => join(sasacodeHome(), "codex-version");

const RELEASE = /^\d+\.\d+\.\d+$/;

/** The latest Codex CLI release on npm, else the one last seen there, else CODEX_CLIENT_VERSION. */
export async function latestCodexVersion(doFetch: typeof fetch = fetch, path = codexVersionPath()): Promise<string> {
  const registry = (process.env.SASACODE_NPM_REGISTRY ?? "https://registry.npmjs.org").replace(/\/$/, "");
  try {
    const res = await doFetch(`${registry}/-/package/@openai/codex/dist-tags`, { signal: AbortSignal.timeout(3000) });
    const latest = res.ok ? ((await res.json()) as { latest?: unknown }).latest : undefined;
    if (typeof latest === "string" && RELEASE.test(latest)) {
      try {
        mkdirSync(dirname(path), { recursive: true });
        writeFileSync(path, `${latest}\n`);
      } catch {}
      return latest;
    }
  } catch {}
  try {
    const last = readFileSync(path, "utf8").trim();
    if (RELEASE.test(last)) return last;
  } catch {}
  return CODEX_CLIENT_VERSION;
}

let lookedUp: Promise<string> | undefined;
const clientVersion = () => (lookedUp ??= latestCodexVersion());

/** Shown when signed in but the backend's model list cannot be fetched. */
export const FALLBACK_MODELS = ["gpt-6.1-sol", "gpt-6-astra", "gpt-6-sol", "gpt-6-luna", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5"];

/** Fields the public Responses API takes but this backend refuses (Codex never sends them). */
const UNSUPPORTED = ["max_output_tokens", "temperature", "top_p", "metadata", "user"];

const NOT_SIGNED_IN = "Not signed in to a ChatGPT plan. Run: sasacode login";

function headers(tokens: CodexTokens, base: Record<string, string> | undefined, sessionId: string, version: string): Record<string, string> {
  if (!tokens.accountId) throw new Error("The ChatGPT login has no account id. Run: sasacode login");
  return {
    ...base,
    "chatgpt-account-id": tokens.accountId,
    originator: "sasacode",
    "OpenAI-Beta": "responses=experimental",
    // The backend gates models on /responses by this too, not only on /models.
    version,
    "session-id": sessionId,
  };
}

/** Rewrites the JSON body on its way out: drops unsupported fields, keys the prompt cache by session. */
export function codexFetch(sessionId: string, inner: typeof fetch = fetch): typeof fetch {
  return (async (input: Parameters<typeof fetch>[0], init?: RequestInit) => {
    if (typeof init?.body === "string") {
      const body = JSON.parse(init.body) as Record<string, unknown>;
      for (const k of UNSUPPORTED) delete body[k];
      body.store = false;
      body.prompt_cache_key = sessionId;
      init = { ...init, body: JSON.stringify(body) };
    }
    return inner(input, init);
  }) as typeof fetch;
}

export function codexProvider(sessionId: () => string, deps: { tokens?: () => Promise<CodexTokens | undefined>; fetch?: typeof fetch; version?: () => Promise<string> } = {}): Provider {
  const tokens = deps.tokens ?? codexTokens;
  const version = deps.version ?? clientVersion;
  return {
    api: "openai-codex",
    async *stream(req: Request): AsyncGenerator<StreamEvent> {
      const t = await tokens();
      if (!t) throw new Error(NOT_SIGNED_IN);
      const id = sessionId();
      try {
        yield* getProvider("openai-responses").stream({
          ...req,
          apiKey: t.access,
          model: { ...req.model, headers: headers(t, req.model.headers, id, await version()) },
          fetch: codexFetch(id, req.fetch ?? deps.fetch),
          // A usage limit is not fixed by retrying for minutes.
          maxRetries: Math.min(req.maxRetries ?? 2, 2),
        });
      } catch (e) {
        const status = (e as { status?: number }).status;
        if (status === 401 || status === 403) throw new Error(`ChatGPT rejected the login (${status}): ${(e as Error).message}. Run: sasacode login`);
        throw e;
      }
    },
    async listModels(config, _apiKey, signal) {
      // Not signed in: nothing to offer (the models would only fail).
      const t = await tokens().catch(() => undefined);
      if (!t?.accountId) return [];
      const res = await (deps.fetch ?? fetch)(`${config.baseUrl}/models?client_version=${await version()}`, {
        headers: { Authorization: `Bearer ${t.access}`, "chatgpt-account-id": t.accountId, originator: "sasacode" },
        signal,
      });
      if (!res.ok) return FALLBACK_MODELS.map((id) => ({ id }));
      const body = (await res.json()) as { models?: { slug?: string; visibility?: string; context_window?: number }[] };
      const listed = (body.models ?? [])
        // Utility models (e.g. automatic review) are marked hidden.
        .filter((m) => m.slug && m.visibility !== "hide" && m.visibility !== "hidden")
        .map((m) => ({ id: m.slug!, contextWindow: typeof m.context_window === "number" ? m.context_window : undefined }));
      return listed.length ? listed : FALLBACK_MODELS.map((id) => ({ id }));
    },
  };
}
