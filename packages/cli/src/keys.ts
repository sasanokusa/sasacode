import type { ProviderConfig } from "@sasacode/ai";

const cache = new Map<string, string | undefined>();

/** API key from the provider's env var, else the OS keychain (service "sasacode", account = provider). */
export async function resolveApiKey(provider: string, cfg: ProviderConfig | undefined): Promise<string | undefined> {
  if (cfg?.apiKeyEnv && process.env[cfg.apiKeyEnv]) return process.env[cfg.apiKeyEnv];
  if (cache.has(provider)) return cache.get(provider);
  const key = await keychain(provider);
  cache.set(provider, key);
  return key;
}

async function keychain(account: string): Promise<string | undefined> {
  const cmd =
    process.platform === "darwin"
      ? ["security", "find-generic-password", "-s", "sasacode", "-a", account, "-w"]
      : process.platform === "linux"
        ? ["secret-tool", "lookup", "service", "sasacode", "account", account]
        : undefined;
  if (!cmd) return;
  try {
    const p = Bun.spawn(cmd, { stdout: "pipe", stderr: "ignore" });
    const out = (await new Response(p.stdout).text()).trim();
    return (await p.exited) === 0 && out ? out : undefined;
  } catch {
    return;
  }
}
