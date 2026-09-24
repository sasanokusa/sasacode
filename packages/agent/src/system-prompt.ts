import { existsSync } from "node:fs";
import { join } from "node:path";

/** Facts only (A1): who we are, where we run. No instructions on how to think. */
export function buildSystemPrompt(opts: { cwd: string; append?: string[] }): string {
  const lines = [
    "You are sasacode, a coding agent running in the user's terminal. You act through the provided tools.",
    "",
    "Environment:",
    `- Working directory: ${opts.cwd}`,
    `- Platform: ${process.platform} (${process.arch})`,
    `- Date: ${new Date().toISOString().slice(0, 10)}`,
    `- Git repository: ${existsSync(join(opts.cwd, ".git")) ? "yes" : "no"}`,
  ];
  for (const a of opts.append ?? []) if (a.trim()) lines.push("", a.trim());
  return lines.join("\n");
}
