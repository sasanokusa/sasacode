// Checks for promises this repository makes about itself that nothing else verifies: the core line
// budget, the record of runtime dependencies, the READMEs staying in step with the CLI flags and the
// bundled plugins, and the docs staying reachable and linked. `bun test` runs them through
// scripts/checks.test.ts (so a change that breaks one fails CI), and `bun run improve` prints them
// next to the numbers the improvement loop starts from (docs/improvement.md).
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { bundledPlugins } from "../packages/bundled/src/index.ts";

export const ROOT = resolve(import.meta.dir, "..");

/** The core (ai, agent, plugin-api, tools) stays under this many lines (README,「開発」). */
export const CORE_LINE_BUDGET = 4000;
export const CORE_PACKAGES = ["ai", "agent", "plugin-api", "tools"] as const;
/** Both READMEs document the CLI and the bundled plugins; a change to one is a change to both. */
export const READMES = ["README.md", "README.en.md"] as const;
/** Rows of the bundled-plugin table that are adapters, not plugins from @sasacode/bundled. */
const TABLE_ONLY = ["skills", "mcp"];
/** Directories that are not part of what a reader of the repository sees (bench/ stays local). */
const SKIPPED = new Set([".git", ".claude", "node_modules", "dist", "bench"]);

const read = (path: string) => readFileSync(path, "utf8");

/** Every file under `dir` whose name `match` accepts, following subdirectories. */
export function walk(dir: string, match: (name: string) => boolean): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      if (!SKIPPED.has(entry.name)) out.push(...walk(join(dir, entry.name), match));
    } else if (match(entry.name)) out.push(join(dir, entry.name));
  }
  return out;
}

/** Lines as `wc -l` counts them: a last line without a newline still counts. */
export function countLines(text: string): number {
  if (!text) return 0;
  return text.split("\n").length - (text.endsWith("\n") ? 1 : 0);
}

export interface CoreLines {
  total: number;
  byPackage: Record<string, number>;
}

export function coreLines(root = ROOT): CoreLines {
  const byPackage: Record<string, number> = {};
  for (const name of CORE_PACKAGES) {
    const dir = join(root, "packages", name, "src");
    byPackage[name] = existsSync(dir)
      ? walk(dir, (file) => file.endsWith(".ts")).reduce((sum, file) => sum + countLines(read(file)), 0)
      : 0;
  }
  return { total: Object.values(byPackage).reduce((sum, n) => sum + n, 0), byPackage };
}

/** Runtime dependencies of the workspace packages, excluding the ones on each other. */
export function runtimeDependencies(root = ROOT): string[] {
  const names = new Set<string>();
  const dir = join(root, "packages");
  if (!existsSync(dir)) return [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const manifest = join(dir, entry.name, "package.json");
    if (!existsSync(manifest)) continue;
    const pkg = JSON.parse(read(manifest)) as { dependencies?: Record<string, string> };
    for (const name of Object.keys(pkg.dependencies ?? {})) if (!name.startsWith("@sasacode/")) names.add(name);
  }
  return [...names].sort();
}

/** The first column of the table in docs/dependencies.md, which records a reason for each one. */
export function documentedDependencies(root = ROOT): string[] {
  const path = join(root, "docs", "dependencies.md");
  if (!existsSync(path)) return [];
  return [...read(path).matchAll(/^\|\s*`([^`]+)`\s*\|/gm)].map((m) => m[1]!);
}

export interface CliOption {
  name: string;
  short?: string;
}

/**
 * The options the CLI offers: the ones parseArgs accepts, plus the ones only the help text lists
 * (subcommand flags such as `--project`). Read from the source so the check needs no process.
 */
export function cliOptions(root = ROOT): CliOption[] {
  const path = join(root, "packages", "cli", "src", "main.ts");
  if (!existsSync(path)) return [];
  const source = read(path);
  const byName = new Map<string, CliOption>();
  for (const m of source.matchAll(/^\s{6}"?([a-z][a-z0-9-]*)"?:\s*\{\s*type:\s*"(?:string|boolean)"(?:,\s*short:\s*"(\w)")?/gm)) {
    byName.set(m[1]!, { name: m[1]!, short: m[2] });
  }
  const help = /const HELP_TEXT = `([\s\S]*?)`;/.exec(source)?.[1] ?? "";
  for (const m of help.matchAll(/^\s+(?:-(\w), )?--([a-z][a-z0-9-]*)/gm)) {
    if (!byName.has(m[2]!)) byName.set(m[2]!, { name: m[2]!, short: m[1] });
  }
  return [...byName.values()];
}

/** Options `text` never mentions, counting `-c` as documenting `--continue` (the tables use shorts). */
export function undocumentedOptions(text: string, options: CliOption[]): string[] {
  return options
    .filter((option) => {
      if (text.includes(`--${option.name}`)) return false;
      return !(option.short && new RegExp(`(^|[\\s\`|(/])-${option.short}(?![\\w-])`).test(text));
    })
    .map((option) => `--${option.name}`);
}

