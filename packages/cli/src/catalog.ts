import { getProvider, type ModelInfo, type Provider, type ProviderConfig } from "@sasacode/ai";
import { BUILTIN_PROVIDERS, DEFAULT_CONTEXT_WINDOW, KNOWN_MODELS } from "@sasacode/host";
import { API_ENDPOINT, type ListedModel, listModels, lmstudioList } from "./list-models.ts";

export interface ModelChoice {
  /** "<provider>/<model>" */
  spec: string;
  /** Effective context window: from the provider, else sasacode's table of known models. */
  contextWindow?: number;
  /** The model's maximum, shown when the effective size is unknown (Ollama without num_ctx, LM Studio before loading). */
  maxContext?: number;
  /** The local server it runs on: why the size may be unknown, and what to do about it. */
  server?: "ollama" | "lmstudio" | "llamacpp";
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

/**
 * A custom OpenAI-style server not told apart. LM Studio set up before sasacode knew it is one
 * (`endpoint add` wrote no flag, a URL-only entry was cached without one) until it is listed (#59).
 */
export function untold(name: string, p: ProviderConfig | undefined): boolean {
  return p?.api === "openai-chat" && !!p.baseUrl && !BUILTIN_PROVIDERS[name] && !p.ollama && !p.llamacpp && !p.lmstudio;
}

/** Models the configured providers offer, fetched from their Models API (GET /v1/models). */
export class ModelCatalog {
  /** Per spec, what the provider told us (context window, max output, price). */
  readonly discovered = new Map<string, Partial<ModelInfo>>();
  /** Each group's listing, asked once (again on refresh). */
  private lists = new Map<string, Promise<ModelChoice[]>>();

  constructor(
    private providers: Record<string, ProviderConfig>,
    private getKey: (provider: string) => Promise<string | undefined>,
    /** Also asked without a key (it may authenticate another way, e.g. an `ant auth login` profile). */
    private currentProvider: () => string,
  ) {}

  /** Cached after the first call; `refresh` fetches again. */
  async list(refresh = false): Promise<ModelChoice[]> {
    return (await Promise.all(groups(this.providers).map((g) => this.listGroup(g, refresh)))).flat();
  }

  /** What the provider of `spec` lists for it; `refresh` asks that provider again, and no other. */
  async entry(spec: string, refresh = false): Promise<ModelChoice | undefined> {
    const group = this.groupOf(spec);
    return group && (await this.listGroup(group, refresh)).find((m) => m.spec === spec);
  }

  private groupOf(spec: string): string[] | undefined {
    const provider = spec.slice(0, spec.indexOf("/"));
    return groups(this.providers).find((g) => g.includes(provider));
  }

  private listGroup(members: string[], refresh = false): Promise<ModelChoice[]> {
    const key = members.join(" ");
    const last = this.lists.get(key);
    if (!refresh && last) return last;
    // A refresh that fails keeps what was listed before: the server's models stay in /model.
    const listed = this.fetchGroup(members).catch(() => last ?? []);
    this.lists.set(key, listed);
    return listed;
  }

  /**
   * Whether the provider of `spec` lists that model, and if not, the listed names most like it.
   * Undefined when it cannot be told: no Models API, offline, or an empty list.
   */
  async offers(spec: string): Promise<{ ok: boolean; similar: string[] } | undefined> {
    const id = spec.slice(spec.indexOf("/") + 1);
    const group = this.groupOf(spec);
    const listed = group ? await this.listGroup(group) : [];
    if (!listed.length) return undefined;
    const ids = listed.map((m) => m.spec.slice(m.spec.indexOf("/") + 1));
    return ids.includes(id) ? { ok: true, similar: [] } : { ok: false, similar: similar(id, ids) };
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
    // Asked when listed, not at startup: a server that is down delays nothing, and LM Studio with no
    // model downloaded when it was detected is still recognised later. The flag stays on the
    // provider for the session, so the turn_end refresh asks it too. The list that told it apart
    // is the one the sizes come from.
    const native = untold(primary, p) ? await lmstudioList(p, key, signal) : undefined;
    if (native) p.lmstudio = true;
    const models: ListedModel[] = impl?.listModels ? await impl.listModels(p, key, signal) : await listModels(p, key, signal, native);
    const server = p.ollama ? "ollama" : p.lmstudio ? "lmstudio" : p.llamacpp ? "llamacpp" : undefined;
    const out: ModelChoice[] = [];
    for (const m of models) {
      const target = m.endpoints
        ? members.find((n) => m.endpoints!.includes(API_ENDPOINT[this.providers[n]!.api] ?? ""))
        : primary;
      if (!target) continue; // e.g. an endpoint sasacode does not speak
      const spec = `${target}/${m.id}`;
      const info: Partial<ModelInfo> = {};
      if (m.contextWindow) info.contextWindow = m.contextWindow;
      // Only the maximum known (not loaded yet): no request gets more, so one below the default is
      // planned with, though the size stays unknown. A size learned before is the better guess.
      else if (m.maxContext && m.maxContext < (KNOWN_MODELS[m.id]?.contextWindow ?? DEFAULT_CONTEXT_WINDOW) && !this.discovered.get(spec)?.contextWindow)
        info.contextWindow = m.maxContext;
      if (m.maxOutput) info.maxOutput = m.maxOutput;
      // A listing that reports an all-zero price is saying "unknown", not free: keep it from
      // shadowing a price the built-in table knows.
      if (m.price && (m.price.input > 0 || m.price.output > 0)) info.price = m.price;
      if (Object.keys(info).length) this.discovered.set(spec, info);
      out.push({ spec, contextWindow: m.contextWindow ?? KNOWN_MODELS[m.id]?.contextWindow, maxContext: m.maxContext, ...(server ? { server } : {}) });
    }
    return out;
  }
}

/** Listed names sharing the most words with `id` (a typo'd "deepseek-v4-flsh" still finds "deepseek-v4-flash"). */
function similar(id: string, ids: string[]): string[] {
  const words = id.toLowerCase().split(/[^a-z0-9.]+/).filter((w) => w.length > 1);
  return ids
    .map((c) => ({ c, n: words.filter((w) => c.toLowerCase().includes(w)).length }))
    .filter((x) => x.n > 0)
    .sort((a, b) => b.n - a.n)
    .slice(0, 5)
    .map((x) => x.c);
}
