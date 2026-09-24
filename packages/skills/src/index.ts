import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import type { Plugin } from "@sasacode/plugin-api";

export interface Skill {
  name: string;
  description: string;
  path: string;
}

/** Minimal YAML frontmatter reader for the standard fields (plain, quoted, or folded `>` / `|` scalars). */
export function parseFrontmatter(text: string): Record<string, string> {
  const m = /^---\r?\n([\s\S]*?)\r?\n---/.exec(text);
  if (!m) return {};
  const out: Record<string, string> = {};
  const lines = m[1]!.split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    const kv = /^([A-Za-z0-9_-]+):\s*(.*)$/.exec(lines[i]!);
    if (!kv) continue;
    let value = kv[2]!.trim();
    if (value === ">" || value === "|" || value === ">-" || value === "|-") {
      const block: string[] = [];
      while (i + 1 < lines.length && /^\s+\S|^\s*$/.test(lines[i + 1]!)) block.push(lines[++i]!.trim());
      value = block.join(value.startsWith(">") ? " " : "\n").trim();
    } else value = value.replace(/^(["'])(.*)\1$/, "$2");
    out[kv[1]!] = value;
  }
  return out;
}

export function discoverSkills(dirs: string[]): Skill[] {
  const byName = new Map<string, Skill>();
  for (const dir of dirs) {
    if (!existsSync(dir)) continue;
    for (const entry of readdirSync(dir)) {
      const path = join(dir, entry, "SKILL.md");
      if (!existsSync(path) || !statSync(path).isFile()) continue;
      const fm = parseFrontmatter(readFileSync(path, "utf8"));
      if (!fm.name || !fm.description) continue;
      // Later directories (project) override earlier ones (global) with the same name.
      byName.set(fm.name, { name: fm.name, description: fm.description, path });
    }
  }
  return [...byName.values()];
}

/**
 * Agent Skills adapter: only name, description and path go into the system prompt; the model
 * reads SKILL.md with the read tool when it needs it (progressive disclosure, A3).
 */
export function createSkillsPlugin(dirs: string[]): Plugin {
  return (api) => {
    const skills = discoverSkills(dirs);
    if (!skills.length) return;
    // Skill files live outside the project too (~/.sasacode/skills); reading them must not prompt every time.
    api.permissions.addRules({ allow: dirs.map((d) => `read(${d}/**)`) });

    const listing = [
      "Agent Skills are available. Each is a folder with a SKILL.md holding instructions; when a task matches a skill's description, read its SKILL.md with the read tool first and follow it.",
      ...skills.map((s) => `- ${s.name}: ${s.description} (${s.path})`),
    ].join("\n");
    api.on("system_prompt", ({ prompt }) => ({ prompt: `${prompt}\n\n${listing}` }));

    for (const s of skills) {
      api.registerCommand({
        name: `skill:${s.name}`,
        description: s.description.slice(0, 80),
        argumentHint: "[task]",
        run({ args }) {
          const body = readFileSync(s.path, "utf8");
          api.session.inject(
            `Use the "${s.name}" skill (${s.path}). Its instructions:\n\n${body}${args ? `\n\nTask: ${args}` : ""}`,
            args ? "now" : "next",
          );
          if (!args) api.ui.notify(`skill ${s.name} は次のメッセージと一緒に送られます`);
        },
      });
    }
    api.registerCommand({
      name: "skills",
      description: "利用できる Skills の一覧",
      run: () => api.ui.notify(skills.map((s) => `${s.name} — ${s.description}\n  ${s.path}`).join("\n")),
    });
  };
}
