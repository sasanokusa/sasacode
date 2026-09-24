import type { Api, ModelInfo } from "./types.ts";

export interface ProviderConfig {
  api: Api;
  baseUrl?: string;
  /** Environment variable holding the API key. */
  apiKeyEnv?: string;
  headers?: Record<string, string>;
  /** Shown in the generated ~/.sasacode/.env. */
  label?: string;
  /** Model used when nothing is configured and this provider's key is set. */
  defaultModel?: string;
}

/** Order matters: with no model configured, the first provider whose key is set supplies the default. */
export const BUILTIN_PROVIDERS: Record<string, ProviderConfig> = {
  anthropic: { api: "anthropic", apiKeyEnv: "ANTHROPIC_API_KEY", label: "Anthropic", defaultModel: "claude-opus-5" },
  openai: { api: "openai-responses", apiKeyEnv: "OPENAI_API_KEY", label: "OpenAI", defaultModel: "gpt-5.5" },
  "openai-chat": { api: "openai-chat", apiKeyEnv: "OPENAI_API_KEY" },
  openrouter: {
    api: "openai-chat",
    baseUrl: "https://openrouter.ai/api/v1",
    apiKeyEnv: "OPENROUTER_API_KEY",
    label: "OpenRouter",
    defaultModel: "openrouter/auto",
  },
  ollama: { api: "openai-chat", baseUrl: "http://localhost:11434/v1" },
  // Command Code serves all three formats on one key; Claude models answer on /messages only.
  commandcode: {
    api: "openai-chat",
    baseUrl: "https://api.commandcode.ai/provider/v1",
    apiKeyEnv: "CMD_API_KEY",
    label: "Command Code",
    defaultModel: "deepseek/deepseek-v4-flash",
  },
  "commandcode-responses": { api: "openai-responses", baseUrl: "https://api.commandcode.ai/provider/v1", apiKeyEnv: "CMD_API_KEY" },
  "commandcode-anthropic": { api: "anthropic", baseUrl: "https://api.commandcode.ai/provider", apiKeyEnv: "CMD_API_KEY" },
};

type Known = Omit<ModelInfo, "id" | "provider" | "api" | "baseUrl">;

const claude = (input: number, output: number, contextWindow = 1_000_000): Known => ({
  contextWindow,
  maxOutput: 64_000,
  price: { input, output },
  reasoning: true,
  images: true,
});

/** Known model metadata. Anything else gets conservative defaults and can be overridden in config. */
export const KNOWN_MODELS: Record<string, Known> = {
  "claude-fable-5-1": claude(10, 50),
  "claude-opus-5-5": { ...claude(4, 20), price: { input: 4, output: 20, cacheRead: 0.2 } },
  "claude-opus-5": claude(5, 25),
  "claude-opus-4-8": claude(5, 25),
  "claude-sonnet-5": claude(2, 10),
  "claude-sonnet-4-6": claude(3, 15),
  "claude-haiku-4-5": { ...claude(1, 5, 200_000), reasoning: false },
};

export const DEFAULT_MODEL = "anthropic/claude-opus-5";

/** First provider with a non-empty key in the environment, as "<provider>/<model>". */
export function defaultModelFor(providers: Record<string, ProviderConfig>, env: Record<string, string | undefined> = process.env): string {
  for (const [name, p] of Object.entries(providers))
    if (p.defaultModel && p.apiKeyEnv && env[p.apiKeyEnv]?.trim()) return `${name}/${p.defaultModel}`;
  return DEFAULT_MODEL;
}

export function resolveModel(
  spec: string,
  providers: Record<string, ProviderConfig>,
  overrides: Record<string, Partial<ModelInfo>> = {},
): ModelInfo {
  const slash = spec.indexOf("/");
  if (slash < 0) throw new Error(`model must be "<provider>/<model>", got "${spec}"`);
  const provider = spec.slice(0, slash);
  const id = spec.slice(slash + 1);
  const p = providers[provider];
  if (!p) throw new Error(`unknown provider "${provider}" (known: ${Object.keys(providers).join(", ")})`);
  const known = KNOWN_MODELS[id];
  return {
    id,
    provider,
    api: p.api,
    baseUrl: p.baseUrl,
    headers: p.headers,
    contextWindow: known?.contextWindow ?? 128_000,
    maxOutput: known?.maxOutput ?? 32_000,
    price: known?.price,
    reasoning: known?.reasoning ?? p.api !== "openai-chat",
    images: known?.images ?? true,
    ...overrides[spec],
  };
}
