import { mkdirSync } from "node:fs";
import { join } from "node:path";
import {
  Agent,
  buildSystemPrompt,
  headlessUI,
  listSessions,
  PERMISSION_MODES,
  type PermissionMode,
  PermissionPolicy,
  PluginHost,
  restore,
  SessionFile,
  sessionDir,
  type UIBridge,
} from "@sasacode/agent";
import { BUILTIN_PROVIDERS, defaultModelFor, type ModelInfo, type ProviderConfig, registerApi, resolveModel, type ThinkingLevel } from "@sasacode/ai";
import { bundledPlugins } from "@sasacode/bundled";
import { createMcpPlugin, type McpServerConfig } from "@sasacode/mcp";
import { createSkillsPlugin } from "@sasacode/skills";
import builtinTools from "@sasacode/tools";
import { type Config, loadConfig, sasacodeHome } from "./config.ts";
import { discoverPlugins, type FoundPlugin, importExtensions, isTrusted, projectFingerprint, saveTrust } from "./loader.ts";
import { type ModelChoice, ModelCatalog } from "./catalog.ts";
import { resolveEndpoints } from "./endpoints.ts";
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
  /** Trust project-local plugins and MCP servers without asking (headless). */
  trustProject?: boolean;
}

export interface Harness {
  agent: Agent;
  host: PluginHost;
  config: Config;
  warnings: string[];
  providers: Record<string, ProviderConfig>;
  resolve(spec: string): ModelInfo;
  sessionsDir: string;
  historyPath: string;
  /** Models offered by the providers you have keys for (fetched once, then cached). */
  listModels(refresh?: boolean): Promise<ModelChoice[]>;
  /** Load bundled, MCP, Skills and third-party plugins, then start the session. Safe to call once. */
  loadPlugins(ui?: UIBridge): Promise<void>;
  /** Replace the agent's conversation with a stored session. */
  loadSession(path: string): Promise<void>;
  newSession(): Promise<void>;
  /** New session holding the conversation up to (not including) message `index`. */
  fork(index: number): Promise<void>;
  shutdown(): Promise<void>;
}

