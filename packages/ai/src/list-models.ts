import Anthropic from "@anthropic-ai/sdk";
import OpenAI from "openai";
import type { ProviderConfig } from "./models.ts";
import type { Api, ModelInfo } from "./types.ts";

export interface ListedModel {
  id: string;
  /** The context the server actually uses for this model. */
  contextWindow?: number;
  /** The most the model supports, when the effective size is not known (Ollama without num_ctx). */
  maxContext?: number;
  maxOutput?: number;
  /** Endpoints the model answers on, when the provider says (e.g. Command Code: "/messages"). */
  endpoints?: string[];
  /** List price in USD per 1M tokens, when the listing reports one (OpenRouter's `pricing`). */
  price?: ModelInfo["price"];
}

export const API_ENDPOINT: Record<string, string> = {
  anthropic: "/messages",
  "openai-chat": "/chat/completions",
  "openai-responses": "/responses",
};

// OpenAI-style lists include embedding, audio and image models that cannot drive an agent.
const NOT_CHAT = /embed|whisper|tts|dall-e|gpt-image|moderation|transcribe|realtime|audio|search-preview|^babbage|^davinci/i;

/**
 * OpenRouter's `pricing` (per token, decimal strings like "0.0000015") as `ModelInfo.price` (USD per
 * 1M tokens). A zero or missing price is "unknown", not free: returning undefined keeps it from
 * overriding a price the built-in table or a config override already knows.
 */
export function parsePricing(pricing: unknown): ModelInfo["price"] | undefined {
  const p = pricing as Record<string, unknown> | undefined;
  if (!p || typeof p !== "object") return undefined;
  const perMillion = (v: unknown): number | undefined => {
    const n = typeof v === "string" || typeof v === "number" ? Number(v) : NaN;
    return Number.isFinite(n) && n > 0 ? n * 1e6 : undefined;
  };
  const input = perMillion(p.prompt);
  const output = perMillion(p.completion);
  if (input === undefined && output === undefined) return undefined;
  const cacheRead = perMillion(p.input_cache_read);
  const cacheWrite = perMillion(p.input_cache_write);
  return {
    input: input ?? 0,
    output: output ?? 0,
    ...(cacheRead !== undefined ? { cacheRead } : {}),
    ...(cacheWrite !== undefined ? { cacheWrite } : {}),
  };
}

/** GET <baseUrl>/models (the Models API) for one provider. Throws on network or auth errors. */
export async function listModels(p: ProviderConfig, apiKey: string | undefined, signal?: AbortSignal): Promise<ListedModel[]> {
  const out: ListedModel[] = [];
  const num = (v: unknown) => (typeof v === "number" && v > 0 ? v : undefined);
  if (p.api === "anthropic") {
    const client = new Anthropic({ ...(apiKey ? { apiKey } : {}), baseURL: p.baseUrl, maxRetries: 1, defaultHeaders: p.headers });
    for await (const m of client.models.list({ limit: 100 }, { signal })) {
      const x = m as unknown as Record<string, unknown>;
      out.push({ id: m.id, contextWindow: num(x.max_input_tokens), maxOutput: num(x.max_tokens), endpoints: ["/messages"] });
    }
  } else {
    const client = new OpenAI({ apiKey: apiKey || "none", baseURL: p.baseUrl, maxRetries: 1, defaultHeaders: p.headers });
    for await (const m of client.models.list({ signal })) {
      const x = m as unknown as Record<string, unknown>;
      const top = x.top_provider as Record<string, unknown> | undefined; // OpenRouter
      const meta = x.meta as Record<string, unknown> | undefined; // llama.cpp
      out.push({
        id: m.id,
        maxContext: num(meta?.n_ctx_train),
        // max_model_len: vLLM's window (input and output together).
        contextWindow: num(x.context_length) ?? num(x.context_window) ?? num(x.max_input_tokens) ?? num(x.max_model_len) ?? num(meta?.n_ctx),
        maxOutput: num(top?.max_completion_tokens) ?? num(x.max_output_tokens),
        endpoints: Array.isArray(x.supported_endpoints) ? (x.supported_endpoints as string[]) : undefined,
        price: parsePricing(x.pricing),
      });
    }
  }
  const chat = out.filter((m) => !NOT_CHAT.test(m.id));
  if (p.ollama) {
    const loaded = await ollamaLoaded(p, signal);
    await Promise.all(chat.map((m) => ollamaDetails(p, m, loaded, signal)));
  }
  if (p.llamacpp) await llamacppContext(p, chat, signal);
  return chat;
}

/** llama-server serves one model; /props has the context each request slot gets. */
async function llamacppContext(p: ProviderConfig, models: ListedModel[], signal?: AbortSignal): Promise<void> {
  const root = (p.baseUrl ?? "").replace(/\/v1\/?$/, "");
  try {
    const res = await fetch(`${root}/props`, { signal, headers: p.headers });
    if (!res.ok) return;
    const props = (await res.json()) as { default_generation_settings?: { n_ctx?: number } };
    const nCtx = props.default_generation_settings?.n_ctx;
    if (typeof nCtx === "number" && nCtx > 0) for (const m of models) m.contextWindow = nCtx;
  } catch {}
}

const ollamaRoot = (p: ProviderConfig) => (p.baseUrl ?? "http://localhost:11434/v1").replace(/\/v1\/?$/, "");

/** Models Ollama has in memory, with the context they were loaded with (OLLAMA_CONTEXT_LENGTH included). */
async function ollamaLoaded(p: ProviderConfig, signal?: AbortSignal): Promise<Map<string, number>> {
  try {
    const res = await fetch(`${ollamaRoot(p)}/api/ps`, { signal });
    const body = res.ok ? ((await res.json()) as { models?: { name?: string; model?: string; context_length?: number }[] }) : {};
    return new Map((body.models ?? []).flatMap((m) => (m.context_length ? [[m.name ?? m.model ?? "", m.context_length] as [string, number]] : [])));
  } catch {
    return new Map();
  }
}

/**
 * Ollama's /v1/models has no sizes; its native /api/show has the trained context length and,
 * when the Modelfile sets it, num_ctx (what requests actually get). A loaded model's size in
 * /api/ps is what it really runs with, whatever set it.
 */
async function ollamaDetails(p: ProviderConfig, m: ListedModel, loaded: Map<string, number>, signal?: AbortSignal): Promise<void> {
  const running = loaded.get(m.id);
  if (running) {
    m.contextWindow = running;
    return;
  }
  const root = ollamaRoot(p);
  try {
    const res = await fetch(`${root}/api/show`, { method: "POST", body: JSON.stringify({ model: m.id }), signal });
    if (!res.ok) return;
    const info = (await res.json()) as { model_info?: Record<string, unknown>; parameters?: string };
    const trained = Object.entries(info.model_info ?? {}).find(([k]) => k.endsWith(".context_length"))?.[1];
    const numCtx = /^num_ctx\s+(\d+)/m.exec(info.parameters ?? "")?.[1];
    if (numCtx) m.contextWindow = Number(numCtx);
    else if (typeof trained === "number") m.maxContext = trained;
  } catch {}
}

export function apiForEndpoint(endpoint: string): Api | undefined {
  return Object.keys(API_ENDPOINT).find((a) => API_ENDPOINT[a] === endpoint);
}
