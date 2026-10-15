// The repository's own checks, plus tests of the checks themselves: a check that silently stops
// working is worse than no check. The repository-wide case is the one that fails CI.
import { expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import {
  brokenLinks,
  cliOptions,
  coreLines,
  countLines,
  readmeShapeProblem,
  readmeStructure,
  documentedDependencies,
  runtimeDependencies,
  runChecks,
  tableNames,
  undocumentedOptions,
  unreachableDocs,
} from "./checks.ts";

/** A throwaway repository root holding `files`, keyed by path relative to it. */
function fixture(files: Record<string, string>): string {
  const root = mkdtempSync(join(tmpdir(), "sasacode-check-"));
  for (const [path, body] of Object.entries(files)) {
    mkdirSync(join(root, dirname(path)), { recursive: true });
    writeFileSync(join(root, path), body);
  }
  return root;
}

test("every check passes in this repository", () => {
  const problems = runChecks().flatMap((check) => check.problems.map((problem) => `${check.name}: ${problem}`));
  expect(problems).toEqual([]);
});

test("countLines counts a last line without a newline", () => {
  expect(countLines("")).toBe(0);
  expect(countLines("a\n")).toBe(1);
  expect(countLines("a\nb")).toBe(2);
  expect(countLines("a\nb\n")).toBe(2);
});

test("coreLines adds up src/*.ts of the core packages, subdirectories included", () => {
  const root = fixture({
    "packages/ai/src/a.ts": "one\ntwo\nthree\n",
    "packages/ai/src/nested/b.ts": "one\ntwo", // no trailing newline: still two lines
    "packages/agent/src/c.ts": "one\n",
    "packages/tools/src/d.ts": "one\ntwo\nthree\nfour\n",
    "packages/ai/test/a.test.ts": "one\ntwo\nthree\nfour\nfive\n", // tests are not core
    "packages/cli/src/main.ts": "one\n",
  });
  expect(coreLines(root)).toEqual({ total: 10, byPackage: { ai: 5, agent: 1, "plugin-api": 0, tools: 4 } });
});

test("runtimeDependencies skips the packages depending on each other", () => {
  const root = fixture({
    "packages/ai/package.json": JSON.stringify({ dependencies: { "@anthropic-ai/sdk": "^1", openai: "^1", "@sasacode/plugin-api": "workspace:*" } }),
    "packages/tui/package.json": JSON.stringify({ dependencies: { "@earendil-works/pi-tui": "^1" }, devDependencies: { "@sasacode/ai": "workspace:*" } }),
  });
  expect(runtimeDependencies(root)).toEqual(["@anthropic-ai/sdk", "@earendil-works/pi-tui", "openai"]);
  expect(documentedDependencies(fixture({ "docs/dependencies.md": "| `openai` | where | why | instead |\n| --- | --- | --- | --- |\n" }))).toEqual(["openai"]);
});

test("a README that misses a flag is reported", () => {
  const root = fixture({
    "packages/cli/src/main.ts": `const HELP_TEXT = \`sasacode
Options:
  -p, --print <prompt>           headless
      --max-turns <n>            turns
  -h, --help
\`;
const { values } = parseArgs({ options: {
      print: { type: "string", short: "p" },
      "max-turns": { type: "string" },
      help: { type: "boolean", short: "h" },
      "trust-project": { type: "boolean" },
}});
`,
    "README.md": "| `-p <prompt>` | headless |\n| `--max-turns <n>` | turns |\n",
    "README.en.md": "| `-p <prompt>` | headless |\n| `--max-turns` | turns |\n| `-h` | help |\n",
  });
  const options = cliOptions(root);
  expect(options.map((o) => o.name).sort()).toEqual(["help", "max-turns", "print", "trust-project"]);
  expect(options.find((o) => o.name === "print")?.short).toBe("p");
  const problems = runChecks(root).find((check) => check.name === "readme-cli")!.problems;
  expect(problems).toEqual(["README.md never mentions --help", "README.md never mentions --trust-project", "README.en.md never mentions --trust-project"]);
});

test("the bundled-plugin table reads names, including `` `a` / `b` `` rows", () => {
  const text = ["### Bundled plugins", "", "| Name | What |", "| --- | --- |", "| `todo` | plan |", "| `skills` / `mcp` | adapters |", "", "### MCP", "| `other` | x |"].join("\n");
  expect(tableNames(text, "### Bundled plugins")).toEqual(["todo", "skills", "mcp"]);
});

test("readmeStructure sees headings and opening fences, and ignores both inside a block", () => {
  const text = ["# Title", "## One", "```bash", "# not a heading", "```", "### Two", "```", "```", "#### Three"].join("\n");
  expect(readmeStructure(text)).toEqual(["h1", "h2", "code:bash", "h3", "code:", "h4"]);
});

test("a README that drifts shape from the other is reported", () => {
  const same = { "README.md": "# a\n## b\n```sh\nx\n```\n", "README.en.md": "# A\n## B\n```sh\ny\n```\n" };
  expect(readmeShapeProblem(fixture(same))).toEqual([]);
  expect(readmeShapeProblem(fixture({ ...same, "README.en.md": "# A\n```sh\ny\n```\n" }))).toEqual([
    "README.md has 3 headings and code blocks, README.en.md has 2",
  ]);
  expect(readmeShapeProblem(fixture({ ...same, "README.en.md": "# A\n### B\n```sh\ny\n```\n" }))).toEqual([
    "the two READMEs part ways at block 2: h2 vs h3",
  ]);
});

test("a broken relative link and an unlinked doc are reported", () => {
  const root = fixture({
    "README.md": "[a](docs/here.md) [b](docs/gone.md) [c](https://example.com/missing) [d](#anchor)\n```\n[e](docs/in-a-fence.md)\n```\n",
    "README.en.md": "[a](docs/here.md)\n",
    "docs/here.md": "[up](../README.md)\n",
    "docs/orphan.md": "# nothing links to me\n",
  });
  expect(brokenLinks(root)).toEqual(["README.md: docs/gone.md"]);
  expect(unreachableDocs(root)).toEqual(["docs/orphan.md"]);
});

test("an option with a short form counts as documented when the README uses the short form", () => {
  expect(undocumentedOptions("| `-c` / `-r <id>` / `--no-session` |", [{ name: "continue", short: "c" }, { name: "resume", short: "r" }, { name: "no-session" }])).toEqual([]);
  expect(undocumentedOptions("sasacode --version", [{ name: "version", short: "v" }, { name: "help", short: "h" }])).toEqual(["--help"]);
});
