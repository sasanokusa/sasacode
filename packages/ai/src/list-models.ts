import Anthropic from "@anthropic-ai/sdk";
import OpenAI from "openai";
import type { ProviderConfig } from "./models.ts";
import type { Api } from "./types.ts";

export interface ListedModel {
  id: string;
  contextWindow?: number;
  maxOutput?: number;
  /** Endpoints the model answers on, when the provider says (e.g. Command Code: "/messages"). */
  endpoints?: string[];
}

export const API_ENDPOINT: Record<string, string> = {
  anthropic: "/messages",
  "openai-chat": "/chat/completions",
  "openai-responses": "/responses",
};

// OpenAI-style lists include embedding, audio and image models that cannot drive an agent.
const NOT_CHAT = /embed|whisper|tts|dall-e|gpt-image|moderation|transcribe|realtime|audio|search-preview|^babbage|^davinci/i;

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
      out.push({
        id: m.id,
        contextWindow: num(x.context_length) ?? num(x.context_window) ?? num(x.max_input_tokens),
        maxOutput: num(top?.max_completion_tokens) ?? num(x.max_output_tokens),
        endpoints: Array.isArray(x.supported_endpoints) ? (x.supported_endpoints as string[]) : undefined,
      });
    }
  }
  return out.filter((m) => !NOT_CHAT.test(m.id));
}

export function apiForEndpoint(endpoint: string): Api | undefined {
  return Object.keys(API_ENDPOINT).find((a) => API_ENDPOINT[a] === endpoint);
}
