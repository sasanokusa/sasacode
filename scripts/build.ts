// Single-file executables with `bun build --compile`.
//   bun run build          → dist/sasacode for this machine
//   bun run build --all    → dist/sasacode-<os>-<arch> for every release target
import { $ } from "bun";

//   bun run build --targets darwin   → only targets containing "darwin"
const TARGETS = [
  "bun-darwin-arm64",
  "bun-darwin-x64",
  "bun-linux-x64",
  "bun-linux-x64-baseline",
  "bun-linux-arm64",
  "bun-linux-x64-musl",
  "bun-linux-arm64-musl",
];
const entry = "packages/cli/src/main.ts";
const all = process.argv.includes("--all");
const filter = process.argv[process.argv.indexOf("--targets") + 1];
const chosen = process.argv.includes("--targets") ? TARGETS.filter((t) => t.includes(filter!)) : all ? TARGETS : [undefined];

await $`rm -rf dist && mkdir -p dist`;
for (const target of chosen) {
  const out = target ? `dist/sasacode-${target.replace(/^bun-/, "")}` : "dist/sasacode";
  const targetArgs = target ? [`--target=${target}`] : [];
  await $`bun build ${entry} --compile --minify --sourcemap ${targetArgs} --outfile ${out}`;
  console.log(`built ${out}`);
}
