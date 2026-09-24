// Single-file executables with `bun build --compile`.
//   bun run build          → dist/sasacode for this machine
//   bun run build --all    → dist/sasacode-<os>-<arch> for every release target
import { $ } from "bun";

const TARGETS = ["bun-darwin-arm64", "bun-darwin-x64", "bun-linux-x64", "bun-linux-arm64"];
const entry = "packages/cli/src/main.ts";
const all = process.argv.includes("--all");

await $`rm -rf dist && mkdir -p dist`;
for (const target of all ? TARGETS : [undefined]) {
  const out = target ? `dist/sasacode-${target.replace(/^bun-/, "")}` : "dist/sasacode";
  const targetArgs = target ? [`--target=${target}`] : [];
  await $`bun build ${entry} --compile --minify --sourcemap ${targetArgs} --outfile ${out}`;
  console.log(`built ${out}`);
}
