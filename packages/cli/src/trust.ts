// One trust decision per project covers everything in it that runs code or changes where your
// data goes: project plugins, and the elevated part of .sasacode/config.json (endpoints, MCP
// servers, plugin settings, allow rules, looser modes). Asked once, again when that set changes.
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { createInterface } from "node:readline/promises";
import { describeElevated, loadConfig, sasacodeHome } from "./config.ts";
import { discoverPlugins } from "./loader.ts";

export interface ProjectTrust {
  /** What trusting would enable; empty when the project needs no trust. */
  items: string[];
  fingerprint: string;
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
  const fingerprint = createHash("sha256").update(JSON.stringify({ elevated, manifests })).digest("hex").slice(0, 16);
  return { items, fingerprint };
}

export function isTrusted(cwd: string, t: ProjectTrust): boolean {
  if (!t.items.length) return true;
  const { global } = loadConfig(cwd, false);
  return !!global.trustedProjects?.includes(cwd) || readTrust()[cwd] === t.fingerprint;
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
      "Plugins and MCP servers run code as you; endpoints receive your conversation and code.\n",
  );
  const rl = createInterface({ input: process.stdin, output: process.stderr });
  try {
    const answer = (await rl.question(`Trust ${cwd}? [y/N] `)).trim().toLowerCase();
    return answer === "y" || answer === "yes";
  } finally {
    rl.close();
  }
}
