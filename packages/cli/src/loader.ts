import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import * as pluginApi from "@sasacode/plugin-api";
import { isCompatible, type Plugin } from "@sasacode/plugin-api";
import type { McpServerConfig } from "./config.ts";
import { sasacodeHome } from "./config.ts";

export interface Manifest {
  name: string;
  version?: string;
  /** Required plugin API range, e.g. "^1.0.0". */
  apiVersion?: string;
  /** In-process extension modules (TS runs without a build step). */
  extensions?: string[];
  /** Directory of Agent Skills (SKILL.md folders). */
  skills?: string;
  mcpServers?: Record<string, McpServerConfig>;
}

export interface FoundPlugin {
  manifest: Manifest;
  dir: string;
  scope: "global" | "project";
}

export function pluginDirs(cwd: string): { dir: string; scope: FoundPlugin["scope"] }[] {
  return [
    { dir: join(sasacodeHome(), "plugins"), scope: "global" },
    { dir: join(cwd, ".sasacode", "plugins"), scope: "project" },
  ];
}

function readJson(path: string): any {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch {
    return undefined;
  }
}

/** plugin.json, or package.json with a "sasacode" field. */
export function readManifest(dir: string): Manifest | undefined {
  const pj = readJson(join(dir, "plugin.json"));
  if (pj) return { name: basename(dir), ...pj };
  const pkg = readJson(join(dir, "package.json"));
  if (pkg?.sasacode) {
    const apiVersion = pkg.sasacode.apiVersion ?? pkg.peerDependencies?.["@sasacode/plugin-api"];
    return { name: pkg.name ?? basename(dir), version: pkg.version, apiVersion, ...pkg.sasacode };
  }
  return undefined;
}

/**
 * Plugins are directories with a manifest, single .ts/.js files, or packages installed with
 * `sasacode plugin install` (listed in the plugins directory's package.json).
 */
export function discoverPlugins(cwd: string): FoundPlugin[] {
  const found: FoundPlugin[] = [];
  for (const { dir, scope } of pluginDirs(cwd)) {
    if (!existsSync(dir)) continue;
    for (const entry of readdirSync(dir)) {
      if (entry.startsWith(".") || entry === "node_modules" || entry === "package.json" || entry === "bun.lock") continue;
      const path = join(dir, entry);
      if (statSync(path).isDirectory()) {
        const manifest = readManifest(path);
        if (manifest) found.push({ manifest, dir: path, scope });
      } else if (/\.(ts|js|mjs)$/.test(entry)) {
        found.push({ manifest: { name: entry.replace(/\.(ts|js|mjs)$/, ""), extensions: [entry] }, dir, scope });
      }
    }
    const deps = readJson(join(dir, "package.json"))?.dependencies ?? {};
    for (const name of Object.keys(deps)) {
      const pdir = join(dir, "node_modules", name);
      const manifest = readManifest(pdir);
      if (manifest) found.push({ manifest, dir: pdir, scope });
    }
  }
  return found;
}

let virtualApiRegistered = false;
/** Plugins may import "@sasacode/plugin-api" without installing it: resolve it to the host's copy. */
function registerVirtualApi(): void {
  if (virtualApiRegistered) return;
  virtualApiRegistered = true;
  Bun.plugin({
    name: "sasacode-plugin-api",
    setup(build) {
      build.module("@sasacode/plugin-api", () => ({ exports: { ...pluginApi }, loader: "object" }));
    },
  });
}

/** Import every extension module of a plugin. Throws when the API version does not fit. */
export async function importExtensions(p: FoundPlugin): Promise<Plugin[]> {
  if (p.manifest.apiVersion && !isCompatible(p.manifest.apiVersion))
    throw new Error(`needs plugin API ${p.manifest.apiVersion}, host provides ${pluginApi.PLUGIN_API_VERSION}`);
  registerVirtualApi();
  const out: Plugin[] = [];
  for (const rel of p.manifest.extensions ?? []) {
    const mod = await import(resolve(p.dir, rel));
    const fn = mod.default ?? mod.plugin;
    if (typeof fn !== "function") throw new Error(`${rel} has no default export function`);
    out.push(fn);
  }
  return out;
}

// ── trust for project-local plugins and MCP servers ─────────────────

const trustFile = () => join(sasacodeHome(), "trust.json");

/** Changes whenever the set of project plugins or MCP servers changes, so the prompt comes back. */
export function projectFingerprint(plugins: FoundPlugin[], mcp: Record<string, McpServerConfig>): string {
  const data = JSON.stringify({ plugins: plugins.map((p) => p.manifest).sort((a, b) => a.name.localeCompare(b.name)), mcp });
  return createHash("sha256").update(data).digest("hex").slice(0, 16);
}

export function isTrusted(cwd: string, fingerprint: string): boolean {
  return readJson(trustFile())?.[cwd] === fingerprint;
}

export function saveTrust(cwd: string, fingerprint: string): void {
  const all = readJson(trustFile()) ?? {};
  all[cwd] = fingerprint;
  mkdirSync(dirname(trustFile()), { recursive: true });
  writeFileSync(trustFile(), `${JSON.stringify(all, null, 2)}\n`);
}

// ── install / list / remove ─────────────────────────────────────────

async function sh(cmd: string[], cwd: string): Promise<void> {
  const p = Bun.spawn(cmd, { cwd, stdout: "inherit", stderr: "inherit" });
  if ((await p.exited) !== 0) throw new Error(`${cmd.join(" ")} failed`);
}

const isGit = (spec: string) => /^(git@|git\+|https?:\/\/.*\.git$|https:\/\/github\.com\/)/.test(spec);

export async function pluginCommand(args: string[], cwd: string): Promise<number> {
  const [sub, spec] = args;
  const project = args.includes("--project");
  const dir = pluginDirs(cwd)[project ? 1 : 0]!.dir;
  mkdirSync(dir, { recursive: true });
  if (sub === "install" && spec) {
    if (isGit(spec)) {
      const name = basename(spec.replace(/^git\+/, "")).replace(/\.git$/, "");
      await sh(["git", "clone", "--depth", "1", spec.replace(/^git\+/, ""), name], dir);
      if (existsSync(join(dir, name, "package.json"))) await sh(["bun", "install", "--production"], join(dir, name));
      if (!readManifest(join(dir, name))) console.error(`warning: ${name} has no plugin.json or "sasacode" field in package.json`);
    } else {
      if (!existsSync(join(dir, "package.json"))) writeFileSync(join(dir, "package.json"), '{ "private": true }\n');
      await sh(["bun", "add", spec], dir);
    }
    console.log(`installed ${spec} into ${dir}`);
    return 0;
  }
  if (sub === "remove" && spec) {
    const target = join(dir, spec);
    if (existsSync(target) && statSync(target).isDirectory() && !existsSync(join(dir, "node_modules", spec))) {
      await sh(["rm", "-rf", target], dir);
    } else await sh(["bun", "remove", spec], dir);
    console.log(`removed ${spec}`);
    return 0;
  }
  if (sub === "list" || !sub) {
    for (const p of discoverPlugins(cwd))
      console.log(`${p.manifest.name}${p.manifest.version ? `@${p.manifest.version}` : ""}  [${p.scope}]  ${p.dir}`);
    return 0;
  }
  console.error("usage: sasacode plugin <install <npm-spec|git-url>|remove <name>|list> [--project]");
  return 1;
}
