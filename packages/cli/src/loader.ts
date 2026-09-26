import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { basename, join, resolve } from "node:path";
import { createInterface } from "node:readline/promises";
import * as pluginApi from "@sasacode/plugin-api";
import { isCompatible, type Plugin } from "@sasacode/plugin-api";
import { type McpServerConfig, sasacodeHome } from "./config.ts";
import { checkPackageDir, describeLocal, describeNpm, formatListings, isDir, latestVersion, parseSpec, searchPlugins, stagePackage } from "./plugin-share.ts";

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

// ── install / list / remove / search / publish / update ─────────────

async function sh(cmd: string[], cwd: string, env?: Record<string, string>): Promise<void> {
  // stdin too: npm asks for a one-time password or a browser login on the terminal.
  const p = Bun.spawn(cmd, { cwd, stdin: "inherit", stdout: "inherit", stderr: "inherit", env: env ? { ...process.env, ...env } : undefined });
  if ((await p.exited) !== 0) throw new Error(`${cmd.join(" ")} failed`);
}

/** The package manager is the Bun inside sasacode itself, so the single binary needs no bun installed. */
const bun = (args: string[], cwd: string) => sh([process.execPath, ...args], cwd, { BUN_BE_BUN: "1" });

const isGit = (spec: string) => /^(git@|git\+|https?:\/\/.*\.git$|https:\/\/github\.com\/)/.test(spec);

async function confirm(question: string, yes: boolean): Promise<boolean> {
  if (yes) return true;
  if (!process.stdin.isTTY) {
    console.error("stopped: pass --yes to go ahead without a question (no terminal to ask on)");
    return false;
  }
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  try {
    return /^y(es)?$/i.test((await rl.question(`${question} [y/N] `)).trim());
  } finally {
    rl.close();
  }
}

const gitHead = (dir: string) => Bun.spawnSync(["git", "-C", dir, "rev-parse", "--short", "HEAD"]).stdout.toString().trim();
const installedVersion = (dir: string, name: string) => readJson(join(dir, "node_modules", name, "package.json"))?.version as string | undefined;
const flag = (args: string[], name: string) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
};

const USAGE = `usage: sasacode plugin <command> [--project]
  search [words]                       find plugins (the list on sasanokusa.com and npm); no words lists all
  install <npm-spec|git-url|dir> [--yes]  add one (shows what it is and asks first)
  update [name]                        update npm plugins, or pull a git one (shows the changes)
  remove <name>
  list
  publish <file.ts|dir> [--name <pkg>] [--version <v>] [--api <range>] [--license <id>] [--description <text>] [--otp <code>] [--dry-run]`;

