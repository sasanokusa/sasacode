// Custom endpoints (a llama.cpp server, vLLM, LM Studio, a proxy …): configured with just a URL,
// their API format is detected from the Models API and remembered.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { applyDetected, BUILTIN_PROVIDERS, type DetectedEndpoint, detectEndpoint, listModels, type ProviderConfig } from "@sasacode/ai";
import { loadConfig, sasacodeHome } from "./config.ts";

const cacheFile = () => join(sasacodeHome(), "endpoints.json");

function readJson(path: string): Record<string, any> {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch {
    return {};
  }
}

function writeJson(path: string, value: unknown): void {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
}

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
  const cache = readJson(cacheFile()) as Record<string, DetectedEndpoint>;
  let changed = false;
  await Promise.all(
    Object.entries(providers).map(async ([name, p]) => {
      if (p.api) return;
      if (!p.baseUrl) {
        warnings.push(`provider ${name} has neither "api" nor "baseUrl"; ignored`);
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
          warnings.push(`provider ${name} (${p.baseUrl}): ${(e as Error).message}; ignored`);
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
  const project = args.includes("--project");
  const keyAt = args.indexOf("--key-env");
  const keyEnv = keyAt >= 0 ? args[keyAt + 1] : undefined;
  const [sub, name, url] = args.filter((a, i) => !a.startsWith("--") && (keyAt < 0 || i !== keyAt + 1));
  const path = configPath(cwd, project);

  if (sub === "add" && name && url) {
    if (!/^[\w.-]+$/.test(name)) throw new Error("name may contain letters, digits, _ . - only");
    if (BUILTIN_PROVIDERS[name]) throw new Error(`"${name}" is a built-in provider; choose another name`);
    const key = keyEnv ? process.env[keyEnv] : undefined;
    if (keyEnv && !key) console.error(`warning: ${keyEnv} is not set; detecting without a key`);
    const found = await detectEndpoint(url, key, AbortSignal.timeout(10_000));
    const provider: Partial<ProviderConfig> = { ...applyDetected({}, found), ...(keyEnv ? { apiKeyEnv: keyEnv } : {}) };
    const flavour = found.llamacpp ? " (llama.cpp)" : found.ollama ? " (Ollama)" : "";
    console.log(`${name}: ${found.api === "anthropic" ? "Anthropic Messages" : "OpenAI Chat Completions"}${flavour} at ${found.baseUrl}`);
    const models = await listModels(provider as ProviderConfig, key, AbortSignal.timeout(10_000)).catch(() => []);
    for (const m of models.slice(0, 20))
      console.log(`  ${name}/${m.id}${m.contextWindow ? `  (${m.contextWindow} ctx)` : m.maxContext ? `  (max ${m.maxContext})` : ""}`);
    if (models.length > 20) console.log(`  … ${models.length - 20} more (see /model)`);
    const config = readJson(path);
    config.providers = { ...config.providers, [name]: provider };
    writeJson(path, config);
    console.log(`saved to ${path}${models[0] ? ` — try: sasacode -m ${name}/${models[0].id}` : ""}`);
    if (project) console.log("note: endpoints in a project config are used once you trust the project (sasacode asks on the next start)");
    return 0;
  }
  if (sub === "remove" && name) {
    const config = readJson(path);
    if (!config.providers?.[name]) {
      console.error(`${name} is not in ${path}`);
      return 1;
    }
    delete config.providers[name];
    writeJson(path, config);
    console.log(`removed ${name} from ${path}`);
    return 0;
  }
  if (sub === "list" || !sub) {
    const { config } = loadConfig(cwd);
    const custom = Object.entries(config.providers ?? {}).filter(([n]) => !BUILTIN_PROVIDERS[n]);
    if (!custom.length) console.log("no custom endpoints (add one: sasacode endpoint add <name> <url>)");
    for (const [n, p] of custom) console.log(`${n}  ${p.baseUrl ?? "-"}  ${p.api ?? "auto"}${p.apiKeyEnv ? `  key: ${p.apiKeyEnv}` : ""}`);
    return 0;
  }
  console.error("usage: sasacode endpoint <add <name> <url> [--key-env VAR]|remove <name>|list> [--project]");
  return 1;
}
