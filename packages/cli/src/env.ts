import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import type { ProviderConfig } from "@sasacode/ai";

/** Key variables of the built-in providers, each once, in provider order. */
export function keyVariables(providers: Record<string, ProviderConfig>): { name: string; label: string; example?: string }[] {
  const seen = new Map<string, { name: string; label: string; example?: string }>();
  for (const [provider, p] of Object.entries(providers)) {
    if (!p.apiKeyEnv || !p.label || seen.has(p.apiKeyEnv)) continue;
    seen.set(p.apiKeyEnv, { name: p.apiKeyEnv, label: p.label, example: p.defaultModel && `${provider}/${p.defaultModel}` });
  }
  return [...seen.values()];
}

/** Create the env file with an empty line per provider key, unless it exists. Owner-only permissions. */
export function ensureEnvFile(path: string, providers: Record<string, ProviderConfig>): boolean {
  if (existsSync(path)) return false;
  const lines = [
    "# sasacode API keys. Fill in the providers you use; blank entries are ignored.",
    "# Read on every start, from any directory. A project's ./.env is read as well.",
    "",
  ];
  for (const k of keyVariables(providers)) lines.push(`# ${k.label}${k.example ? ` (e.g. -m ${k.example})` : ""}`, `${k.name}=`, "");
  lines.push("# Ollama (ollama/<model>) runs locally and needs no key.", "");
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, lines.join("\n"), { mode: 0o600 });
  return true;
}

/** KEY=value lines; blank values are skipped so an empty entry means "not configured". Existing vars win. */
export function loadEnvFile(path: string): void {
  let text: string;
  try {
    text = readFileSync(path, "utf8");
  } catch {
    return;
  }
  for (const line of text.split("\n")) {
    const m = /^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$/.exec(line);
    if (!m) continue;
    const value = m[2]!.replace(/^(["'])(.*)\1$/, "$2");
    if (value && !process.env[m[1]!]) process.env[m[1]!] = value;
  }
}

/** Bun loads ./.env itself and keeps empty values; an empty key must not shadow a real one (or an SDK login). */
export function dropBlankKeys(providers: Record<string, ProviderConfig>): void {
  for (const k of keyVariables(providers)) if (process.env[k.name] !== undefined && !process.env[k.name]!.trim()) delete process.env[k.name];
}
