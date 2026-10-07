import { chmodSync, mkdirSync, readFileSync, statSync } from "node:fs";
import { join, resolve as resolvePath } from "node:path";
import { Agent, buildSystemPrompt, headlessUI, isInside, PERMISSION_MODES, type PermissionMode, PermissionPolicy, PluginHost, restore, SessionFile, sessionDir, type UIBridge } from "@sasacode/agent";
import { currentLang, findSession, type Lang, listSessions, t } from "@sasacode/host";
import { BUILTIN_PROVIDERS, defaultModelFor, type ModelInfo, type ProviderConfig, registerApi, resolveModel, type ThinkingLevel } from "@sasacode/ai";
import { bundledPlugins, readCodexAuth } from "@sasacode/bundled";
import { createMcpPlugin, type McpServerConfig } from "@sasacode/mcp";
import { createSkillsPlugin } from "@sasacode/skills";
import builtinTools from "@sasacode/tools";
import { type Config, type ConfigFiles, DEFAULT_MAX_TURNS, loadConfig, sasacodeHome } from "./config.ts";
import { discoverPlugins, type FoundPlugin, importExtensions } from "./loader.ts";
import { type ModelChoice, ModelCatalog } from "./catalog.ts";
import { resolveEndpoints } from "./endpoints.ts";
import { resolveApiKey } from "./keys.ts";
import { readJson, writeJson } from "./json.ts";

/**
 * The first provider whose API key is set; with no key at all but a ChatGPT login, that plan.
 */
function defaultModel(providers: Record<string, ProviderConfig>, codex: boolean): string {
  const anyKey = Object.values(providers).some((p) => p.apiKeyEnv && process.env[p.apiKeyEnv]?.trim());
  if (!anyKey && codex && readCodexAuth()) return `openai-codex/${providers["openai-codex"]?.defaultModel ?? "gpt-5.5"}`;
  return defaultModelFor(providers);
}

export interface SetupOptions {
  cwd: string;
  model?: string;
  permission?: string;
  thinking?: string;
  maxTurns?: number;
  /** "last" = most recent session in cwd, otherwise a session id or path. */
  resume?: string;
  noSession?: boolean;
  /** Text added to the system prompt after `config.instructions` (--append-system-prompt-file). */
  appendSystemPrompt?: string;
  /** The project is trusted: its plugins and the elevated part of its config are used. */
  trustProject?: boolean;
  /** Config files and plugins already read for the trust check (see assessProject). */
  preloaded?: { files: ConfigFiles; plugins: FoundPlugin[]; warnings: string[] };
}

/** --append-system-prompt-file: the file's text as UTF-8; a file that cannot be read is an error, not a silent skip. */
export function readPromptFile(path: string): string {
  try {
    return readFileSync(path, "utf8");
  } catch (e) {
    throw new Error(t("failed to read {path}: {error}", { path, error: e instanceof Error ? e.message : String(e) }));
  }
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
  /** /language: the `lang` setting in the global config.json. */
  saveLang(lang: Lang): void;
}

