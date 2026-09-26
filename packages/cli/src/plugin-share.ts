// Sharing plugins: finding them (npm's keyword search plus a small curated list on sasanokusa.com),
// publishing a single-file plugin to npm, and looking at what a package is before installing it.
// Packages themselves always come from npm or git; the site only serves the list, a few KB.
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { basename, extname, join } from "node:path";
import { PLUGIN_API_VERSION } from "@sasacode/plugin-api";
import { sasacodeHome } from "./config.ts";

/** The npm keyword that makes a package show up in `sasacode plugin search`. */
export const PLUGIN_KEYWORD = "sasacode-plugin";
const registry = () => (process.env.SASACODE_NPM_REGISTRY ?? "https://registry.npmjs.org").replace(/\/$/, "");
const indexUrl = () => process.env.SASACODE_PLUGIN_INDEX ?? "https://sasanokusa.com/sasacode/plugins.json";

export interface Listing {
  name: string;
  /** What to pass to `sasacode plugin install`. */
  source: string;
  version?: string;
  description?: string;
  author?: string;
  /** From the curated list. */
  recommended?: boolean;
}

async function getJson(url: string, doFetch: typeof fetch): Promise<any> {
  const res = await doFetch(url, { signal: AbortSignal.timeout(8000), headers: { accept: "application/json" } });
  if (!res.ok) throw new Error(`${url} returned ${res.status}`);
  return res.json();
}

const matches = (l: Listing, words: string[]) => words.every((w) => `${l.name} ${l.description ?? ""}`.toLowerCase().includes(w.toLowerCase()));

/** The curated list and npm's keyword search together; one failing source does not hide the other. */
export async function searchPlugins(query: string, doFetch: typeof fetch = fetch): Promise<{ listings: Listing[]; errors: string[] }> {
  const words = query.split(/\s+/).filter(Boolean);
  const errors: string[] = [];
  const [curated, npm] = await Promise.all([
    getJson(indexUrl(), doFetch)
      .then((j) =>
        ((j?.plugins ?? []) as Listing[])
          .filter((p) => typeof p?.name === "string" && typeof p?.source === "string")
          .map((p) => ({ name: p.name, source: p.source, description: p.description, author: p.author, recommended: true }))
          .filter((l) => matches(l, words)),
      )
      .catch((e) => (errors.push(`list: ${(e as Error).message}`), [] as Listing[])),
    // No words: list everything (npm allows up to 250 a page).
    getJson(`${registry()}/-/v1/search?text=${encodeURIComponent(`keywords:${PLUGIN_KEYWORD} ${query}`.trim())}&size=${words.length ? 25 : 250}`, doFetch)
      .then((j) =>
        ((j?.objects ?? []) as { package: any }[]).map(({ package: p }) => ({
          name: p.name,
          source: p.name,
          version: p.version,
          description: p.description,
          author: p.publisher?.username ?? p.author?.name,
        })),
      )
      .catch((e) => (errors.push(`npm: ${(e as Error).message}`), [] as Listing[])),
  ]);
  const seen = new Set(curated.map((l) => l.source));
  return { listings: [...curated, ...npm.filter((l) => !seen.has(l.source))], errors };
}

export function formatListings(listings: Listing[]): string {
  if (!listings.length) return "no plugins found";
  return listings
    .map((l) => {
      const head = `${l.recommended ? "★ " : "  "}${l.name}${l.version ? `@${l.version}` : ""}${l.author ? `  by ${l.author}` : ""}`;
      return [head, l.description ? `    ${l.description}` : "", `    sasacode plugin install ${l.source}`].filter(Boolean).join("\n");
    })
    .join("\n\n");
}

// ── before installing ───────────────────────────────────────────────

/** "name", "name@1.2.3", "@scope/name@^1" → name and requested version or range. */
export function parseSpec(spec: string): { name: string; range?: string } {
  const at = spec.indexOf("@", spec.startsWith("@") ? 1 : 0);
  return at < 0 ? { name: spec } : { name: spec.slice(0, at), range: spec.slice(at + 1) || undefined };
}

