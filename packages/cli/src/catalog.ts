import { API_ENDPOINT, getProvider, KNOWN_MODELS, type ListedModel, listModels, type ModelInfo, type Provider, type ProviderConfig } from "@sasacode/ai";

export interface ModelChoice {
  /** "<provider>/<model>" */
  spec: string;
  /** Effective context window: from the provider, else sasacode's table of known models. */
  contextWindow?: number;
  /** The model's maximum, shown when the effective size is unknown (Ollama without num_ctx). */
  maxContext?: number;
}

const DEFAULT_HOST: Record<string, string> = {
  anthropic: "api.anthropic.com",
  "openai-chat": "api.openai.com",
  "openai-responses": "api.openai.com",
};

function host(p: ProviderConfig): string {
  try {
    return p.baseUrl ? new URL(p.baseUrl).host : (DEFAULT_HOST[p.api] ?? p.api);
  } catch {
    return p.baseUrl ?? p.api;
  }
}

/**
 * Providers that reach the same service with the same key (openai / openai-chat, the three
 * commandcode entries) are one group: listed once, and each model goes to the member whose API
 * format it answers on.
 */
function groups(providers: Record<string, ProviderConfig>): string[][] {
  const byKey = new Map<string, string[]>();
  for (const [name, p] of Object.entries(providers)) {
    const k = `${p.apiKeyEnv ?? ""}@${host(p)}`;
    byKey.set(k, [...(byKey.get(k) ?? []), name]);
  }
  return [...byKey.values()];
}

/** Models the configured providers offer, fetched from their Models API (GET /v1/models). */
export class ModelCatalog {
  /** Per spec, what the provider told us (context window, max output). */
  readonly discovered = new Map<string, Partial<ModelInfo>>();
  private pending?: Promise<ModelChoice[]>;

  constructor(
    private providers: Record<string, ProviderConfig>,
    private getKey: (provider: string) => Promise<string | undefined>,
    /** Also asked without a key (it may authenticate another way, e.g. an `ant auth login` profile). */
    private currentProvider: () => string,
  ) {}

  /** Cached after the first call; `refresh` fetches again. */
  list(refresh = false): Promise<ModelChoice[]> {
    if (refresh || !this.pending) this.pending = this.fetch();
    return this.pending;
  }

  private async fetch(): Promise<ModelChoice[]> {
    const lists = await Promise.all(groups(this.providers).map((g) => this.fetchGroup(g).catch(() => [])));
    return lists.flat();
  }

  private async fetchGroup(members: string[]): Promise<ModelChoice[]> {
    const primary = members[0]!;
    const p = this.providers[primary]!;
    const key = await this.getKey(primary);
    if (p.apiKeyEnv && !key && !members.includes(this.currentProvider())) return [];
    // A provider implementation (e.g. from a plugin) may know its own models.
    let impl: Provider | undefined;
    try {
      impl = getProvider(p.api);
    } catch {}
    const signal = AbortSignal.timeout(8000);
    const models: ListedModel[] = impl?.listModels ? await impl.listModels(p, key, signal) : await listModels(p, key, signal);
    const out: ModelChoice[] = [];
    for (const m of models) {
      const target = m.endpoints
        ? members.find((n) => m.endpoints!.includes(API_ENDPOINT[this.providers[n]!.api] ?? ""))
        : primary;
      if (!target) continue; // e.g. an endpoint sasacode does not speak
      const spec = `${target}/${m.id}`;
      const info: Partial<ModelInfo> = {};
      if (m.contextWindow) info.contextWindow = m.contextWindow;
      if (m.maxOutput) info.maxOutput = m.maxOutput;
      if (Object.keys(info).length) this.discovered.set(spec, info);
      out.push({ spec, contextWindow: m.contextWindow ?? KNOWN_MODELS[m.id]?.contextWindow, maxContext: m.maxContext });
    }
    return out;
  }
}
