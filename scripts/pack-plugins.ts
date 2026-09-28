#!/usr/bin/env bun
// Pack the distribution candidates without publishing, then load the extracted archives through
// the real host. No dependency installation or npm lifecycle scripts are needed.
import { mkdtempSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { Agent, PluginHost } from "../packages/agent/src/index.ts";
import { readManifest, importExtensions } from "../packages/cli/src/loader.ts";

const root = resolve(import.meta.dir, "..");
const out = join(root, "dist", "plugin-candidates");
mkdirSync(out, { recursive: true });
const expected: Record<string, { tools: string[]; commands: string[] }> = {
  "ask-user": { tools: ["ask_user"], commands: [] },
  "git-context": { tools: ["git_context"], commands: [] },
  "project-checks": { tools: ["project_checks", "project_check"], commands: ["checks"] },
  workflows: { tools: [], commands: ["workflow"] },
};
for (const [name, registration] of Object.entries(expected)) {
  const src = join(root, "plugins", name);
  const pkg = JSON.parse(readFileSync(join(src, "package.json"), "utf8"));
  const result = JSON.parse(execFileSync("npm", ["pack", "--ignore-scripts", "--json", "--pack-destination", out], { cwd: src, encoding: "utf8" }))[0];
  const allowed = new Set(["package.json", ...pkg.files]);
  if (result.files.some((f: { path: string }) => !allowed.has(f.path))) throw new Error(`${name}: unexpected packaged files`);
  for (const file of allowed) {
    if (!result.files.some((f: { path: string }) => f.path === file)) throw new Error(`${name}: missing ${file}`);
  }
  const dir = mkdtempSync(join(tmpdir(), "sasacode-packed-plugin-"));
  try {
    execFileSync("tar", ["-xzf", join(out, result.filename), "-C", dir]);
    const extracted = join(dir, "package");
    const manifest = readManifest(extracted);
    if (!manifest || manifest.name !== name) throw new Error(`${name}: invalid manifest`);
    const agent = new Agent({ cwd: dir, systemPrompt: "test", model: { id: "test", provider: "test", api: "replay", contextWindow: 10000, maxOutput: 1000 } });
    const host = new PluginHost({ agent, cwd: dir, settings: () => name === "project-checks" ? { checks: { smoke: "true" } } : {} });
    for (const extension of await importExtensions({ dir: extracted, manifest, scope: "project" })) {
      if (!await host.load(name, extension)) throw new Error(host.plugins.at(-1)?.error);
    }
    for (const tool of registration.tools) if (!agent.getTools().some((t) => t.name === tool)) throw new Error(`${name}: missing tool ${tool}`);
    for (const cmd of registration.commands) if (!host.commands.some((c) => c.name === cmd)) throw new Error(`${name}: missing command ${cmd}`);
    console.log(`${result.filename}: ${result.files.length} files, ${result.size} bytes; extracted host load OK`);
  } finally { rmSync(dir, { recursive: true, force: true }); }
}
console.log(`Archives ready in ${out}. Nothing published.`);
