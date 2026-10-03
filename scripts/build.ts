// Single-file executables with `bun build --compile`.
//   bun run build          → dist/sasacode for this machine
//   bun run build --all    → dist/sasacode-<os>-<arch>[.exe] for every release target
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
  "bun-windows-x64",
  "bun-windows-x64-baseline",
  "bun-windows-arm64",
];
const entry = "packages/cli/src/main.ts";
const all = process.argv.includes("--all");
const filter = process.argv[process.argv.indexOf("--targets") + 1];
const chosen = process.argv.includes("--targets") ? TARGETS.filter((t) => t.includes(filter!)) : all ? TARGETS : [undefined];

await $`rm -rf dist && mkdir -p dist`;
for (const target of chosen) {
  // Windows executables end in .exe (Bun would add it anyway; written out, `out` is the real name).
  const windows = target ? target.includes("windows") : process.platform === "win32";
  const out = `${target ? `dist/sasacode-${target.replace(/^bun-/, "")}` : "dist/sasacode"}${windows ? ".exe" : ""}`;
  const targetArgs = target ? [`--target=${target}`] : [];
  // The working directory is an untrusted repository: never pick up its bunfig.toml (preload
  // scripts would run as sasacode) or .env (e.g. ANTHROPIC_BASE_URL would reroute API keys).
  await $`bun build ${entry} --compile --minify --sourcemap --no-compile-autoload-bunfig --no-compile-autoload-dotenv ${targetArgs} --outfile ${out}`;
  console.log(`built ${out}`);
}
