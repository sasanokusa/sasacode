// The ChatGPT-plan backend (chatgpt.com/backend-api/codex) speaks the Responses API, so this
// provider hands requests to the core openai-responses implementation and only adapts the wire:
// the plan's token and account header, and the fields this backend rejects.
import { getProvider, type Provider, type Request, type StreamEvent } from "@sasacode/ai";
import { codexTokens, type CodexTokens } from "./auth.ts";

/**
 * The backend hides models from clients older than their release, so it is told a recent Codex
 * CLI version (npm @openai/codex). Raise it when new models do not show up in /model.
 */
export const CODEX_CLIENT_VERSION = "0.156.1";

/** Shown when signed in but the backend's model list cannot be fetched. */
export const FALLBACK_MODELS = ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5", "gpt-5.4", "gpt-5.4-mini"];

/** Fields the public Responses API takes but this backend refuses (Codex never sends them). */
const UNSUPPORTED = ["max_output_tokens", "temperature", "top_p", "metadata", "user"];

const NOT_SIGNED_IN = "Not signed in to a ChatGPT plan. Run: sasacode login";

function headers(tokens: CodexTokens, base: Record<string, string> | undefined, sessionId: string): Record<string, string> {
  if (!tokens.accountId) throw new Error("The ChatGPT login has no account id. Run: sasacode login");
  return {
    ...base,
    "chatgpt-account-id": tokens.accountId,
    originator: "sasacode",
    "OpenAI-Beta": "responses=experimental",
    // The backend gates models on /responses by this too, not only on /models.
    version: CODEX_CLIENT_VERSION,
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

export function codexProvider(sessionId: () => string, deps: { tokens?: () => Promise<CodexTokens | undefined>; fetch?: typeof fetch } = {}): Provider {
  const tokens = deps.tokens ?? codexTokens;
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
          model: { ...req.model, headers: headers(t, req.model.headers, id) },
          fetch: codexFetch(id, deps.fetch),
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
      const res = await (deps.fetch ?? fetch)(`${config.baseUrl}/models?client_version=${CODEX_CLIENT_VERSION}`, {
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
