// The improvement loop's dashboard: `bun run improve`.
// Prints what one round of docs/improvement.md starts from — the checks, the core line budget and
// the size of the repository — and exits 1 when a check fails, so it works as a gate as well.
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { CORE_LINE_BUDGET, READMES, ROOT, coreLines, runChecks, walk } from "./checks.ts";

const checks = runChecks();
const lines = coreLines();
const failed = checks.filter((check) => check.problems.length > 0);

/** `  3991 / 4000  ai 1136 · agent 2019 · plugin-api 406 · tools 430`. */
function coreLine() {
  const perPackage = Object.entries(lines.byPackage)
    .map(([name, count]) => `${name} ${count}`)
    .join(" · ");
  const left = CORE_LINE_BUDGET - lines.total;
  return `${lines.total} / ${CORE_LINE_BUDGET} (${left} left)   ${perPackage}`;
}

function repository() {
  const typescript = walk(ROOT, (name) => name.endsWith(".ts"));
  const tests = typescript.filter((path) => path.endsWith(".test.ts")).length;
  const packages = readdirSync(join(ROOT, "packages"), { withFileTypes: true }).filter((entry) => entry.isDirectory()).length;
  const docs = existsSync(join(ROOT, "docs")) ? readdirSync(join(ROOT, "docs")).filter((name) => name.endsWith(".md")).length : 0;
  return `${packages} packages · ${docs} docs · ${typescript.length} TypeScript files, ${tests} of them tests`;
}

/** The two READMEs are one document in two languages: a size difference is worth a look. */
function readmes() {
  return READMES.map((name) => `${name} ${readFileSync(join(ROOT, name), "utf8").split("\n").length - 1} lines`).join(" · ");
}

console.log("sasacode — improvement signals");
console.log("");
console.log("checks");
for (const check of checks) {
  console.log(`  ${check.problems.length === 0 ? "ok  " : "FAIL"}  ${check.name.padEnd(18)} ${check.promise}`);
  for (const problem of check.problems) console.log(`          ${problem}`);
}
console.log("");
console.log("core lines");
console.log(`  ${coreLine()}`);
console.log("");
console.log("repository");
console.log(`  ${repository()}`);
console.log(`  ${readmes()}`);
console.log("");
console.log("next");
if (failed.length > 0) {
  console.log(`  fix the ${failed.length} failing check${failed.length === 1 ? "" : "s"} above, then run this again`);
} else {
  console.log("  pick a lens in docs/improvement.md and make one small change");
}
if (CORE_LINE_BUDGET - lines.total < 100) {
  console.log(`  the core has ${CORE_LINE_BUDGET - lines.total} lines left of its ${CORE_LINE_BUDGET}: shrink it or raise the budget on purpose`);
}
console.log("  confirm with: bun run typecheck && bun test");
console.log("  record it in: docs/improvement.md");

process.exit(failed.length > 0 ? 1 : 0);