/** A package in a local directory (to try one before publishing it). */
export function describeLocal(dir: string): string[] {
  const pkg = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
  const lines = [`${pkg.name ?? "?"}@${pkg.version ?? "?"}  (local: ${dir})`];
  if (pkg.description) lines.push(`  ${pkg.description}`);
  if (!pkg.sasacode) lines.push('  warning: no "sasacode" field in package.json, so it may not be a sasacode plugin');
  return lines;
}

/** What an npm package is, shown before it is installed (it will run with the user's rights). */
export async function describeNpm(spec: string, doFetch: typeof fetch = fetch): Promise<string[]> {
  const { name, range } = parseSpec(spec);
  const doc = await getJson(`${registry()}/${name.replace("/", "%2f")}`, doFetch);
  const version = range && doc.versions?.[range] ? range : range && doc["dist-tags"]?.[range] ? doc["dist-tags"][range] : doc["dist-tags"]?.latest;
  const v = doc.versions?.[version] ?? {};
  const lines = [`${name}@${version ?? "?"}${range && !doc.versions?.[range] && !doc["dist-tags"]?.[range] ? ` (range ${range}: the newest match is installed)` : ""}`];
  if (v.description) lines.push(`  ${v.description}`);
  const people = (v.maintainers ?? doc.maintainers ?? []).map((m: { name?: string }) => m.name).filter(Boolean);
  if (people.length) lines.push(`  published by ${people.join(", ")}${doc.time?.[version] ? ` on ${String(doc.time[version]).slice(0, 10)}` : ""}`);
  if (v.dist) lines.push(`  ${v.dist.fileCount ?? "?"} files, ${v.dist.unpackedSize ? `${Math.round(v.dist.unpackedSize / 1024)} KB` : "size unknown"}`);
  const deps = Object.keys(v.dependencies ?? {});
  if (deps.length) lines.push(`  depends on ${deps.join(", ")}`);
  if (v.scripts?.preinstall || v.scripts?.install || v.scripts?.postinstall) lines.push("  has install scripts (not run: packages are added without them)");
  if (!v.sasacode) lines.push('  warning: no "sasacode" field in package.json, so it may not be a sasacode plugin');
  const repo = typeof v.repository === "string" ? v.repository : v.repository?.url;
  if (repo) lines.push(`  source ${repo}`);
  return lines;
}

// ── publishing ──────────────────────────────────────────────────────

