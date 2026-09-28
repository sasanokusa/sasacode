import { join } from "node:path";
import { PERMISSION_MODES, type PermissionMode, type PermissionRules, type ToolSearchConfig } from "@sasacode/agent";
import { t } from "@sasacode/host";
import { BUILTIN_PROVIDERS, type ModelInfo, type ProviderConfig, type ThinkingLevel } from "@sasacode/ai";
import { sasacodeHome } from "@sasacode/plugin-api";
import { readJson } from "./json.ts";

export interface Config {
  model?: string;
  /** The language the interface speaks: "ja" or "en" (default: SASACODE_LANG, else the system locale). */
  lang?: "ja" | "en";
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
  /** Look for a newer release at startup and mention it quietly (default true). */
  updateCheck?: boolean;
  /** Projects trusted without asking (global config only). */
  trustedProjects?: string[];
  plugins?: {
    /** Plugin names not to load (bundled ones included: tool-repair, repetition-guard, agents-md, compaction, subagent, todo, web-fetch, permission-presets, browsr, openai-codex, usage, goal, background-sessions, auto-reconnect, skills, mcp). */
    disabled?: string[];
    /** Per-plugin settings, handed to the plugin as api.settings. */
    settings?: Record<string, Record<string, unknown>>;
  };
  tools?: { disabled?: string[] };
  mcpServers?: Record<string, McpServerConfig>;
  toolSearch?: ToolSearchConfig;
  tui?: {
    /** Full-screen transcript that disappears on exit, leaving a summary (default true). */
    altScreen?: boolean;
    /** Ring the terminal bell when a run that took a while finishes (default true). */
    bell?: boolean;
  };
}

export type McpServerConfig =
  | { command: string; args?: string[]; env?: Record<string, string>; cwd?: string; disabled?: boolean; alwaysLoad?: boolean }
  | { url: string; headers?: Record<string, string>; disabled?: boolean; alwaysLoad?: boolean };

export { sasacodeHome };

function isObject(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === "object" && !Array.isArray(v);
}

const isString = (v: unknown) => typeof v === "string";
const isCount = (max: number) => (v: unknown) => Number.isInteger(v) && (v as number) >= 0 && (v as number) <= max;
const isStrings = (v: unknown) => Array.isArray(v) && v.every(isString);
const isObjectOfObjects = (v: unknown) => isObject(v) && Object.values(v).every(isObject);
const THINKING = ["off", "low", "medium", "high", "xhigh", "max"];

/** Shape checks per key; a value that fails is dropped with a warning instead of silently merging. */
const CHECKS: Record<keyof Config, (v: unknown) => boolean> = {
  model: isString,
  lang: (v) => v === "ja" || v === "en",
  models: isStrings,
  thinking: (v) => THINKING.includes(v as string),
  providers: isObjectOfObjects,
  modelOverrides: isObjectOfObjects,
  permissions: (v) =>
    isObject(v) &&
    ["allow", "ask", "deny", "softDeny"].every((k) => v[k] === undefined || isStrings(v[k])) &&
    (v.mode === undefined || PERMISSION_MODES.includes(v.mode as PermissionMode)),
  instructions: isString,
  maxTurns: isCount(100_000),
  maxRetries: isCount(20),
  updateCheck: (v) => typeof v === "boolean",
  trustedProjects: isStrings,
  plugins: (v) => isObject(v) && (v.disabled === undefined || isStrings(v.disabled)) && (v.settings === undefined || isObjectOfObjects(v.settings)),
  tools: (v) => isObject(v) && (v.disabled === undefined || isStrings(v.disabled)),
  mcpServers: isObjectOfObjects,
  toolSearch: isObject,
  tui: (v) => isObject(v) && ["altScreen", "bell"].every((k) => v[k] === undefined || typeof v[k] === "boolean"),
};