export async function setup(opts: SetupOptions): Promise<Harness> {
  // Decided before setup (asked on the terminal, --trust-project, or remembered).
  const trusted = !!opts.trustProject;
  const { config, warnings } = loadConfig(opts.cwd, trusted, opts.preloaded?.files);
  warnings.push(...(opts.preloaded?.warnings ?? []));
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
  if (!PERMISSION_MODES.includes(mode)) throw new Error(t('unknown permission mode "{mode}" ({modes})', { mode, modes: PERMISSION_MODES.join(", ") }));
  const home = sasacodeHome();
  const sessionsDir = sessionDir(home, opts.cwd);
  // Sessions, input history, saved outputs and keys live here: private to the user, whatever the umask.
  mkdirSync(home, { recursive: true, mode: 0o700 });
  try {
    if (statSync(home).mode & 0o077) chmodSync(home, 0o700);
  } catch {}

  const agent = new Agent({
    model: resolve(opts.model ?? config.model ?? defaultModel(providers, !config.plugins?.disabled?.includes("openai-codex"))),
    cwd: opts.cwd,
    systemPrompt: buildSystemPrompt({ cwd: opts.cwd, append: [config.instructions, opts.appendSystemPrompt].filter((s): s is string => !!s) }),
    thinking: (opts.thinking ?? config.thinking ?? "high") as ThinkingLevel,
    permissions: new PermissionPolicy(mode, config.permissions),
    getApiKey: (p) => resolveApiKey(p, providers[p]),
    resolveModel: resolve,
    maxTurns: opts.maxTurns ?? config.maxTurns ?? DEFAULT_MAX_TURNS,
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
    lang: currentLang(),
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
  // Loaded now, not with the other plugins: /model lists its models at startup.
  if (!disabled.has("openai-codex")) await host.load("openai-codex", bundledPlugins["openai-codex"]!);

  let sessionEntries: Parameters<PluginHost["setSessionEntries"]>[0] = [];
  const startSession = async (resumed: boolean) => {
    host.setSessionEntries(sessionEntries);
    await agent.hooks.run("session_start", { sessionId: agent.session?.id, resumed });
  };
  const endSession = (reason: "switch" | "exit") => agent.hooks.run("session_end", { sessionId: agent.session?.id, reason });

  /** `keepModel`: -m was given on the command line, and beats the model the session ended with. */
  const openSession = (path: string, keepModel = false) => {
    const { file, entries } = SessionFile.open(path);
    const state = restore(entries);
    agent.messages = state.messages;
    agent.session = file;
    // Record the repair, so restoring again (after more turns are appended) gives the same history.
    if (state.repaired) file.append({ type: "replace", messages: state.messages });
    sessionEntries = entries;
    if (state.model && !keepModel) {
      try {
        agent.model = resolve(state.model);
      } catch (e) {
        warnings.push(t("session model {model} unavailable: {error}", { model: state.model, error: (e as Error).message }));
      }
    }
  };
  const createSession = () => {
    agent.messages = [];
    agent.session = opts.noSession ? undefined : SessionFile.create(sessionsDir, opts.cwd);
    sessionEntries = [];
  };

  if (opts.resume) {
    const list = listSessions(sessionsDir, opts.cwd);
    const target =
      opts.resume === "last"
        ? list[0]
        : list.find((s) => s.id === opts.resume || s.path === opts.resume || s.id.startsWith(opts.resume!));
    if (!target) {
      if (opts.resume === "last") throw new Error(t("no previous session in this directory"));
      // The id alone is often all a script keeps: when the session lives in another directory,
      // name it, so the run can be continued from there instead of "not found".
      const elsewhere = findSession(join(home, "sessions"), opts.resume);
      throw new Error(elsewhere ? t("session {id} belongs to {cwd}: run sasacode from there", { id: opts.resume, cwd: elsewhere.cwd }) : t("session not found: {id}", { id: opts.resume }));
    }
    openSession(target.path, !!opts.model);
  } else createSession();

  let loading: Promise<void> | undefined;
  const loadPlugins = (ui: UIBridge = headlessUI) =>
    (loading ??= (async () => {
      host.setUI(ui);
      const found: FoundPlugin[] = [];
      if (opts.preloaded) found.push(...opts.preloaded.plugins);
      else {
        const problems: string[] = [];
        found.push(...discoverPlugins(opts.cwd, problems));
        for (const w of problems) host.notify(w, "warning");
      }
      for (let i = found.length - 1; i >= 0; i--) if (disabled.has(found[i]!.manifest.name)) found.splice(i, 1);
      const skipped = found.filter((p) => p.scope === "project" && !trusted);
      if (skipped.length)
        host.notify(t("project plugins not loaded until you trust this project: {names}", { names: skipped.map((p) => p.manifest.name).join(", ") }), "warning");
      const usable = found.filter((p) => p.scope === "global" || trusted);
      // A user or project plugin of the same name replaces the bundled one (both would hook twice).
      const own = new Set(usable.map((p) => p.manifest.name));
      for (const [name, plugin] of Object.entries(bundledPlugins))
        if (!disabled.has(name) && name !== "openai-codex" && !own.has(name)) await host.load(name, plugin);
      // jev-guard left the bundle (0.9.6): a config that turns it on must not lose it without a word.
      if ((config.plugins?.settings?.["jev-guard"] as { enabled?: boolean } | undefined)?.enabled && !own.has("jev-guard") && !disabled.has("jev-guard"))
        host.notify(t("jev-guard is no longer bundled, so the Jev check your config turns on is not running: sasacode plugin install sasacode-plugin-jev-guard"), "warning");

      if (!disabled.has("skills")) {
        const dirs = [join(home, "skills"), join(opts.cwd, ".sasacode", "skills"), ...usable.flatMap((p) => {
          if (!p.manifest.skills) return [];
          const dir = resolvePath(p.dir, p.manifest.skills);
          // A plugin's skills come from its own folder, like its code.
          if (isInside(dir, p.dir)) return [dir];
          host.notify(t("plugin {name}: skills folder {folder} is outside the plugin; ignored", { name: p.manifest.name, folder: p.manifest.skills! }), "warning");
          return [];
        })];
        await host.load("skills", createSkillsPlugin(dirs, { builtinDir: join(home, "builtin-skills") }));
      }
      if (!disabled.has("mcp")) {
        const servers: Record<string, McpServerConfig> = {};
        // Project MCP servers are only in `config` when the project is trusted.
        for (const [n, s] of Object.entries(config.mcpServers ?? {})) servers[n] = s as McpServerConfig;
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
      await endSession("switch");
      openSession(path);
      await startSession(true);
    },
    async newSession() {
      await endSession("switch");
      createSession();
      await startSession(false);
    },
    async fork(index) {
      const kept = agent.messages.slice(0, index);
      await endSession("switch");
      createSession();
      for (const m of kept) agent.session?.append({ type: "message", message: m });
      agent.messages = kept;
      await startSession(false);
    },
    saveLang(lang) {
      const path = join(sasacodeHome(), "config.json");
      writeJson(path, { ...readJson(path), lang }); // a config.json that cannot be read is left alone (readJson throws)
    },
    async shutdown() {
      await endSession("exit");
    },
  };
}

async function loadExternal(host: PluginHost, p: FoundPlugin): Promise<void> {
  let exts;
  try {
    exts = await importExtensions(p);
  } catch (e) {
    host.plugins.push({ name: p.manifest.name, error: (e as Error).message });
    host.notify(t("plugin {name} skipped: {error}", { name: p.manifest.name, error: (e as Error).message }), "warning");
    return;
  }
  for (const [i, fn] of exts.entries()) await host.load(exts.length > 1 ? `${p.manifest.name}#${i}` : p.manifest.name, fn);
}