/** The lowest plugin API a file needs, judged by the features it mentions. */
export function apiRangeFor(source: string): string {
  if (/on\(\s*["']permission["']|softDeny/.test(source)) return "^1.7.0";
  if (/permissionsAs/.test(source)) return "^1.6.0";
  if (/listModels|registerProvider[\s\S]*fetch/.test(source)) return "^1.5.0";
  return "^1.4.0";
}

/**
 * The first real line of the file's header comment (the first block comment, or leading // lines),
 * skipping a line that only repeats the plugin's name.
 */
export function describeSource(source: string, name?: string): string | undefined {
  const comment = /\/\*[\s\S]*?\*\//.exec(source)?.[0] ?? /^\s*((?:\s*\/\/.*\n?)+)/.exec(source)?.[1] ?? "";
  const line = comment
    .split("\n")
    .map((l) => l.replace(/^\s*(\/\*\*?|\*\/|\*|\/\/)\s?/, "").replace(/\*\/\s*$/, "").trim())
    .find((l) => l && !l.startsWith("@") && l !== name && !/^[\w-]+:$/.test(l));
  return line?.replace(/^[\w-]+\s+[—–-]\s+/, "").slice(0, 200);
}

const NPM_NAME = /^(@[a-z0-9][\w.-]*\/)?[a-z0-9][\w.-]*$/;

export interface StageOptions {
  name?: string;
  version?: string;
  api?: string;
  license?: string;
  description?: string;
  /** Latest published version, to publish the next patch. */
  latest?: string;
}

/**
 * Turn a single-file plugin into an npm package directory (under ~/.sasacode/publish): package.json
 * with the "sasacode" manifest, the search keyword and the plugin API as a peer, plus a README.
 */
export function stagePackage(file: string, o: StageOptions = {}): { dir: string; pkg: Record<string, any>; notes: string[] } {
  const source = readFileSync(file, "utf8");
  const ext = extname(file);
  if (![".ts", ".js", ".mjs"].includes(ext)) throw new Error(`${file}: a plugin file ends in .ts, .js or .mjs`);
  const base = basename(file, ext);
  const name = o.name ?? `sasacode-plugin-${base.toLowerCase().replace(/[^a-z0-9.-]+/g, "-")}`;
  if (!NPM_NAME.test(name)) throw new Error(`"${name}" is not a valid npm package name; pass --name`);
  const notes: string[] = [];
  if (/from\s+["']\.\.?\//.test(source)) notes.push("it imports other local files, which are not included: publish its directory instead");
  const nextPatch = (v: string) => v.replace(/^(\d+)\.(\d+)\.(\d+).*$/, (_m, a, b, c) => `${a}.${b}.${Number(c) + 1}`);
  const version = o.version ?? (o.latest ? nextPatch(o.latest) : "0.1.0");
  const api = o.api ?? apiRangeFor(source);
  const description = o.description ?? describeSource(source, base) ?? `${base} plugin for sasacode`;
  const entry = `index${ext}`;
  const pkg: Record<string, any> = {
    name,
    version,
    description,
    keywords: [PLUGIN_KEYWORD, "sasacode"],
    type: "module",
    files: [entry, "README.md"],
    peerDependencies: { "@sasacode/plugin-api": api },
    sasacode: { apiVersion: api, extensions: [entry] },
  };
  if (o.license) pkg.license = o.license;
  else notes.push("no license given: others may not reuse it (add one with --license MIT, for example)");

  const dir = join(sasacodeHome(), "publish", name.replace("/", "__"));
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });
  cpSync(file, join(dir, entry));
  writeFileSync(join(dir, "package.json"), `${JSON.stringify(pkg, null, 2)}\n`);
  writeFileSync(
    join(dir, "README.md"),
    `# ${name}\n\n${description}\n\nA plugin for [sasacode](https://github.com/sasanokusa/sasacode) (plugin API ${api}).\n\n\`\`\`bash\nsasacode plugin install ${name}\n\`\`\`\n`,
  );
  if (PLUGIN_API_VERSION.split(".")[0] !== api.replace(/^\D+/, "").split(".")[0]) notes.push(`plugin API ${api} does not match this sasacode (${PLUGIN_API_VERSION})`);
  return { dir, pkg, notes };
}

/** A plugin directory that is already a package: checked, and given the search keyword if missing. */
export function checkPackageDir(dir: string): { pkg: Record<string, any>; notes: string[] } {
  const path = join(dir, "package.json");
  if (!existsSync(path)) throw new Error(`${dir} has no package.json; publish a single .ts file instead, or add one`);
  const pkg = JSON.parse(readFileSync(path, "utf8"));
  if (!pkg.sasacode) throw new Error(`${path} has no "sasacode" field (the manifest: extensions, apiVersion, skills)`);
  const notes: string[] = [];
  if (!pkg.keywords?.includes(PLUGIN_KEYWORD)) {
    pkg.keywords = [...(pkg.keywords ?? []), PLUGIN_KEYWORD];
    writeFileSync(path, `${JSON.stringify(pkg, null, 2)}\n`);
    notes.push(`added the "${PLUGIN_KEYWORD}" keyword to package.json so that plugin search finds it`);
  }
  return { pkg, notes };
}

export async function latestVersion(name: string, doFetch: typeof fetch = fetch): Promise<string | undefined> {
  try {
    return (await getJson(`${registry()}/${name.replace("/", "%2f")}`, doFetch))["dist-tags"]?.latest;
  } catch {
    return undefined;
  }
}

export const isDir = (p: string) => existsSync(p) && statSync(p).isDirectory();