export function sanitizeConfig(raw: unknown, source: string, warnings: string[]): Config {
  if (!isObject(raw)) {
    warnings.push(t("{source}: not a JSON object; ignored", { source }));
    return {};
  }
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(raw)) {
    const check = CHECKS[k as keyof Config];
    if (!check) warnings.push(t('{source}: unknown key "{key}" ignored', { source, key: k }));
    else if (!check(v)) warnings.push(t('{source}: "{key}" has an invalid value; ignored', { source, key: k }));
    else out[k] = v;
  }
  return out as Config;
}

/** Lists that both levels add to. Anything else named "disabled" (mcpServers.<name>.disabled) is a plain value. */
const ACCUMULATE = ["permissions.allow", "permissions.ask", "permissions.deny", "permissions.softDeny", "tools.disabled", "plugins.disabled"];

/**
 * Later values win; objects merge recursively. Permission rules and disabled lists accumulate
 * (never replace), so a project cannot drop the user's deny rules or re-enable what they turned off.
 */
export function mergeConfig(base: Config, over: Config): Config {
  const merge = (a: any, b: any, path = ""): any => {
    if (ACCUMULATE.includes(path) && Array.isArray(a) && Array.isArray(b)) return [...new Set([...a, ...b])];
    if (isObject(a) && isObject(b)) {
      const out: Record<string, unknown> = { ...a };
      for (const [k, v] of Object.entries(b)) out[k] = k in a ? merge(a[k], v, path ? `${path}.${k}` : k) : v;
      return out;
    }
    return b === undefined ? a : b;
  };
  return merge(base, over);
}

/** Turns per run when none is configured: long tasks fit, a runaway loop stops. 0 means no limit. */
export const DEFAULT_MAX_TURNS = 200;
/** The SDKs' retry count when none is configured. */
const DEFAULT_RETRIES = 8;

/** How much a mode restricts: a project may only move to an equal or stricter one without trust. */
const MODE_STRENGTH: Record<PermissionMode, number> = { auto: 0, agent: 1, edits: 2, ask: 3 };

/**
 * Split a project's config into what it may set on its own and what needs the user's trust.
 * Without trust a project can only tighten things: it cannot add endpoints (even keyless ones
 * would receive your code), change model metadata (baseUrl could reroute keys), configure or
 * disable plugins, start MCP servers, add allow rules, loosen the permission mode, or raise the
 * turn and retry limits.
 */
const ROUTERS = ["openrouter"];

export function splitProjectConfig(project: Config, global: Config): { safe: Config; elevated: Config } {
  const safe: Config = {};
  const elevated: Config = {};
  // A provider the user set up, with one exception: a router (OpenRouter) reaches many companies
  // with one key, so which model it is decides where the conversation and code go.
  const knownProvider = (spec: string) => {
    const name = spec.slice(0, spec.indexOf("/"));
    if (ROUTERS.includes(name)) return false;
    return !!BUILTIN_PROVIDERS[name] || !!global.providers?.[name];
  };
  // tools.disabled only adds to the user's list (see mergeConfig).
  for (const key of ["thinking", "instructions", "toolSearch", "tools", "tui"] as const) if (project[key] !== undefined) (safe as any)[key] = project[key];
  // 0 turns means no limit, so it is the loosest value.
  const turns = (n: number | undefined) => (n ? n : Infinity);
  if (project.maxTurns !== undefined) (turns(project.maxTurns) <= turns(global.maxTurns ?? DEFAULT_MAX_TURNS) ? safe : elevated).maxTurns = project.maxTurns;
  if (project.maxRetries !== undefined)
    (project.maxRetries <= (global.maxRetries ?? DEFAULT_RETRIES) ? safe : elevated).maxRetries = project.maxRetries;
  if (project.model !== undefined) (knownProvider(project.model) ? safe : elevated).model = project.model;
  if (project.models) {
    const [ok, rest] = [project.models.filter(knownProvider), project.models.filter((m) => !knownProvider(m))];
    if (ok.length) safe.models = ok;
    if (rest.length) elevated.models = rest;
  }
  if (project.permissions) {
    const { allow, ask, deny, softDeny, mode } = project.permissions;
    safe.permissions = { ...(ask ? { ask } : {}), ...(deny ? { deny } : {}), ...(softDeny ? { softDeny } : {}) };
    if (allow?.length) elevated.permissions = { allow };
    if (mode) {
      const floor = global.permissions?.mode ?? "edits";
      if (MODE_STRENGTH[mode] >= MODE_STRENGTH[floor]) safe.permissions.mode = mode;
      else elevated.permissions = { ...elevated.permissions, mode };
    }
  }
  for (const key of ["providers", "modelOverrides", "plugins", "mcpServers"] as const)
    if (project[key] !== undefined) (elevated as any)[key] = project[key];
  return { safe, elevated };
}