export async function setup(opts: SetupOptions): Promise<Harness> {
  const { config, warnings, projectMcp } = loadConfig(opts.cwd);
  const providers: Record<string, ProviderConfig> = { ...BUILTIN_PROVIDERS };
  for (const [name, p] of Object.entries(config.providers ?? {}))
    providers[name] = { ...providers[name], ...p } as ProviderConfig;
  // Custom endpoints configured with only a URL: detect OpenAI- or Anthropic-style (cached).
  await resolveEndpoints(providers, (p) => resolveApiKey(p, providers[p]), warnings);
  // What the Models API reported fills in context windows; explicit modelOverrides in config win.
  let catalog: ModelCatalog;
  const overrides = () => {
    const merged: Record<string, Partial<ModelInfo>> = Object.fromEntries(catalog?.discovered ?? []);
    for (const [k, v] of Object.entries(config.modelOverrides ?? {})) merged[k] = { ...merged[k], ...v };
    return merged;
  };
  const resolve = (spec: string) => resolveModel(spec, providers, overrides());

  const mode = (opts.permission ?? config.permissions?.mode ?? "edits") as PermissionMode;
  if (!PERMISSION_MODES.includes(mode)) throw new Error(`unknown permission mode "${mode}" (${PERMISSION_MODES.join(", ")})`);
  const home = sasacodeHome();
  const sessionsDir = sessionDir(home, opts.cwd);
  mkdirSync(home, { recursive: true });

  const agent = new Agent({
    model: resolve(opts.model ?? config.model ?? defaultModelFor(providers)),
    cwd: opts.cwd,
    systemPrompt: buildSystemPrompt({ cwd: opts.cwd, append: config.instructions ? [config.instructions] : [] }),
    thinking: (opts.thinking ?? config.thinking ?? "high") as ThinkingLevel,
    permissions: new PermissionPolicy(mode, config.permissions),
    getApiKey: (p) => resolveApiKey(p, providers[p]),
    resolveModel: resolve,
    maxTurns: opts.maxTurns ?? config.maxTurns,
    maxRetries: config.maxRetries,
    toolSearch: config.toolSearch,
  });

  catalog = new ModelCatalog(
    providers,
    (p) => resolveApiKey(p, providers[p]),
    () => agent.model.provider,
  );

  const disabled = new Set(config.plugins?.disabled ?? []);
  const host = new PluginHost({
    agent,
    cwd: opts.cwd,
    settings: (name) => config.plugins?.settings?.[name] ?? {},
    disabledTools: config.tools?.disabled,
    registerProvider(name, cfg, impl) {
      providers[name] = cfg;
      if (impl) registerApi(cfg.api, impl);
    },
  });
  // Built-in tools go through the same API as everything else (P2); they load first and synchronously.
  await host.load("builtin-tools", builtinTools);

  let sessionEntries: Parameters<PluginHost["setSessionEntries"]>[0] = [];
  const startSession = async (resumed: boolean) => {
    host.setSessionEntries(sessionEntries);
    await agent.hooks.run("session_start", { sessionId: agent.session?.id, resumed });
  };
  const endSession = () => agent.hooks.run("session_end", { sessionId: agent.session?.id });

  const openSession = (path: string) => {
    const { file, entries } = SessionFile.open(path);
    const state = restore(entries);
    agent.messages = state.messages;
    agent.session = file;
    sessionEntries = entries;
    if (state.model) {
      try {
        agent.model = resolve(state.model);
      } catch (e) {
        warnings.push(`session model ${state.model} unavailable: ${(e as Error).message}`);
      }
    }
  };
  const createSession = () => {
    agent.messages = [];
    agent.session = opts.noSession ? undefined : SessionFile.create(sessionsDir, opts.cwd);
    sessionEntries = [];
  };

  if (opts.resume) {
    const list = listSessions(sessionsDir);
    const target =
      opts.resume === "last"
        ? list[0]
        : list.find((s) => s.id === opts.resume || s.path === opts.resume || s.id.startsWith(opts.resume!));
    if (!target) throw new Error(opts.resume === "last" ? "no previous session in this directory" : `session not found: ${opts.resume}`);
    openSession(target.path);
  } else createSession();

  let loading: Promise<void> | undefined;
  const loadPlugins = (ui: UIBridge = headlessUI) =>
    (loading ??= (async () => {
      host.setUI(ui);
      for (const [name, plugin] of Object.entries(bundledPlugins)) if (!disabled.has(name)) await host.load(name, plugin);

      const found = discoverPlugins(opts.cwd).filter((p) => !disabled.has(p.manifest.name));
      const projectPlugins = found.filter((p) => p.scope === "project");
      const projectServers = Object.fromEntries(projectMcp.map((n) => [n, config.mcpServers![n]!]));
      let trusted = true;
      if (projectPlugins.length || projectMcp.length) {
        const fp = projectFingerprint(projectPlugins, projectServers as Record<string, McpServerConfig>);
        trusted = opts.trustProject || isTrusted(opts.cwd, fp);
        if (!trusted && ui.interactive) {
          const names = [...projectPlugins.map((p) => `plugin ${p.manifest.name}`), ...projectMcp.map((n) => `MCP ${n}`)];
          trusted = await ui.confirm(
            "このプロジェクトのプラグイン / MCP サーバーを信頼しますか？",
            `${names.join(", ")}\nインプロセスプラグインと MCP サーバーはあなたの権限でコードを実行します。`,
          );
          if (trusted) saveTrust(opts.cwd, fp);
        }
        if (!trusted) host.notify("プロジェクトのプラグイン / MCP サーバーは信頼されていないため読み込みません（headless では --trust-project）", "warning");
      }
      const usable = found.filter((p) => p.scope === "global" || trusted);

      if (!disabled.has("skills")) {
        const dirs = [join(home, "skills"), join(opts.cwd, ".sasacode", "skills"), ...usable.flatMap((p) => (p.manifest.skills ? [join(p.dir, p.manifest.skills)] : []))];
        await host.load("skills", createSkillsPlugin(dirs, { builtinDir: join(home, "builtin-skills") }));
      }
      if (!disabled.has("mcp")) {
        const servers: Record<string, McpServerConfig> = {};
        for (const [n, s] of Object.entries(config.mcpServers ?? {})) if (trusted || !projectMcp.includes(n)) servers[n] = s as McpServerConfig;
        for (const p of usable) Object.assign(servers, p.manifest.mcpServers ?? {});
        await host.load("mcp", createMcpPlugin(servers));
      }
      for (const p of usable) await loadExternal(host, p);
      await startSession(!!opts.resume);
    })());

  return {
    agent,
    host,
    config,
    warnings,
    providers,
    resolve,
    sessionsDir,
    historyPath: join(home, "history.jsonl"),
    async listModels(refresh) {
      const models = await catalog.list(refresh);
      // The current model may just have learned its real context window.
      const current = `${agent.model.provider}/${agent.model.id}`;
      if (catalog.discovered.has(current)) agent.model = resolve(current);
      return models;
    },
    loadPlugins,
    async loadSession(path) {
      await endSession();
      openSession(path);
      await startSession(true);
    },
    async newSession() {
      await endSession();
      createSession();
      await startSession(false);
    },
    async fork(index) {
      const kept = agent.messages.slice(0, index);
      await endSession();
      createSession();
      for (const m of kept) agent.session?.append({ type: "message", message: m });
      agent.messages = kept;
      await startSession(false);
    },
    async shutdown() {
      await endSession();
    },
  };
}

async function loadExternal(host: PluginHost, p: FoundPlugin): Promise<void> {
  let exts;
  try {
    exts = await importExtensions(p);
  } catch (e) {
    host.plugins.push({ name: p.manifest.name, error: (e as Error).message });
    host.notify(`plugin ${p.manifest.name} skipped: ${(e as Error).message}`, "warning");
    return;
  }
  for (const [i, fn] of exts.entries()) await host.load(exts.length > 1 ? `${p.manifest.name}#${i}` : p.manifest.name, fn);
}
