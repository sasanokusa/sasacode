import { homedir } from "node:os";
import { isAbsolute, resolve } from "node:path";

export function resolvePath(p: string, cwd: string): string {
  if (p === "~" || p.startsWith("~/")) p = homedir() + p.slice(1);
  return isAbsolute(p) ? p : resolve(cwd, p);
}

/** Compact line diff for display: common prefix/suffix trimmed, 3 lines of context. */
export function lineDiff(before: string, after: string, context = 3): string {
  const a = before.split("\n");
  const b = after.split("\n");
  let start = 0;
  while (start < a.length && start < b.length && a[start] === b[start]) start++;
  let endA = a.length - 1;
  let endB = b.length - 1;
  while (endA >= start && endB >= start && a[endA] === b[endB]) {
    endA--;
    endB--;
  }
  const from = Math.max(0, start - context);
  const out: string[] = [`@@ -${from + 1} +${from + 1} @@`];
  for (let i = from; i < start; i++) out.push(` ${a[i]}`);
  for (let i = start; i <= endA; i++) out.push(`-${a[i]}`);
  for (let i = start; i <= endB; i++) out.push(`+${b[i]}`);
  for (let i = endA + 1; i <= Math.min(a.length - 1, endA + context); i++) out.push(` ${a[i]}`);
  return out.join("\n");
}
