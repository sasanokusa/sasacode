// Custom endpoints (a llama.cpp server, vLLM, LM Studio, a proxy …): configured with just a URL,
// their API format is detected from the Models API and remembered.
import { join } from "node:path";
import { parseArgs } from "node:util";
import { t } from "@sasacode/host";
import { BUILTIN_PROVIDERS, type ProviderConfig } from "@sasacode/ai";
import { listModels } from "./list-models.ts";
import { applyDetected, type DetectedEndpoint, detectEndpoint } from "./detect.ts";
import { loadConfig, sasacodeHome } from "./config.ts";
import { readJson, readJsonOr, writeJson } from "./json.ts";

const cacheFile = () => join(sasacodeHome(), "endpoints.json");


/**
 * Give every provider configured without `api` its detected format, using the cached result
 * when there is one (so startup does not wait on the network). Unreachable ones are dropped
 * with a warning.
 */
export async function resolveEndpoints(
  providers: Record<string, Partial<ProviderConfig>>,
  getKey: (provider: string) => Promise<string | undefined>,
  warnings: string[],
): Promise<void> {
  const cache = readJsonOr<Record<string, DetectedEndpoint>>(cacheFile(), {}); // only a cache: detect again
  let changed = false;
  await Promise.all(
    Object.entries(providers).map(async ([name, p]) => {
      if (p.api) return;
      if (!p.baseUrl) {
        warnings.push(t('provider {name} has neither "api" nor "baseUrl"; ignored', { name }));
        delete providers[name];
        return;
      }
      let found = cache[p.baseUrl];
      if (!found) {
        try {
          found = await detectEndpoint(p.baseUrl, await getKey(name), AbortSignal.timeout(4000));
          cache[p.baseUrl] = found;
          changed = true;
        } catch (e) {
          warnings.push(t("provider {name} ({url}): {error}; ignored", { name, url: p.baseUrl, error: (e as Error).message }));
          delete providers[name];
          return;
        }
      }
      providers[name] = applyDetected(p, found);
    }),
  );
  if (changed) writeJson(cacheFile(), cache);
}

const configPath = (cwd: string, project: boolean) => (project ? join(cwd, ".sasacode", "config.json") : join(sasacodeHome(), "config.json"));

/** `sasacode endpoint add|list|remove` */
export async function endpointCommand(args: string[], cwd: string): Promise<number> {
  const { values, positionals } = parseArgs({ args, allowPositionals: true, options: { project: { type: "boolean" }, "key-env": { type: "string" } } });
  const project = !!values.project;
  const keyEnv = values["key-env"];
  const [sub, name, url] = positionals;
  const path = configPath(cwd, project);

  if (sub === "add" && name && url) {
    if (!/^[\w.-]+$/.test(name)) throw new Error(t("name may contain letters, digits, _ . - only"));
    if (BUILTIN_PROVIDERS[name]) throw new Error(t('"{name}" is a built-in provider; choose another name', { name }));
    const config = readJson(path); // before detecting: a config that cannot be read stops here
    const key = keyEnv ? process.env[keyEnv] : undefined;
    if (keyEnv && !key) console.error(t("warning: {keyEnv} is not set; detecting without a key", { keyEnv }));
    const found = await detectEndpoint(url, key, AbortSignal.timeout(10_000));
    const provider: Partial<ProviderConfig> = { ...applyDetected({}, found), ...(keyEnv ? { apiKeyEnv: keyEnv } : {}) };
    const flavour = found.llamacpp ? " (llama.cpp)" : found.ollama ? " (Ollama)" : "";
    console.log(
      t("{name}: {api}{flavour} at {url}", {
        name,
        api: found.api === "anthropic" ? "Anthropic Messages" : "OpenAI Chat Completions",
        flavour,
        url: found.baseUrl,
      }),
    );
    const models = await listModels(provider as ProviderConfig, key, AbortSignal.timeout(10_000)).catch(() => []);
    for (const m of models.slice(0, 20))
      console.log(`  ${name}/${m.id}${m.contextWindow ? `  ${t("({ctx} ctx)", { ctx: m.contextWindow })}` : m.maxContext ? `  ${t("(max {max})", { max: m.maxContext })}` : ""}`);
    if (models.length > 20) console.log(`  ${t("… {n} more (see /model)", { n: models.length - 20 })}`);
    config.providers = { ...config.providers, [name]: provider };
    writeJson(path, config);
    console.log(`${t("saved to {path}", { path })}${models[0] ? ` — ${t("try: sasacode -m {spec}", { spec: `${name}/${models[0].id}` })}` : ""}`);
    if (project) console.log(t("note: endpoints in a project config are used once you trust the project (sasacode asks on the next start)"));
    return 0;
  }
  if (sub === "remove" && name) {
    const config = readJson(path);
    if (!config.providers?.[name]) {
      console.error(t("{name} is not in {path}", { name, path }));
      return 1;
    }
    delete config.providers[name];
    writeJson(path, config);
    console.log(t("removed {name} from {path}", { name, path }));
    return 0;
  }
  if (sub === "list" || !sub) {
    const { config } = loadConfig(cwd);
    const custom = Object.entries(config.providers ?? {}).filter(([n]) => !BUILTIN_PROVIDERS[n]);
    if (!custom.length) console.log(t("no custom endpoints (add one: sasacode endpoint add <name> <url>)"));
    for (const [n, p] of custom) console.log(`${n}  ${p.baseUrl ?? "-"}  ${p.api ?? "auto"}${p.apiKeyEnv ? `  ${t("key: {env}", { env: p.apiKeyEnv })}` : ""}`);
    return 0;
  }
  console.error(t("usage: sasacode endpoint <add <name> <url> [--key-env VAR]|remove <name>|list> [--project]"));
  return 1;
}