export async function pluginCommand(args: string[], cwd: string): Promise<number> {
  const [sub, spec] = args;
  const project = args.includes("--project");
  const yes = args.includes("--yes") || args.includes("-y");
  const dir = pluginDirs(cwd)[project ? 1 : 0]!.dir;
  try {
    if (sub === "search") {
      const words = args.slice(1).filter((a) => !a.startsWith("--")).join(" ");
      const { listings, errors } = await searchPlugins(words);
      console.log(formatListings(listings));
      for (const e of errors) console.error(`warning: could not search ${e}`);
      return 0;
    }
    if (sub === "install" && spec) {
      mkdirSync(dir, { recursive: true });
      const warning = "A plugin runs inside sasacode with your permissions: install only code you trust.";
      if (isGit(spec)) {
        const url = spec.replace(/^git\+/, "");
        const name = basename(url).replace(/\.git$/, "");
        if (existsSync(join(dir, name))) throw new Error(`${join(dir, name)} already exists; remove it first or use plugin update`);
        console.log(`${url}\n  cloned into ${join(dir, name)}\n${warning}`);
        if (!(await confirm("Install it?", yes))) return 1;
        await sh(["git", "clone", "--depth", "1", url, name], dir);
        if (existsSync(join(dir, name, "package.json"))) await bun(["install", "--production"], join(dir, name));
        if (!readManifest(join(dir, name))) console.error(`warning: ${name} has no plugin.json or "sasacode" field in package.json`);
        console.log(`installed ${name} at commit ${gitHead(join(dir, name))}`);
      } else {
        const local = isDir(spec) ? resolve(spec) : undefined;
        console.log([...(local ? describeLocal(local) : await describeNpm(spec)), warning].join("\n"));
        if (!(await confirm("Install it?", yes))) return 1;
        if (!existsSync(join(dir, "package.json"))) writeFileSync(join(dir, "package.json"), '{ "private": true }\n');
        await bun(["add", local ?? spec], dir);
        const name = local ? readJson(join(local, "package.json"))?.name : parseSpec(spec).name;
        console.log(`installed ${name}@${installedVersion(dir, name) ?? "?"} into ${dir}`);
      }
      return 0;
    }
    if (sub === "update") {
      const deps = Object.keys(readJson(join(dir, "package.json"))?.dependencies ?? {});
      const gitDirs = discoverPlugins(cwd).filter((p) => pluginDirs(cwd)[project ? 1 : 0]!.dir === join(p.dir, "..") && existsSync(join(p.dir, ".git")));
      const npmTargets = spec ? deps.filter((d) => d === spec) : deps;
      const gitTargets = spec ? gitDirs.filter((p) => p.manifest.name === spec || basename(p.dir) === spec) : gitDirs;
      if (!npmTargets.length && !gitTargets.length) throw new Error(spec ? `${spec} is not an installed npm or git plugin here` : "no npm or git plugins to update");
      if (npmTargets.length) {
        const before = Object.fromEntries(npmTargets.map((n) => [n, installedVersion(dir, n)]));
        await bun(["update", ...npmTargets, "--latest"], dir);
        for (const n of npmTargets) {
          const after = installedVersion(dir, n);
          console.log(`${n}: ${before[n] === after ? `${after} (already the newest)` : `${before[n]} → ${after}`}`);
        }
      }
      for (const p of gitTargets) {
        await sh(["git", "-C", p.dir, "fetch", "-q", "--depth", "50"], p.dir);
        const log = Bun.spawnSync(["git", "-C", p.dir, "log", "--oneline", "HEAD..FETCH_HEAD"]).stdout.toString().trim();
        if (!log) {
          console.log(`${p.manifest.name}: already the newest (${gitHead(p.dir)})`);
          continue;
        }
        const stat = Bun.spawnSync(["git", "-C", p.dir, "diff", "--stat", "HEAD", "FETCH_HEAD"]).stdout.toString().trim();
        console.log(`${p.manifest.name}: new commits\n${log}\n${stat}`);
        if (!(await confirm(`Update ${p.manifest.name}?`, yes))) continue;
        await sh(["git", "-C", p.dir, "reset", "-q", "--hard", "FETCH_HEAD"], p.dir);
        if (existsSync(join(p.dir, "package.json"))) await bun(["install", "--production"], p.dir);
        console.log(`${p.manifest.name}: now at ${gitHead(p.dir)}`);
      }
      return 0;
    }
    if (sub === "publish" && spec) {
      if (!Bun.which("npm")) throw new Error("publishing uses npm (npm publish): install Node.js / npm first");
      let target: string;
      let notes: string[];
      let pkg: Record<string, any>;
      if (isDir(spec)) {
        ({ pkg, notes } = checkPackageDir(spec));
        target = spec;
      } else {
        const name = flag(args, "--name");
        const latest = flag(args, "--version") ? undefined : await latestVersion(name ?? `sasacode-plugin-${basename(spec).replace(/\.(ts|js|mjs)$/, "").toLowerCase()}`);
        ({ dir: target, pkg, notes } = stagePackage(spec, { name, version: flag(args, "--version"), api: flag(args, "--api"), license: flag(args, "--license"), description: flag(args, "--description"), latest }));
      }
      console.log(`${pkg.name}@${pkg.version}  (plugin API ${pkg.sasacode?.apiVersion ?? "?"})\n  ${target}`);
      for (const n of notes) console.log(`  note: ${n}`);
      const otp = flag(args, "--otp");
      await sh(["npm", "publish", "--access", "public", ...(otp ? [`--otp=${otp}`] : []), ...(args.includes("--dry-run") ? ["--dry-run"] : [])], target);
      if (!args.includes("--dry-run")) console.log(`published: others can run  sasacode plugin install ${pkg.name}`);
      return 0;
    }
    if (sub === "remove" && spec) {
      const target = join(dir, spec);
      if (existsSync(target) && statSync(target).isDirectory() && !existsSync(join(dir, "node_modules", spec))) rmSync(target, { recursive: true, force: true });
      else await bun(["remove", spec], dir);
      console.log(`removed ${spec}`);
      return 0;
    }
    if (sub === "list" || !sub) {
      for (const p of discoverPlugins(cwd))
        console.log(`${p.manifest.name}${p.manifest.version ? `@${p.manifest.version}` : ""}  [${p.scope}]  ${p.dir}`);
      return 0;
    }
  } catch (e) {
    console.error(`error: ${(e as Error).message}`);
    return 1;
  }
  console.error(USAGE);
  return 1;
}
