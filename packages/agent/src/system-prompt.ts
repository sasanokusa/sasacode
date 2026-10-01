import { existsSync } from "node:fs";
import { join } from "node:path";

/** Facts only (A1): who we are, where we run. No instructions on how to think. */
export function buildSystemPrompt(opts: { cwd: string; append?: string[] }): string {
  const lines = [
    "You are sasacode, a coding agent running in the user's terminal. You act through the provided tools.",
  ];
  for (const a of opts.append ?? []) if (a.trim()) lines.push("", a.trim());
  // Stable instructions precede session facts, so a different cwd does not cut their cache prefix.
  lines.push("", "Environment:", `- Platform: ${process.platform} (${process.arch})`,
    `- Git repository: ${existsSync(join(opts.cwd, ".git")) ? "yes" : "no"}`,
    `- Date: ${new Date().toISOString().slice(0, 10)}`, `- Working directory: ${opts.cwd}`);
  return lines.join("\n");
}
