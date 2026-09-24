// One trust decision per project covers everything in it that runs code or changes where your
// data goes: project plugins, and the elevated part of .sasacode/config.json (endpoints, MCP
// servers, plugin settings, allow rules, looser modes). Asked once, and again when any of it
// changes, the plugins' code included.
import { createHash, type Hash } from "node:crypto";
import { mkdirSync, readdirSync, readFileSync, realpathSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { createInterface } from "node:readline/promises";
import { describeElevated, loadConfig, sasacodeHome } from "./config.ts";
import { discoverPlugins } from "./loader.ts";

export interface ProjectTrust {
  /** What trusting would enable; empty when the project needs no trust. */
  items: string[];
  fingerprint: string;
  /**
   * False when the plugins' code could not all be read (too many files, unreadable, a link loop):
   * a remembered decision then cannot vouch for it, so the user is asked every time.
   */
  complete: boolean;
}

const trustFile = () => join(sasacodeHome(), "trust.json");

function readTrust(): Record<string, string> {
  try {
    return JSON.parse(readFileSync(trustFile(), "utf8"));
  } catch {
    return {};
  }
}

export function assessProject(cwd: string): ProjectTrust {
  const { elevated } = loadConfig(cwd, false);
  const plugins = discoverPlugins(cwd).filter((p) => p.scope === "project");
  const items = [...plugins.map((p) => `plugin ${p.manifest.name}`), ...describeElevated(elevated)];
  const manifests = plugins.map((p) => p.manifest).sort((a, b) => a.name.localeCompare(b.name));
  const hash = createHash("sha256").update(JSON.stringify({ elevated, manifests }));
  let complete = true;
  if (plugins.length) {
    // The code itself: the project's plugins folder, and packages installed into it.
    const root = join(cwd, ".sasacode", "plugins");
    const walk: Walk = { hash, root, files: 5000, seen: new Set() };
    complete = hashTree(walk, root);
    for (const p of plugins) if (relative(root, p.dir).startsWith("node_modules")) complete = hashTree(walk, p.dir) && complete;
  }
  return { items, fingerprint: hash.digest("hex").slice(0, 16), complete };
}

interface Walk {
  hash: Hash;
  root: string;
  /** Files left before giving up. */
  files: number;
  /** Real paths of folders already walked (link loops). */
  seen: Set<string>;
}

/**
 * Every file under `dir` (paths and contents), in a fixed order, following links to files and
 * folders. Dependencies (node_modules) are not included. False when something could not be read.
 */
function hashTree(w: Walk, dir: string): boolean {
  let real: string;
  let entries: string[];
  try {
    real = realpathSync(dir);
    entries = readdirSync(dir).sort();
  } catch {
    return false;
  }
  if (w.seen.has(real)) return false; // a link loop, or a folder linked twice
  w.seen.add(real);
  let ok = true;
  for (const name of entries) {
    if (name === "node_modules" || name === ".git") continue;
    if (--w.files < 0) return false;
    const path = join(dir, name);
    w.hash.update(`\0${relative(w.root, path)}\0`);
    try {
      const st = statSync(path); // through links: what would be loaded
      if (st.isDirectory()) ok = hashTree(w, path) && ok;
      else if (st.isFile()) w.hash.update(readFileSync(path));
    } catch {
      ok = false;
    }
  }
  return ok;
}

export function isTrusted(cwd: string, t: ProjectTrust): boolean {
  if (!t.items.length) return true;
  const { global } = loadConfig(cwd, false);
  return !!global.trustedProjects?.includes(cwd) || (t.complete && readTrust()[cwd] === t.fingerprint);
}

export function saveTrust(cwd: string, t: ProjectTrust): void {
  const all = readTrust();
  all[cwd] = t.fingerprint;
  mkdirSync(dirname(trustFile()), { recursive: true });
  writeFileSync(trustFile(), `${JSON.stringify(all, null, 2)}\n`);
}

/** Ask on the terminal before anything from the project is used. */
export async function askTrust(cwd: string, t: ProjectTrust): Promise<boolean> {
  process.stderr.write(
    `\nThis project wants to use:\n${t.items.map((i) => `  - ${i}`).join("\n")}\n` +
      "Plugins and MCP servers run code as you; endpoints receive your conversation and code.\n" +
      (t.complete ? "" : "The plugins' code could not all be read (too many files, unreadable, or a link loop), so you will be asked every time.\n"),
  );
  const rl = createInterface({ input: process.stdin, output: process.stderr });
  try {
    const answer = (await rl.question(`Trust ${cwd}? [y/N] `)).trim().toLowerCase();
    return answer === "y" || answer === "yes";
  } finally {
    rl.close();
  }
}
