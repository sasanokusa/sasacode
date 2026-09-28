import { existsSync, mkdirSync, readdirSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import { basename, join, resolve } from "node:path";
import { createInterface } from "node:readline/promises";
import { parseArgs } from "node:util";
import { isInside } from "@sasacode/agent";
import { t } from "@sasacode/host";
import * as pluginApi from "@sasacode/plugin-api";
import { isCompatible, type Plugin } from "@sasacode/plugin-api";
import { type McpServerConfig, sasacodeHome } from "./config.ts";
import { readJsonOr } from "./json.ts";
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

const readJson = (path: string): any => readJsonOr(path, undefined);

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
export function discoverPlugins(cwd: string, warnings?: string[]): FoundPlugin[] {
  const found: FoundPlugin[] = [];
  for (const { dir, scope } of pluginDirs(cwd)) {
    if (!existsSync(dir)) continue;
    for (const entry of readdirSync(dir)) {
      if (entry.startsWith(".") || entry === "node_modules" || entry === "package.json" || entry === "bun.lock") continue;
      const path = join(dir, entry);
      let isDirectory: boolean;
      try {
        isDirectory = statSync(path).isDirectory();
      } catch (e) {
        // A broken link or an unreadable entry must not keep sasacode from starting.
        warnings?.push(t("plugin {path} skipped: {error}", { path, error: (e as Error).message }));
        continue;
      }
      if (isDirectory) {
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
    throw new Error(t("needs plugin API {need}, host provides {have}", { need: p.manifest.apiVersion, have: pluginApi.PLUGIN_API_VERSION }));
  registerVirtualApi();
  const out: Plugin[] = [];
  for (const rel of p.manifest.extensions ?? []) {
    const file = resolve(p.dir, rel);
    // Only code inside the plugin's own folder: that is what trust and review covered.
    if (!existsSync(file) || !isInside(realpathSync(file), realpathSync(p.dir))) throw new Error(t("{file} is not a file inside {dir}", { file: rel, dir: p.dir }));
    const mod = await import(file);
    const fn = mod.default ?? mod.plugin;
    if (typeof fn !== "function") throw new Error(t("{file} has no default export function", { file: rel }));
    out.push(fn);
  }
  return out;
}

// ── install / list / remove / search / publish / update ─────────────

async function sh(cmd: string[], cwd: string, env?: Record<string, string>): Promise<void> {
  // stdin too: npm asks for a one-time password or a browser login on the terminal.
  const p = Bun.spawn(cmd, { cwd, stdin: "inherit", stdout: "inherit", stderr: "inherit", env: env ? { ...process.env, ...env } : undefined });
  if ((await p.exited) !== 0) throw new Error(t("{cmd} failed", { cmd: cmd.join(" ") }));
}

/** The package manager is the Bun inside sasacode itself, so the single binary needs no bun installed. */
const bun = (args: string[], cwd: string) => sh([process.execPath, ...args], cwd, { BUN_BE_BUN: "1" });

const isGit = (spec: string) => /^(git@|git\+|https?:\/\/.*\.git$|https:\/\/github\.com\/)/.test(spec);

async function confirm(question: string, yes: boolean): Promise<boolean> {
  if (yes) return true;
  if (!process.stdin.isTTY) {
    console.error(t("stopped: pass --yes to go ahead without a question (no terminal to ask on)"));
    return false;
  }
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  try {
    return /^y(es)?$/i.test((await rl.question(t("{question} [y/N] ", { question }))).trim());
  } finally {
    rl.close();
  }
}

const gitHead = (dir: string) => Bun.spawnSync(["git", "-C", dir, "rev-parse", "--short", "HEAD"]).stdout.toString().trim();
const installedVersion = (dir: string, name: string) => readJson(join(dir, "node_modules", name, "package.json"))?.version as string | undefined;

const USAGE = `usage: sasacode plugin <command> [--project]
  search [words]                       find plugins (the list on sasanokusa.com and npm); no words lists all
  install <npm-spec|git-url|dir> [--yes]  add one (shows what it is and asks first)
  update [name] [--yes]                update npm plugins, or pull a git one (shows the changes and asks)
  remove <name> [--yes]
  list
  publish <file.ts|dir> [--name <pkg>] [--version <v>] [--api <range>] [--license <id>] [--description <text>] [--otp <code>] [--dry-run]`;

export async function pluginCommand(args: string[], cwd: string): Promise<number> {
  let o;
  try {
    o = parseArgs({
      args,
      allowPositionals: true,
      options: {
        project: { type: "boolean" },
        yes: { type: "boolean", short: "y" },
        name: { type: "string" },
        version: { type: "string" },
        api: { type: "string" },
        license: { type: "string" },
        description: { type: "string" },
        otp: { type: "string" },
        "dry-run": { type: "boolean" },
      },
    });
  } catch (e) {
    console.error(`${t("error: {error}", { error: (e as Error).message })}\n${t(USAGE)}`);
    return 1;
  }
  const { values: v, positionals } = o;
  const [sub, spec] = positionals;
  const project = !!v.project;
  const yes = !!v.yes;
  const dir = pluginDirs(cwd)[project ? 1 : 0]!.dir;
  try {
    if (sub === "search") {
      const words = positionals.slice(1).join(" ");
      const { listings, errors } = await searchPlugins(words);
      console.log(formatListings(listings));
      for (const e of errors) console.error(t("warning: could not search {error}", { error: e }));
      return 0;
    }
    if (sub === "install" && spec) {
      mkdirSync(dir, { recursive: true });
      const warning = "A plugin runs inside sasacode with your permissions: install only code you trust.";
      if (isGit(spec)) {
        const url = spec.replace(/^git\+/, "");
        const name = basename(url).replace(/\.git$/, "");
        if (existsSync(join(dir, name))) throw new Error(t("{path} already exists; remove it first or use plugin update", { path: join(dir, name) }));
        console.log(`${url}\n  ${t("cloned into {path}", { path: join(dir, name) })}\n${t(warning)}`);
        if (!(await confirm(t("Install it?"), yes))) return 1;
        await sh(["git", "clone", "--depth", "1", url, name], dir);
        // Like npm installs: the plugin's own install scripts do not run.
        if (existsSync(join(dir, name, "package.json"))) await bun(["install", "--production", "--ignore-scripts"], join(dir, name));
        if (!readManifest(join(dir, name))) console.error(t('warning: {name} has no plugin.json or "sasacode" field in package.json', { name }));
        console.log(t("installed {name} at commit {commit}", { name, commit: gitHead(join(dir, name)) }));
      } else {
        const local = isDir(spec) ? resolve(spec) : undefined;
        console.log([...(local ? describeLocal(local) : await describeNpm(spec)), t(warning)].join("\n"));
        if (!(await confirm(t("Install it?"), yes))) return 1;
        if (!existsSync(join(dir, "package.json"))) writeFileSync(join(dir, "package.json"), '{ "private": true }\n');
        await bun(["add", "--ignore-scripts", local ?? spec], dir);
        const name = local ? readJson(join(local, "package.json"))?.name : parseSpec(spec).name;
        console.log(t("installed {name}@{version} into {dir}", { name, version: installedVersion(dir, name) ?? "?", dir }));
      }
      return 0;
    }
    if (sub === "update") {
      const deps = Object.keys(readJson(join(dir, "package.json"))?.dependencies ?? {});
      const gitDirs = discoverPlugins(cwd).filter((p) => pluginDirs(cwd)[project ? 1 : 0]!.dir === join(p.dir, "..") && existsSync(join(p.dir, ".git")));
      const npmTargets = spec ? deps.filter((d) => d === spec) : deps;
      const gitTargets = spec ? gitDirs.filter((p) => p.manifest.name === spec || basename(p.dir) === spec) : gitDirs;
      if (!npmTargets.length && !gitTargets.length) throw new Error(spec ? t("{spec} is not an installed npm or git plugin here", { spec }) : t("no npm or git plugins to update"));
      if (npmTargets.length) {
        const before = Object.fromEntries(npmTargets.map((n) => [n, installedVersion(dir, n)]));
        // Shown and asked like an install: a new version is new code.
        const changed: string[] = [];
        for (const n of npmTargets) {
          const lines = await describeNpm(`${n}@latest`);
          if (lines[0]?.startsWith(`${n}@${before[n]}`)) {
            console.log(t("{name}: {version} (already the newest)", { name: n, version: before[n] ?? "?" }));
            continue;
          }
          console.log([t("{name}: {version} →", { name: n, version: before[n] ?? "?" }), ...lines].join("\n"));
          if (await confirm(t("Update {name}?", { name: n }), yes)) changed.push(n);
        }
        if (changed.length) await bun(["update", "--ignore-scripts", ...changed, "--latest"], dir);
        for (const n of changed) console.log(t("{name}: {before} → {after}", { name: n, before: before[n] ?? "?", after: installedVersion(dir, n) ?? "?" }));
      }
      for (const p of gitTargets) {
        await sh(["git", "-C", p.dir, "fetch", "-q", "--depth", "50"], p.dir);
        const log = Bun.spawnSync(["git", "-C", p.dir, "log", "--oneline", "HEAD..FETCH_HEAD"]).stdout.toString().trim();
        if (!log) {
          console.log(t("{name}: already the newest ({commit})", { name: p.manifest.name, commit: gitHead(p.dir) }));
          continue;
        }
        const stat = Bun.spawnSync(["git", "-C", p.dir, "diff", "--stat", "HEAD", "FETCH_HEAD"]).stdout.toString().trim();
        console.log(t("{name}: new commits\n{log}\n{stat}", { name: p.manifest.name, log, stat }));
        if (!(await confirm(t("Update {name}?", { name: p.manifest.name }), yes))) continue;
        await sh(["git", "-C", p.dir, "reset", "-q", "--hard", "FETCH_HEAD"], p.dir);
        if (existsSync(join(p.dir, "package.json"))) await bun(["install", "--production", "--ignore-scripts"], p.dir);
        console.log(t("{name}: now at {commit}", { name: p.manifest.name, commit: gitHead(p.dir) }));
      }
      return 0;
    }
    if (sub === "publish" && spec) {
      if (!Bun.which("npm")) throw new Error(t("publishing uses npm (npm publish): install Node.js / npm first"));
      let target: string;
      let notes: string[];
      let pkg: Record<string, any>;
      if (isDir(spec)) {
        ({ pkg, notes } = checkPackageDir(spec));
        target = spec;
      } else {
        const name = v.name;
        const latest = v.version ? undefined : await latestVersion(name ?? `sasacode-plugin-${basename(spec).replace(/\.(ts|js|mjs)$/, "").toLowerCase()}`);
        ({ dir: target, pkg, notes } = stagePackage(spec, { name, version: v.version, api: v.api, license: v.license, description: v.description, latest }));
      }
      console.log(t("{name}@{version}  (plugin API {api})\n  {dir}", { name: pkg.name, version: pkg.version, api: pkg.sasacode?.apiVersion ?? "?", dir: target }));
      for (const n of notes) console.log(`  ${t("note: {note}", { note: n })}`);
      const otp = v.otp;
      await sh(["npm", "publish", "--access", "public", ...(otp ? [`--otp=${otp}`] : []), ...(v["dry-run"] ? ["--dry-run"] : [])], target);
      if (!v["dry-run"]) console.log(t("published: others can run  sasacode plugin install {name}", { name: pkg.name }));
      return 0;
    }
    if (sub === "remove" && spec) {
      // A folder directly in the plugins folder, or an npm package name: never a path elsewhere.
      const folder = /^[^/\\]+$/.test(spec) && spec !== "." && spec !== ".." && spec !== "node_modules";
      if (!folder && !/^(@[a-z0-9~-][a-z0-9._~-]*\/)?[a-z0-9~-][a-z0-9._~-]*$/.test(spec))
        throw new Error(t("{spec}: give the plugin's name as `sasacode plugin list` shows it, not a path", { spec }));
      const target = join(dir, spec);
      if (!(await confirm(t("Remove {name} from {dir}?", { name: spec, dir }), yes))) return 1;
      if (folder && existsSync(target) && statSync(target).isDirectory() && !existsSync(join(dir, "node_modules", spec))) rmSync(target, { recursive: true, force: true });
      else await bun(["remove", spec], dir);
      console.log(t("removed {name}", { name: spec }));
      return 0;
    }
    if (sub === "list" || !sub) {
      for (const p of discoverPlugins(cwd))
        console.log(`${p.manifest.name}${p.manifest.version ? `@${p.manifest.version}` : ""}  [${p.scope}]  ${p.dir}`);
      return 0;
    }
  } catch (e) {
    console.error(t("error: {error}", { error: (e as Error).message }));
    return 1;
  }
  console.error(t(USAGE));
  return 1;
}