/** One line per thing the trust prompt covers. */
export function describeElevated(e: Config): string[] {
  const out: string[] = [];
  for (const [n, p] of Object.entries(e.providers ?? {})) out.push(t("endpoint {name} ({url})", { name: n, url: p.baseUrl ?? p.api ?? "?" }));
  if (e.model) out.push(t("model {model}", { model: e.model }));
  if (e.models?.length) out.push(t("models {models}", { models: e.models.join(", ") }));
  if (e.modelOverrides) out.push(t("model overrides for {models}", { models: Object.keys(e.modelOverrides).join(", ") }));
  for (const n of Object.keys(e.mcpServers ?? {})) out.push(t("MCP server {name}", { name: n }));
  if (e.plugins?.disabled?.length) out.push(t("disables plugins {names}", { names: e.plugins.disabled.join(", ") }));
  if (e.plugins?.settings) out.push(t("settings for plugins {names}", { names: Object.keys(e.plugins.settings).join(", ") }));
  if (e.permissions?.allow?.length) out.push(t("allow rules {rules}", { rules: e.permissions.allow.join(", ") }));
  if (e.permissions?.mode) out.push(t("permission mode {mode}", { mode: e.permissions.mode }));
  if (e.maxTurns !== undefined) out.push(e.maxTurns ? `maxTurns ${e.maxTurns}` : t("maxTurns unlimited"));
  if (e.maxRetries !== undefined) out.push(`maxRetries ${e.maxRetries}`);
  return out;
}

export interface LoadedConfig {
  config: Config;
  warnings: string[];
  /** Project settings that need trust (applied only when `trusted`). */
  elevated: Config;
  global: Config;
}

/** The two config files as read, before trust decides what of the project's applies. */
export interface ConfigFiles {
  global: Config;
  project: Config;
  warnings: string[];
}

export function readConfigFiles(cwd: string): ConfigFiles {
  const warnings: string[] = [];
  const globalPath = join(sasacodeHome(), "config.json");
  const global = sanitizeConfig(readJson(globalPath), globalPath, warnings);
  const project = sanitizeConfig(readJson(join(cwd, ".sasacode", "config.json")), ".sasacode/config.json", warnings);
  if (project.trustedProjects) {
    warnings.push(t(".sasacode/config.json: trustedProjects is only read from the global config; ignored"));
    delete project.trustedProjects;
  }
  return { global, project, warnings };
}

/** `files`: already read (startup reads them once for the trust check and for setup). */
export function loadConfig(cwd: string, trusted = false, files: ConfigFiles = readConfigFiles(cwd)): LoadedConfig {
  const { global, project } = files;
  const warnings = [...files.warnings];
  const { safe, elevated } = splitProjectConfig(project, global);
  const items = describeElevated(elevated);
  // Allowed without trust, but the user should know the project, not they, picked the model.
  if (!trusted && safe.model && safe.model !== global.model) warnings.push(t(".sasacode/config.json: this project selects the model {model}", { model: safe.model }));
  if (!trusted && items.length)
    warnings.push(t(".sasacode/config.json: not applied until you trust this project ({items})", { items: items.join("; ") }));
  return { config: mergeConfig(global, trusted ? project : safe), warnings, elevated, global };
}
