import { existsSync, readFileSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, parse } from "node:path";
import type { Plugin } from "@sasacode/plugin-api";

/** AGENTS.md files that apply to cwd: the global one, then from the repository root down to cwd. */
export function agentsFiles(cwd: string, home = process.env.SASACODE_HOME ?? join(homedir(), ".sasacode")): string[] {
  const chain: string[] = [];
  let dir = cwd;
  while (true) {
    chain.unshift(dir);
    if (existsSync(join(dir, ".git")) || dir === parse(dir).root) break;
    dir = dirname(dir);
  }
  return [join(home, "AGENTS.md"), ...chain.map((d) => join(d, "AGENTS.md"))].filter((p) => existsSync(p) && statSync(p).isFile());
}

/** Project instructions (AGENTS.md; CLAUDE.md is intentionally not read). Re-read when a file changes. */
const agentsMd: Plugin = (api) => {
  let cache = { key: "", text: "" };
  api.on("system_prompt", ({ prompt }) => {
    const files = agentsFiles(api.cwd);
    if (!files.length) return;
    const key = files.map((f) => `${f}@${statSync(f).mtimeMs}`).join("|");
    if (key !== cache.key) {
      const text = files.map((f) => `Instructions from ${f}:\n${readFileSync(f, "utf8").trim()}`).join("\n\n");
      cache = { key, text };
    }
    return { prompt: `${prompt}\n\n${cache.text}` };
  });
};
export default agentsMd;
