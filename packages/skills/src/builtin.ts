import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import toolAuthoring from "../builtin/tool-authoring/SKILL.md" with { type: "text" };

/** Skills shipped inside sasacode (and inside the single binary). */
export const BUILTIN_SKILLS: Record<string, string> = {
  "tool-authoring": toolAuthoring,
};

/**
 * Write the built-in skills under `dir` so they are ordinary files the model can read with the
 * read tool. Rewritten only when the content changed (e.g. after an upgrade).
 */
export function materializeBuiltinSkills(dir: string): void {
  for (const [name, body] of Object.entries(BUILTIN_SKILLS)) {
    const path = join(dir, name, "SKILL.md");
    if (existsSync(path) && readFileSync(path, "utf8") === body) continue;
    mkdirSync(join(dir, name), { recursive: true });
    writeFileSync(path, body);
  }
}
