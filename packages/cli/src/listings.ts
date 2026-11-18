// What a front end (the desktop GUI) asks before it starts a run: the models, slash commands and
// skills there are, the plugins and their state, and whether the folder is trusted. Each is read
// without running plugin code: a plugin does its startup work when it loads (background-sessions
// reports finished jobs then, MCP connects its servers), and a listing must not take that from the
// next run. Plugin commands are therefore not listed here: a control run reports them (`commands`).
import { existsSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { PERMISSION_MODES } from "@sasacode/agent";
import { fmtTokens } from "@sasacode/ai";
import { bundledPlugins } from "@sasacode/bundled";
import { currentLang, t } from "@sasacode/host";
import type { CommandDefinition, PluginAPI } from "@sasacode/plugin-api";
import { createSkillsPlugin, discoverSkills, materializeBuiltinSkills, type Skill } from "@sasacode/skills";
import { loadConfig, sasacodeHome } from "./config.ts";
import { readJsonOr } from "./json.ts";
import { extensionFiles, pluginDirs, readManifest } from "./loader.ts";
import { type Harness, setup, skillFolders, usablePlugins } from "./setup.ts";
import { assessProject, isTrusted, type ProjectTrust, saveTrust } from "./trust.ts";

/**
 * The TUI's own commands (coreCommands in packages/tui/src/app.ts, which a test keeps in step): they
 * work in the TUI only. Descriptions as the TUI writes them, shown through t().
 */
export const TUI_COMMANDS: { name: string; description: string; argumentHint?: string }[] = [
  { name: "help", description: "コマンドとキー操作を表示" },
  { name: "model", description: "モデルを切り替える（プロバイダーから取得した一覧）", argumentHint: "<provider/model> | --refresh" },
  { name: "effort", description: "推論の深さ（thinking）を切り替える", argumentHint: "off|low|medium|high|xhigh|max" },
  { name: "copy", description: "直前の応答をクリップボードにコピー" },
  { name: "resume", description: "過去のセッションを再開" },
  { name: "clear", description: "新しいセッションを始める" },
  { name: "permission", description: "権限モードを切り替える", argumentHint: PERMISSION_MODES.join("|") },
  { name: "fork", description: "過去のメッセージから会話を分岐する" },
  { name: "session", description: "このセッションの ID と保存先" },
  { name: "language", description: "表示言語を切り替える（日本語 / English）", argumentHint: "ja|en" },
  { name: "exit", description: "終了する" },
  { name: "quit", description: "終了する（/exit と同じ）" },
];

export interface ListedModel {
  /** "<provider>/<model>", as -m takes it. */
  id: string;
  contextWindow?: number;
  /** The most the model takes, when what it runs with is not known (Ollama without num_ctx, LM Studio before loading). */
  maxContext?: number;
  server?: "ollama" | "lmstudio" | "llamacpp";
  /** The model a run here starts with. */
  current?: true;
}

export interface ListedCommand {
  name: string;
  description: string;
  argumentHint?: string;
  /** The plugin that registered it ("core-commands": the TUI). */
  owner: string;
  /** Runs in `sasacode -p "/name …" --slash`; false for the TUI's own commands. */
  headless: boolean;
}

export interface ListedPlugin {
  name: string;
  version?: string;
  source: "bundled" | "global" | "project";
  /** Where it is installed (not for bundled ones). */
  dir?: string;
  /** Loaded by a run here: not turned off in config, not replaced, and a project's only in a trusted project. */
  enabled: boolean;
  /** Why it would not load, as far as can be told without running it (API version, missing files). */
  error?: string;
}

const warn = (warning: string) => console.error(t("warning: {warning}", { warning }));

/** The trust decision as it stands, and the config it covers (no question asked). */
function folder(cwd: string, project: ProjectTrust = assessProject(cwd)) {
  const trusted = isTrusted(cwd, project);
  const disabled = new Set(loadConfig(cwd, trusted, project.files).config.plugins?.disabled ?? []);
  return { project, trusted, disabled, usable: usablePlugins(project.plugins, disabled, trusted) };
}

/** What the TUI's /model offers in `cwd`: the current model, the config's `models`, then what the providers list. */
export async function modelListing(cwd: string): Promise<ListedModel[]> {
  const { project, trusted } = folder(cwd);
  const h = await setup({ cwd, noSession: true, trustProject: trusted, preloaded: project });
  h.warnings.forEach(warn);
  return modelsOf(h);
}

/** modelListing for a harness already set up. */
export async function modelsOf(h: Pick<Harness, "agent" | "config" | "listModels">): Promise<ListedModel[]> {
  const current = `${h.agent.model.provider}/${h.agent.model.id}`;
  const listed = await h.listModels().catch((e: Error) => (warn(e.message), []));
  const known = new Map(listed.map((m) => [m.spec, m]));
  return [...new Set([current, ...(h.config.models ?? []), ...listed.map((m) => m.spec)])].map((id) => {
    const m = known.get(id);
    return {
      id,
      ...(m?.contextWindow ? { contextWindow: m.contextWindow } : {}),
      ...(m?.maxContext ? { maxContext: m.maxContext } : {}),
      ...(m?.server ? { server: m.server } : {}),
      ...(id === current ? { current: true as const } : {}),
    };
  });
}

/** The skills a run here offers: the skills plugin's folders, the built-in ones first as it puts them. */
export function skillListing(cwd: string): Skill[] {
  const { disabled, usable } = folder(cwd);
  if (disabled.has("skills")) return [];
  const builtin = join(sasacodeHome(), "builtin-skills");
  const dirs = skillFolders(sasacodeHome(), cwd, usable, warn);
  try {
    materializeBuiltinSkills(builtin);
    dirs.unshift(builtin);
  } catch {}
  return discoverSkills(dirs);
}

/**
 * The slash commands known without loading plugins: the TUI's own, and the skills plugin's (made by
 * that plugin from the skill files, with nothing else running).
 */
export function commandListing(cwd: string): ListedCommand[] {
  const tui = TUI_COMMANDS.map((c) => ({ name: c.name, description: t(c.description), ...(c.argumentHint ? { argumentHint: c.argumentHint } : {}), owner: "core-commands", headless: false }));
  const { disabled, usable } = folder(cwd);
  if (disabled.has("skills")) return tui;
  const registered: CommandDefinition[] = [];
  // Only what the skills plugin calls while it loads.
  const api = {
    ui: { lang: currentLang(), notify: (m: string, level?: string) => level === "info" || warn(m) },
    permissions: { addRules() {} },
    on() {},
    registerCommand: (c: CommandDefinition) => void registered.push(c),
  } as unknown as PluginAPI;
  void createSkillsPlugin(skillFolders(sasacodeHome(), cwd, usable, warn), { builtinDir: join(sasacodeHome(), "builtin-skills") })(api);
  const skills = registered.map((c) => ({ name: c.name, description: c.description, ...(c.argumentHint ? { argumentHint: c.argumentHint } : {}), owner: "skills", headless: true }));
  return [...tui, ...skills];
}

/** `sasacode plugin list --json`: every plugin a run here could load, bundled ones too. */
export function pluginListing(cwd: string): ListedPlugin[] {
  const { project, disabled, usable } = folder(cwd);
  const own = new Set(usable.map((p) => p.manifest.name));
  const out: ListedPlugin[] = [];
  // openai-codex, skills and mcp load whatever a user plugin is called; the others give way to one of the same name.
  for (const name of Object.keys(bundledPlugins))
    out.push({ name, source: "bundled", enabled: !disabled.has(name) && (name === "openai-codex" || !own.has(name)) });
  for (const name of ["skills", "mcp"]) out.push({ name, source: "bundled", enabled: !disabled.has(name) });
  for (const p of project.plugins) {
    let error: string | undefined;
    try {
      extensionFiles(p);
    } catch (e) {
      error = (e as Error).message;
    }
    const { name, version } = p.manifest;
    out.push({ name, ...(version ? { version } : {}), source: p.scope, dir: p.dir, enabled: usable.includes(p), ...(error ? { error } : {}) });
  }
  // Installed by name before install refused them: never loaded, but worth removing.
  for (const { dir, scope } of pluginDirs(cwd))
    for (const name of Object.keys(readJsonOr<{ dependencies?: object }>(join(dir, "package.json"), {})?.dependencies ?? {})) {
      const at = join(dir, "node_modules", name);
      if (existsSync(at) && !readManifest(at)) out.push({ name, source: scope, dir: at, enabled: false, error: t("not a sasacode plugin") });
    }
  return out;
}

/** `sasacode trust --json`: the decision for `cwd`, and what the project's config would add with it. */
export function trustStatus(cwd: string, project: ProjectTrust = assessProject(cwd)) {
  const { elevated } = loadConfig(cwd, false, project.files);
  return {
    cwd,
    trusted: isTrusted(cwd, project),
    /** False when the project config adds nothing that needs trust. */
    needed: project.items.length > 0,
    /** False when the plugins' code could not all be read: a saved decision does not hold, so --trust-project is needed every run. */
    complete: project.complete,
    /** One line per thing trust enables, as the trust question lists them. */
    items: project.items,
    plugins: project.plugins.filter((p) => p.scope === "project").map((p) => p.manifest.name),
    mcpServers: Object.keys(elevated.mcpServers ?? {}),
    endpoints: Object.entries(elevated.providers ?? {}).map(([name, p]) => ({ name, url: p.baseUrl ?? null })),
    allow: elevated.permissions?.allow ?? [],
    mode: elevated.permissions?.mode ?? null,
  };
}

/** `sasacode trust [--yes] [--json]`: show the folder's trust; --yes trusts it, as answering y at the question does. */
export function trustCommand(args: string[], cwd: string): number {
  const { values, positionals } = parseArgs({ args, allowPositionals: true, options: { yes: { type: "boolean", short: "y" }, json: { type: "boolean" } } });
  if (positionals.length) throw new Error(t("usage: sasacode trust [--yes] [--json]"));
  const project = assessProject(cwd);
  if (values.yes && project.items.length && !isTrusted(cwd, project)) saveTrust(cwd, project);
  const status = trustStatus(cwd, project);
  if (values.json) console.log(JSON.stringify(status));
  else if (!status.needed) console.log(t("{cwd}: nothing in this project needs trust", { cwd }));
  else {
    console.log(status.trusted ? t("{cwd} is trusted", { cwd }) : t("{cwd} is not trusted", { cwd }));
    console.log(t("This project wants to use:"));
    for (const item of status.items) console.log(`  - ${item}`);
    if (!status.complete) console.log(t("The plugins' code could not all be read (too many files, unreadable, or a link loop), so you will be asked every time."));
    else if (!status.trusted) console.log(t("trust it with: sasacode trust --yes"));
  }
  return values.yes && !status.trusted ? 1 : 0;
}

/** `sasacode list models|commands|skills [--json]`. */
export async function listCommand(args: string[], cwd: string): Promise<number> {
  const { values, positionals } = parseArgs({ args, allowPositionals: true, options: { json: { type: "boolean" } } });
  const [what] = positionals;
  if (positionals.length !== 1 || !["models", "commands", "skills"].includes(what!)) throw new Error(t("usage: sasacode list models|commands|skills [--json]"));
  const json = (v: unknown) => console.log(JSON.stringify(v));
  if (what === "models") {
    const models = await modelListing(cwd);
    if (values.json) json(models);
    else
      for (const m of models) {
        const size = m.contextWindow ? `${fmtTokens(m.contextWindow)} ctx` : m.maxContext ? t("up to {size}", { size: fmtTokens(m.maxContext) }) : "";
        console.log([m.id, size, m.current ? t("現在") : ""].filter(Boolean).join("  "));
      }
  } else if (what === "commands") {
    const commands = commandListing(cwd);
    if (values.json) json(commands);
    else {
      for (const c of commands)
        console.log(`/${c.name}${c.argumentHint ? ` ${c.argumentHint}` : ""}  ${c.description}  [${c.owner}${c.headless ? "" : `, ${t("TUI only")}`}]`);
      console.log(t("(other plugins' commands are listed by a run: the `commands` line of --control stdio)"));
    }
  } else {
    const skills = skillListing(cwd);
    if (values.json) json(skills);
    else for (const s of skills) console.log(`${s.name} — ${s.description}\n  ${s.path}`);
  }
  return 0;
}
