import {
  Agent,
  buildSystemPrompt,
  listSessions,
  PERMISSION_MODES,
  type PermissionMode,
  PermissionPolicy,
  restore,
  SessionFile,
  sessionDir,
} from "@sasacode/agent";
import { BUILTIN_PROVIDERS, DEFAULT_MODEL, type ModelInfo, type ProviderConfig, resolveModel, type ThinkingLevel } from "@sasacode/ai";
import type { CommandDefinition, PluginAPI, ToolDefinition } from "@sasacode/plugin-api";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import builtinTools from "@sasacode/tools";
import { type Config, loadConfig, sasacodeHome } from "./config.ts";
import { resolveApiKey } from "./keys.ts";

export interface SetupOptions {
  cwd: string;
  model?: string;
  permission?: string;
  thinking?: string;
  maxTurns?: number;
  /** "last" = most recent session in cwd, otherwise a session id or path. */
  resume?: string;
  noSession?: boolean;
}

export interface Harness {
  agent: Agent;
  config: Config;
  warnings: string[];
  commands: CommandDefinition[];
  providers: Record<string, ProviderConfig>;
  resolve(spec: string): ModelInfo;
  sessionsDir: string;
  historyPath: string;
  /** Replace the agent's conversation with a stored session. */
  loadSession(path: string): void;
  newSession(): void;
}

export async function setup(opts: SetupOptions): Promise<Harness> {
  const { config, warnings } = loadConfig(opts.cwd);
  const providers: Record<string, ProviderConfig> = { ...BUILTIN_PROVIDERS };
  for (const [name, p] of Object.entries(config.providers ?? {}))
    providers[name] = { ...providers[name], ...p } as ProviderConfig;
  const resolve = (spec: string) => resolveModel(spec, providers, config.modelOverrides);

  // Everything, built-ins included, goes through the plugin API (P2).
  const tools: ToolDefinition<any>[] = [];
  const commands: CommandDefinition[] = [];
  const api: PluginAPI = {
    registerTool(t) {
      const i = tools.findIndex((x) => x.name === t.name);
      if (i >= 0) tools[i] = t;
      else tools.push(t);
    },
    registerCommand(c) {
      commands.push(c);
    },
  };
  await builtinTools(api);

  const mode = (opts.permission ?? config.permissions?.mode ?? "edits") as PermissionMode;
  if (!PERMISSION_MODES.includes(mode)) throw new Error(`unknown permission mode "${mode}" (${PERMISSION_MODES.join(", ")})`);
  const sessionsDir = sessionDir(sasacodeHome(), opts.cwd);
  mkdirSync(sasacodeHome(), { recursive: true });

  const agent = new Agent({
    model: resolve(opts.model ?? config.model ?? DEFAULT_MODEL),
    cwd: opts.cwd,
    systemPrompt: buildSystemPrompt({ cwd: opts.cwd, append: config.instructions ? [config.instructions] : [] }),
    tools,
    thinking: (opts.thinking ?? config.thinking ?? "high") as ThinkingLevel,
    permissions: new PermissionPolicy(mode, config.permissions),
    getApiKey: (p) => resolveApiKey(p, providers[p]),
    maxTurns: opts.maxTurns ?? config.maxTurns,
    maxRetries: config.maxRetries,
  });

  const harness: Harness = {
    agent,
    config,
    warnings,
    commands,
    providers,
    resolve,
    sessionsDir,
    historyPath: join(sasacodeHome(), "history.jsonl"),
    loadSession(path) {
      const { file, entries } = SessionFile.open(path);
      const state = restore(entries);
      agent.messages = state.messages;
      agent.session = file;
      if (state.model) {
        try {
          agent.model = resolve(state.model);
        } catch (e) {
          warnings.push(`session model ${state.model} unavailable: ${(e as Error).message}`);
        }
      }
    },
    newSession() {
      agent.messages = [];
      agent.session = opts.noSession ? undefined : SessionFile.create(sessionsDir, opts.cwd);
    },
  };

  if (opts.resume) {
    const list = listSessions(sessionsDir);
    const target =
      opts.resume === "last"
        ? list[0]
        : list.find((s) => s.id === opts.resume || s.path === opts.resume || s.id.startsWith(opts.resume!));
    if (!target) throw new Error(opts.resume === "last" ? "no previous session in this directory" : `session not found: ${opts.resume}`);
    harness.loadSession(target.path);
  } else harness.newSession();
  return harness;
}
