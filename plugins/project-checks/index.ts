import { readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { text, type Plugin } from "@sasacode/plugin-api";
interface Check { command: string; timeout: number }

const commandSchema = { type: "string", minLength: 1, maxLength: 8000, pattern: "\\S" };
const settingsSchema = {
  type: "object", additionalProperties: false, properties: {
    checks: { type: "object", additionalProperties: { anyOf: [commandSchema, {
      type: "object", required: ["command"], additionalProperties: false,
      properties: { command: commandSchema, timeout: { type: "number", minimum: 1, maximum: 3600 } },
    }] } },
  },
};

export function loadChecks(cwd: string, settings: Record<string, unknown>): Map<string, Check> {
  const checks = new Map<string, Check>();
  if (settings.checks === undefined) {
    // Only known script names, and commands are frozen until the next plugin load.
    let scripts: Record<string, unknown>;
    try { scripts = JSON.parse(readFileSync(join(cwd, "package.json"), "utf8")).scripts ?? {}; }
    catch (e) { if ((e as NodeJS.ErrnoException).code === "ENOENT") return checks; throw e; }
    const managers = [
      ["bun", ["bun.lock", "bun.lockb"]], ["pnpm", ["pnpm-lock.yaml"]],
      ["yarn", ["yarn.lock"]], ["npm", ["package-lock.json", "npm-shrinkwrap.json"]],
    ] as const;
    const found = managers.filter(([, locks]) => locks.some((lock) => existsSync(join(cwd, lock))));
    if (found.length > 1) throw new Error("project-checks: multiple package manager lockfiles; configure checks explicitly");
    const manager = found[0]?.[0] ?? "npm";
    for (const name of ["test", "lint", "typecheck", "check", "build"]) {
      if (typeof scripts[name] === "string") checks.set(name, { command: `${manager} run ${name}`, timeout: 120 });
    }
    return checks;
  }
  if (!settings.checks || typeof settings.checks !== "object" || Array.isArray(settings.checks)) throw new Error("checks must be an object");
  for (const [name, raw] of Object.entries(settings.checks)) {
    if (!/^[a-z][a-z0-9-]{0,39}$/.test(name)) throw new Error(`Invalid check name: ${name}`);
    const value = typeof raw === "string" ? { command: raw } : raw;
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`Invalid check: ${name}`);
    const { command, timeout = 120 } = value as Record<string, unknown>;
    if (typeof command !== "string" || !command.trim() || command.length > 8000) throw new Error(`Invalid command: ${name}`);
    if (typeof timeout !== "number" || !Number.isFinite(timeout) || timeout < 1 || timeout > 3600) throw new Error(`Invalid timeout: ${name} (1..3600 seconds)`);
    checks.set(name, { command, timeout });
  }
  return checks;
}

const plugin: Plugin = (api) => {
  const checks = loadChecks(api.cwd, api.defineSettings({ schema: settingsSchema }));
  const listing = () => checks.size ? [...checks].map(([name, c]) => `${name}: ${c.command} (timeout ${c.timeout}s)`).join("\n")
    : api.ui.lang === "ja" ? "チェック未設定。plugins.settings.project-checks.checks にコマンドを設定してください。" : "No checks. Configure plugins.settings.project-checks.checks.";
  api.registerCommand({ name: "checks", description: api.ui.lang === "ja" ? "利用できるプロジェクト検査を表示" : "List available project checks", run: () => api.ui.showText({ title: "Project checks", text: listing() }) });
  api.registerTool({ name: "project_checks", description: "List configured project checks and their exact commands without executing them.", kind: "read", concurrent: true,
    paths: () => [join(api.cwd, "package.json")], parameters: { type: "object", properties: {}, additionalProperties: false },
    execute: async () => ({ content: [text(listing())] }),
  });
  if (!checks.size) return;
  const get = (name: string) => { const c = checks.get(name); if (!c) throw new Error(`Unknown check: ${name}`); return c; };
  api.registerTool({
    name: "project_check", description: "Run ONE named project check (test/lint/typecheck/build). Returns PASS/FAIL, exit code, duration and bounded log; may modify files as its command does. Does not imply other checks passed. Inspect project_checks first.",
    kind: "exec", permissionsAs: "bash", concurrent: false,
    matchTarget: ({ name }) => get(name).command,
    summary: ({ name }) => `${name}: ${get(name).command}`,
    parameters: { type: "object", properties: { name: { type: "string", enum: [...checks.keys()] } }, required: ["name"], additionalProperties: false },
    execute: async ({ name }, ctx) => {
      if (!ctx.callTool) throw new Error("project-checks requires a host tool context with callTool (API 1.9+)");
      const check = get(name);
      const started = Date.now();
      const result = await ctx.callTool("bash", { command: check.command, timeout: check.timeout });
      const durationMs = Date.now() - started;
      return { ...result, content: [text(`[${name}: ${result.isError ? "FAIL" : "PASS"}; ${durationMs}ms including approval]`), ...result.content],
        details: { ...result.details, name, command: check.command, durationMs } };
    },
  });
};
export default plugin;