/** Plugin names in the table under `heading`, reading `` `a` / `b` `` rows as two names. */
export function tableNames(text: string, heading: string): string[] {
  const at = text.indexOf(heading);
  if (at < 0) return [];
  const rest = text.slice(at + heading.length);
  const end = rest.search(/^#{2,3} /m);
  const section = end < 0 ? rest : rest.slice(0, end);
  return [...section.matchAll(/^\|\s*([^|]+?)\s*\|/gm)]
    .map((m) => m[1]!.trim())
    .filter((cell) => cell.length > 0 && !/^-+$/.test(cell) && cell !== "名前" && cell !== "Name")
    .flatMap((cell) => cell.split("/").map((part) => part.replace(/`/g, "").trim()));
}

/** Relative links in the markdown that point at nothing. */
export function brokenLinks(root = ROOT): string[] {
  const broken: string[] = [];
  for (const file of walk(root, (name) => name.endsWith(".md"))) {
    // Fenced blocks are examples, not prose: a link-looking string inside one is not a link.
    const text = read(file).replace(/```[\s\S]*?```/g, "");
    for (const m of text.matchAll(/\[[^\]]*\]\(([^)\s]+)\)/g)) {
      const target = m[1]!;
      if (/^[a-z][a-z0-9+.-]*:/i.test(target) || target.startsWith("#") || target.startsWith("/")) continue;
      const path = decodeURIComponent(target.split("#")[0]!);
      if (path && !existsSync(resolve(dirname(file), path))) broken.push(`${relative(root, file)}: ${target}`);
    }
  }
  return broken;
}

/** Docs in docs/ that neither README links to: written down, but found by nobody. */
export function unreachableDocs(root = ROOT): string[] {
  const dir = join(root, "docs");
  if (!existsSync(dir)) return [];
  const readmes = READMES.map((name) => (existsSync(join(root, name)) ? read(join(root, name)) : ""));
  return readdirSync(dir)
    .filter((name) => name.endsWith(".md"))
    .filter((name) => !readmes.every((text) => text.includes(name)))
    .map((name) => `docs/${name}`);
}

export interface CheckResult {
  /** Short name, shown by `bun run improve`. */
  name: string;
  /** What the check keeps true, in one line. */
  promise: string;
  /** What is wrong; empty means the check passed. */
  problems: string[];
}

export function runChecks(root = ROOT): CheckResult[] {
  const lines = coreLines(root);
  const used = runtimeDependencies(root);
  const documented = documentedDependencies(root);
  const options = cliOptions(root);
  const plugins = Object.keys(bundledPlugins);
  const readmes = READMES.map((name) => ({ name, text: existsSync(join(root, name)) ? read(join(root, name)) : "" }));

  return [
    {
      name: "core-line-budget",
      promise: `the core (${CORE_PACKAGES.join(", ")}) stays under ${CORE_LINE_BUDGET} lines`,
      problems:
        lines.total > CORE_LINE_BUDGET
          ? [`the core is ${lines.total} lines, ${lines.total - CORE_LINE_BUDGET} over the budget of ${CORE_LINE_BUDGET}`]
          : [],
    },
    {
      name: "dependency-record",
      promise: "every runtime dependency of a package has a row with a reason in docs/dependencies.md",
      problems: [
        ...used.filter((name) => !documented.includes(name)).map((name) => `${name} is used but has no row in docs/dependencies.md`),
        ...documented.filter((name) => !used.includes(name)).map((name) => `${name} has a row in docs/dependencies.md but nothing depends on it`),
      ],
    },
    {
      name: "readme-cli",
      promise: "both READMEs mention every option the CLI offers",
      problems: readmes.flatMap(({ name, text }) => undocumentedOptions(text, options).map((option) => `${name} never mentions ${option}`)),
    },
    {
      name: "readme-plugins",
      promise: "both READMEs list exactly the plugins that ship with sasacode",
      problems: readmes.flatMap(({ name, text }) => {
        const listed = tableNames(text, name === "README.md" ? "### 同梱プラグイン" : "### Bundled plugins");
        return [
          ...plugins.filter((plugin) => !listed.includes(plugin)).map((plugin) => `${name} does not list the bundled plugin ${plugin}`),
          ...listed
            .filter((plugin) => !plugins.includes(plugin) && !TABLE_ONLY.includes(plugin))
            .map((plugin) => `${name} lists ${plugin}, which ships with nobody`),
        ];
      }),
    },
    {
      name: "docs-links",
      promise: "relative links in the markdown resolve",
      problems: brokenLinks(root).map((link) => `broken link ${link}`),
    },
    {
      name: "docs-reachable",
      promise: "both READMEs link every doc in docs/",
      problems: unreachableDocs(root).map((doc) => `${doc} is linked from no README`),
    },
  ];
}
