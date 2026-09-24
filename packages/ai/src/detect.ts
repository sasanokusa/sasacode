import type { ProviderConfig } from "./models.ts";

export interface DetectedEndpoint {
  api: "openai-chat" | "anthropic";
  /** Normalized for that API's SDK: ".../v1" for OpenAI-style, the root for Anthropic. */
  baseUrl: string;
  ollama?: boolean;
  llamacpp?: boolean;
}

/**
 * Work out what speaks at `url` from its Models API. Anthropic lists look like
 * `{ data: [{ type: "model", … }], has_more }`; OpenAI-style ones (OpenAI, llama.cpp, vLLM,
 * LM Studio, Ollama …) like `{ object: "list", data: [{ object: "model", … }] }`.
 * OpenAI-style servers get Chat Completions, the format they all implement.
 */
export async function detectEndpoint(url: string, apiKey?: string, signal?: AbortSignal): Promise<DetectedEndpoint> {
  const base = url.replace(/\/+$/, "");
  const root = base.replace(/\/v1$/, "");
  const headers: Record<string, string> = { "anthropic-version": "2023-06-01" };
  if (apiKey) {
    headers.authorization = `Bearer ${apiKey}`;
    headers["x-api-key"] = apiKey;
  }
  // The standard location first, so the stored base URL ends in /v1 where the server has one.
  const candidates = [...new Set([`${root}/v1/models`, `${base}/models`])];
  let lastError = "no response";
  for (const listUrl of candidates) {
    let res: Response;
    try {
      res = await fetch(listUrl, { headers, signal });
    } catch (e) {
      lastError = e instanceof Error ? e.message : String(e);
      continue;
    }
    if (res.status === 401 || res.status === 403) throw new Error(`${listUrl} needs an API key (HTTP ${res.status})`);
    if (!res.ok) {
      lastError = `${listUrl}: HTTP ${res.status}`;
      continue;
    }
    const body = (await res.json().catch(() => undefined)) as Record<string, any> | undefined;
    const data: any[] | undefined = Array.isArray(body?.data) ? body.data : undefined;
    if (!data) {
      lastError = `${listUrl}: not a model list`;
      continue;
    }
    const listRoot = listUrl.replace(/\/models$/, "");
    if ("has_more" in body! || data[0]?.type === "model") return { api: "anthropic", baseUrl: listRoot.replace(/\/v1$/, "") };
    const endpoint: DetectedEndpoint = { api: "openai-chat", baseUrl: listRoot };
    const owner = String(data[0]?.owned_by ?? "");
    if (owner === "llamacpp" || data[0]?.meta?.n_ctx_train !== undefined) endpoint.llamacpp = true;
    else if (owner === "library" || (await isOllama(root, signal))) endpoint.ollama = true;
    return endpoint;
  }
  throw new Error(`could not find a model list at ${url} (${lastError})`);
}

async function isOllama(root: string, signal?: AbortSignal): Promise<boolean> {
  try {
    const res = await fetch(`${root}/api/version`, { signal });
    return res.ok && typeof ((await res.json()) as { version?: unknown }).version === "string";
  } catch {
    return false;
  }
}

/** Fill in `api` (and the server flavour) for a provider configured with only a URL. */
export function applyDetected(p: Partial<ProviderConfig>, d: DetectedEndpoint): ProviderConfig {
  return { ...p, api: d.api, baseUrl: d.baseUrl, ...(d.ollama ? { ollama: true } : {}), ...(d.llamacpp ? { llamacpp: true } : {}) } as ProviderConfig;
}
