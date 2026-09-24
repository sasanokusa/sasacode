import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import type { PermissionMode, PermissionRules, ToolSearchConfig } from "@sasacode/agent";
import { BUILTIN_PROVIDERS, type ModelInfo, type ProviderConfig, type ThinkingLevel } from "@sasacode/ai";

export interface Config {
  model?: string;
  /** Models offered by /model. */
  models?: string[];
  thinking?: ThinkingLevel;
  /** Extra or overridden providers. Without `api`, the format is detected from `baseUrl`. */
  providers?: Record<string, Partial<ProviderConfig>>;
  /** Per-model metadata overrides keyed by "provider/model" (contextWindow, price, ...). */
  modelOverrides?: Record<string, Partial<ModelInfo>>;
  permissions?: PermissionRules & { mode?: PermissionMode };
  /** Extra text appended to the system prompt. */
  instructions?: string;
  maxTurns?: number;
  maxRetries?: number;
  /** Projects whose .sasacode/config.json may loosen permissions. */
  trustedProjects?: string[];
  plugins?: {
    /** Plugin names not to load (bundled ones included: tool-repair, repetition-guard, agents-md, compaction, subagent, todo, web-fetch, permission-presets, browsr, skills, mcp). */
    disabled?: string[];
    /** Per-plugin settings, handed to the plugin as api.settings. */
    settings?: Record<string, Record<string, unknown>>;
  };
  tools?: { disabled?: string[] };
  mcpServers?: Record<string, McpServerConfig>;
  toolSearch?: ToolSearchConfig;
}

export type McpServerConfig =
  | { command: string; args?: string[]; env?: Record<string, string>; cwd?: string; disabled?: boolean; alwaysLoad?: boolean }
  | { url: string; headers?: Record<string, string>; disabled?: boolean; alwaysLoad?: boolean };

export function sasacodeHome(): string {
  return process.env.SASACODE_HOME ?? join(homedir(), ".sasacode");
}

function readJson(path: string): Config {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return {};
    throw new Error(`failed to read ${path}: ${(e as Error).message}`);
  }
}

function isObject(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === "object" && !Array.isArray(v);
}

/** Project values win; objects merge recursively; permission rule lists concatenate. */
export function mergeConfig(base: Config, over: Config): Config {
  const merge = (a: any, b: any, key = ""): any => {
    if (Array.isArray(a) && Array.isArray(b) && ["allow", "ask", "deny"].includes(key)) return [...a, ...b];
    if (isObject(a) && isObject(b)) {
      const out: Record<string, unknown> = { ...a };
      for (const [k, v] of Object.entries(b)) out[k] = k in a ? merge(a[k], v, k) : v;
      return out;
    }
    return b === undefined ? a : b;
  };
  return merge(base, over);
}

export interface LoadedConfig {
  config: Config;
  warnings: string[];
}

export function loadConfig(cwd: string): LoadedConfig & { projectMcp: string[] } {
  const global = readJson(join(sasacodeHome(), "config.json"));
  const project = readJson(join(cwd, ".sasacode", "config.json"));
  // MCP servers defined by the project run code on this machine, so they need the trust prompt.
  const projectMcp = Object.keys(project.mcpServers ?? {});
  const warnings: string[] = [];
  // A cloned repo must not be able to switch off approvals by itself.
  if (!global.trustedProjects?.includes(cwd) && project.permissions) {
    const { mode, allow, ...rest } = project.permissions;
    if (mode === "auto" || mode === "agent" || allow?.length) {
      warnings.push(
        `.sasacode/config.json loosens permissions (${[mode && `mode ${mode}`, allow?.length && "allow rules"].filter(Boolean).join(", ")}); ignored until "${cwd}" is added to trustedProjects in ${join(sasacodeHome(), "config.json")}.`,
      );
      project.permissions = { ...rest, ...(mode === "ask" || mode === "edits" ? { mode } : {}) };
    }
  }
  // Nor may it send the user's API keys somewhere: it can add keyless providers (a local server)
  // but not redefine existing ones or attach a key or headers.
  if (!global.trustedProjects?.includes(cwd) && project.providers) {
    const kept: typeof project.providers = {};
    for (const [name, p] of Object.entries(project.providers)) {
      if (BUILTIN_PROVIDERS[name] || global.providers?.[name] || p.apiKeyEnv || p.headers)
        warnings.push(
          `.sasacode/config.json provider "${name}" redefines a provider or uses an API key / headers; ignored until "${cwd}" is added to trustedProjects in ${join(sasacodeHome(), "config.json")}.`,
        );
      else kept[name] = p;
    }
    project.providers = kept;
  }
  return { config: mergeConfig(global, project), warnings, projectMcp };
}
