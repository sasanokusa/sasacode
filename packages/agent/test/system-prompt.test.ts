import { expect, test } from "bun:test";
import { buildSystemPrompt } from "../src/system-prompt.ts";

test("stable instructions precede cwd/date, without adding behavioral policy", () => {
  const a = buildSystemPrompt({ cwd: "/tmp/a", append: ["Project instructions."] });
  const b = buildSystemPrompt({ cwd: "/tmp/b", append: ["Project instructions."] });
  expect(a.indexOf("Project instructions.")).toBeLessThan(a.indexOf("Environment:"));
  expect(a.slice(0, a.indexOf("- Working directory:"))).toBe(b.slice(0, b.indexOf("- Working directory:")));
  expect(a).toContain("- Working directory: /tmp/a");
  expect(a.split("\n").filter(line => line.startsWith("- "))).toHaveLength(4);
});
