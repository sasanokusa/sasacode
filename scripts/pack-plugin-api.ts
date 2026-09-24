#!/usr/bin/env bun
// Builds a standalone npm package of @sasacode/plugin-api in dist/plugin-api: JS plus type
// declarations, with the few @sasacode/ai types it uses copied in, so plugin authors get types
// without the provider SDKs. Its version is the plugin API version, not the app's.
//
//   bun scripts/pack-plugin-api.ts            # build, then: cd dist/plugin-api && npm publish
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { $ } from "bun";

const repo = resolve(import.meta.dir, "..");
const out = join(repo, "dist", "plugin-api");
const src = join(out, "src");
rmSync(out, { recursive: true, force: true });
mkdirSync(src, { recursive: true });

// The API source, with its type-only import pointed at a local copy of the AI types.
const api = readFileSync(join(repo, "packages/plugin-api/src/index.ts"), "utf8");
if (!api.includes('from "@sasacode/ai"')) throw new Error("plugin-api no longer imports @sasacode/ai: update this script");
writeFileSync(join(src, "index.ts"), api.replaceAll('from "@sasacode/ai"', 'from "./ai-types.ts"'));
const types = readFileSync(join(repo, "packages/ai/src/types.ts"), "utf8");
if (/^import /m.test(types)) throw new Error("packages/ai/src/types.ts imports other modules; it must stay self-contained");
writeFileSync(join(src, "ai-types.ts"), types);

const version = /PLUGIN_API_VERSION = "([^"]+)"/.exec(api)?.[1];
if (!version) throw new Error("PLUGIN_API_VERSION not found");

await $`bun build ${join(src, "index.ts")} --outdir ${out} --target node --format esm`.quiet();
writeFileSync(
  join(out, "tsconfig.json"),
  JSON.stringify({
    compilerOptions: {
      target: "es2022",
      module: "esnext",
      moduleResolution: "bundler",
      allowImportingTsExtensions: true,
      rewriteRelativeImportExtensions: true,
      declaration: true,
      emitDeclarationOnly: true,
      strict: true,
      skipLibCheck: true,
      types: [],
      outDir: ".",
      rootDir: "src",
    },
    files: ["src/index.ts"],
  }),
);
await $`${join(repo, "node_modules/.bin/tsc")} -p ${join(out, "tsconfig.json")}`;
rmSync(join(out, "tsconfig.json"));
// Declarations are read by any TypeScript setup: point them at the emitted .d.ts, not the source.
const dts = join(out, "index.d.ts");
writeFileSync(dts, readFileSync(dts, "utf8").replaceAll('from "./ai-types.ts"', 'from "./ai-types.js"'));
rmSync(src, { recursive: true });

const pkg = JSON.parse(readFileSync(join(repo, "packages/plugin-api/package.json"), "utf8"));
writeFileSync(
  join(out, "package.json"),
  `${JSON.stringify(
    {
      name: pkg.name,
      version,
      description: pkg.description,
      type: "module",
      main: "index.js",
      types: "index.d.ts",
      exports: { ".": { types: "./index.d.ts", default: "./index.js" } },
      files: ["index.js", "index.d.ts", "ai-types.d.ts"],
      license: pkg.license,
      repository: pkg.repository,
      homepage: "https://github.com/sasanokusa/sasacode/blob/main/docs/plugins.md",
      keywords: ["sasacode", "plugin", "coding-agent"],
      publishConfig: { access: "public" },
    },
    null,
    2,
  )}\n`,
);
cpSync(join(repo, "LICENSE"), join(out, "LICENSE"));
writeFileSync(
  join(out, "README.md"),
  `# @sasacode/plugin-api\n\nTypes and helpers for writing [sasacode](https://github.com/sasanokusa/sasacode) plugins (plugin API ${version}).\n\nPlugins run inside sasacode, which provides this module at runtime; install it only for types and editor support:\n\n\`\`\`bash\nnpm install --save-dev @sasacode/plugin-api\n\`\`\`\n\nSee the [plugin guide](https://github.com/sasanokusa/sasacode/blob/main/docs/plugins.md).\n`,
);
console.log(`built ${out} (${pkg.name}@${version})`);
