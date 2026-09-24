import type { UserContent } from "@sasacode/ai";
import {
  type CommandDefinition,
  PLUGIN_API_VERSION,
  type Plugin,
  type PluginAPI,
  type PluginUI,
  type Provider,
  type ProviderConfig,
  type SelectOption,
  type ToolDefinition,
  type ToolRenderer,
} from "@sasacode/plugin-api";
import type { Agent } from "./agent.ts";
import type { SessionEntry } from "./session.ts";

/** What a front end (TUI or headless) provides to plugins. */
export interface UIBridge {
  interactive: boolean;
  notify(message: string, level: "info" | "warning" | "error"): void;
  confirm(title: string, message?: string): Promise<boolean>;
  select(title: string, options: SelectOption[]): Promise<string | undefined>;
}

export const headlessUI: UIBridge = {
  interactive: false,
  notify: (m, level) => process.stderr.write(`${level === "info" ? "" : `${level}: `}${m}\n`),
  confirm: async () => false,
  select: async () => undefined,
};

export interface PluginHostOptions {
  agent: Agent;
  cwd: string;
  settings?: (plugin: string) => Record<string, unknown>;
  registerProvider?: (name: string, config: ProviderConfig, implementation?: Provider) => void;
  /** Tools switched off in config (P3); registrations under these names are ignored. */
  disabledTools?: string[];
}

export interface LoadedPlugin {
  name: string;
  error?: string;
}

/** Hands each plugin its own PluginAPI and keeps what they register. */
export class PluginHost {
  readonly commands: (CommandDefinition & { owner: string })[] = [];
  readonly renderers = new Map<string, ToolRenderer>();
  readonly status = new Map<string, string>();
  readonly plugins: LoadedPlugin[] = [];
  readonly toolOwners = new Map<string, string>();
  onChange?: () => void;
  private ui: UIBridge = headlessUI;
  private pendingNotices: [string, "info" | "warning" | "error"][] = [];
  private custom: Extract<SessionEntry, { type: "custom" }>[] = [];

  constructor(readonly opts: PluginHostOptions) {}

  setUI(ui: UIBridge): void {
    this.ui = ui;
    for (const [m, l] of this.pendingNotices.splice(0)) ui.notify(m, l);
  }

  /** Custom entries of the session now loaded (call after resume / clear). */
  setSessionEntries(entries: SessionEntry[]): void {
    this.custom = entries.filter((e): e is Extract<SessionEntry, { type: "custom" }> => e.type === "custom");
  }

  async load(name: string, plugin: Plugin): Promise<boolean> {
    try {
      await plugin(this.api(name));
      this.plugins.push({ name });
      return true;
    } catch (e) {
      const error = e instanceof Error ? e.message : String(e);
      this.plugins.push({ name, error });
      this.notify(`plugin ${name} failed to load: ${error}`, "error");
      return false;
    }
  }

  notify(message: string, level: "info" | "warning" | "error" = "info"): void {
    if (this.ui === headlessUI && level === "info") this.pendingNotices.push([message, level]);
    else this.ui.notify(message, level);
  }

  api(name: string): PluginAPI {
    const { agent } = this.opts;
    const host = this;
    const ui: PluginUI = {
      get interactive() {
        return host.ui.interactive;
      },
      notify: (m, level = "info") => host.notify(m, level),
      confirm: (t, m) => host.ui.confirm(t, m),
      select: (t, o) => host.ui.select(t, o),
      setStatus(key, text) {
        const k = `${name}:${key}`;
        if (text === undefined) host.status.delete(k);
        else host.status.set(k, text);
        host.onChange?.();
      },
      registerToolRenderer(tool, r) {
        host.renderers.set(tool, r);
      },
    };
    return {
      version: PLUGIN_API_VERSION,
      name,
      cwd: this.opts.cwd,
      settings: this.opts.settings?.(name) ?? {},
      registerTool(tool: ToolDefinition<any>) {
        if (host.opts.disabledTools?.includes(tool.name)) return;
        agent.setTool(tool);
        host.toolOwners.set(tool.name, name);
      },
      registerCommand(command) {
        const i = host.commands.findIndex((c) => c.name === command.name);
        if (i >= 0) host.commands.splice(i, 1);
        host.commands.push({ ...command, owner: name });
        host.onChange?.();
      },
      registerProvider(providerName, config, implementation) {
        if (!host.opts.registerProvider) throw new Error("registerProvider is not available");
        host.opts.registerProvider(providerName, config, implementation);
      },
      on(event, handler) {
        agent.hooks.on(event, handler, name);
      },
      permissions: { addRules: (rules) => agent.permissions.addRules(rules) },
      ui,
      session: {
        get id() {
          return agent.session?.id;
        },
        append(kind, data) {
          const entry = { type: "custom" as const, plugin: name, kind, data };
          host.custom.push(entry);
          agent.session?.append(entry);
        },
        entries: (kind) => host.custom.filter((e) => e.plugin === name && e.kind === kind).map((e) => e.data),
        messages: () => agent.messages,
        replaceMessages: (messages) => agent.replaceMessages(messages),
        inject(content, when = "next") {
          const c: UserContent[] = typeof content === "string" ? [{ type: "text", text: content }] : content;
          agent.inject(c, when);
        },
      },
      agent: {
        model: () => agent.model,
        tools: () => agent.getTools(),
        run: (o) => agent.runSubagent(o),
        complete: (o) => agent.complete(o),
      },
      log: (...args) => {
        if (process.env.SASACODE_DEBUG) console.error(`[${name}]`, ...args);
      },
    };
  }
}
