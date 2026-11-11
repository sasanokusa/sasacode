import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setLang } from "@sasacode/host";
import { PLUGIN_API_VERSION } from "@sasacode/plugin-api";
import { API_FEATURES, apiRangeFor, checkPackageDir, describeNpm, describeSource, formatListings, packageNameFor, parseSpec, publishedLatest, searchPlugins, stagePackage } from "../src/plugin-share.ts";

beforeAll(() => setLang("en")); // these assertions quote the English strings

const root = mkdtempSync(join(tmpdir(), "sasacode-share-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

test("search joins the curated list and npm's keyword search, recommended first, without duplicates", async () => {
  const urls: string[] = [];
  const doFetch = (async (url: string) => {
    urls.push(url);
    if (url.endsWith("plugins.json"))
      return json({ plugins: [{ name: "goal-plus", source: "sasacode-plugin-goal-plus", description: "goals with deadlines", author: "sasa" }, { name: "other", source: "x", description: "unrelated" }, { bad: true }] });
    return json({
      objects: [
        { package: { name: "sasacode-plugin-goal-plus", version: "1.0.0", description: "goals with deadlines" } },
        { package: { name: "sasacode-plugin-goal-timer", version: "0.2.0", description: "a goal timer", publisher: { username: "someone" } } },
      ],
    });
  }) as unknown as typeof fetch;
  const { listings, errors } = await searchPlugins("goal", doFetch);
  expect(errors).toEqual([]);
  expect(listings.map((l) => [l.name, !!l.recommended])).toEqual([["goal-plus", true], ["sasacode-plugin-goal-timer", false]]);
  expect(decodeURIComponent(urls.find((u) => u.includes("/-/v1/search"))!)).toContain("keywords:sasacode-plugin goal");
  expect(formatListings(listings)).toContain("★ goal-plus  by sasa\n    goals with deadlines\n    sasacode plugin install sasacode-plugin-goal-plus");
});

test("search still answers when one source is down", async () => {
  const doFetch = (async (url: string) => (url.endsWith("plugins.json") ? json({}, 503) : json({ objects: [{ package: { name: "p", version: "1.0.0" } }] }))) as unknown as typeof fetch;
  const { listings, errors } = await searchPlugins("", doFetch);
  expect(listings.map((l) => l.name)).toEqual(["p"]);
  expect(errors[0]).toMatch(/^list: .*503/);
});

test("before installing, the package is described: version, publisher, size, and what looks wrong", async () => {
  expect(parseSpec("@me/tool@^1.2")).toEqual({ name: "@me/tool", range: "^1.2" });
  expect(parseSpec("tool")).toEqual({ name: "tool" });
  let asked = "";
  const doFetch = (async (url: string) => {
    asked = url;
    return json({
      "dist-tags": { latest: "1.1.0" },
      time: { "1.1.0": "2026-09-20T00:00:00Z" },
      versions: {
        "1.1.0": { description: "does things", maintainers: [{ name: "alice" }], dist: { fileCount: 3, unpackedSize: 4096 }, dependencies: { chalk: "^5" }, scripts: { postinstall: "node x.js" }, sasacode: { name: "tool" } },
      },
    });
  }) as unknown as typeof fetch;
  const lines = (await describeNpm("@me/tool", doFetch)).join("\n");
  expect(asked).toEndWith("/@me%2ftool");
  expect(lines).toContain("@me/tool@1.1.0");
  expect(lines).toContain("published by alice on 2026-09-20");
  expect(lines).toContain("3 files, 4 KB");
  expect(lines).toContain("depends on chalk");
  expect(lines).toContain("install scripts");
});

test("a single file becomes a package: manifest, search keyword, API range from what it uses, next version", () => {
  process.env.SASACODE_HOME = join(root, "home");
  try {
    const file = join(root, "Nice Tool.ts");
    writeFileSync(file, `/**\n * nice-tool — counts the lines of every file it is given\n */\nexport default (api) => api.registerTool({ name: "n", permissionsAs: "bash" });\n`);
    const { dir, pkg, notes } = stagePackage(file, { latest: "0.1.4", license: "MIT" });
    expect(pkg).toMatchObject({
      name: "sasacode-plugin-nice-tool",
      version: "0.1.5",
      description: "counts the lines of every file it is given",
      keywords: ["sasacode-plugin", "sasacode"],
      license: "MIT",
      peerDependencies: { "@sasacode/plugin-api": "^1.6.0" },
      sasacode: { apiVersion: "^1.6.0", extensions: ["index.ts"] },
    });
    expect(notes).toEqual([]);
    expect(readFileSync(join(dir, "index.ts"), "utf8")).toContain("nice-tool");
    expect(readFileSync(join(dir, "README.md"), "utf8")).toContain("sasacode plugin install sasacode-plugin-nice-tool");

    const local = join(root, "uses-local.ts");
    writeFileSync(local, `import { x } from "./helper.ts";\nexport default () => {};\n`);
    const second = stagePackage(local, { name: "@me/uses-local" });
    expect(second.pkg.version).toBe("0.1.0");
    expect(second.notes.join("\n")).toMatch(/imports other local files[\s\S]*no license/);
    expect(() => stagePackage(local, { name: "Bad Name" })).toThrow(/not a valid npm package name/);
  } finally {
    delete process.env.SASACODE_HOME;
  }
});

test("API ranges and descriptions", () => {
  for (const source of ["api.ui.input(\"Name?\")", "api.ui.selectMany(\"x\", [])"]) expect(apiRangeFor(source)).toBe("^1.10.0");
  for (const source of ["ctx.callTool('bash', {})", "api.ui.showText({})", "api.defineSettings({})"]) expect(apiRangeFor(source)).toBe("^1.9.0");
  expect(apiRangeFor(`api.on("permission", () => {})`)).toBe("^1.7.0");
  expect(apiRangeFor(`api.registerTool({})`)).toBe("^1.4.0");
  expect(describeSource("// quick — does a quick thing\nexport default () => {}")).toBe("does a quick thing");
  expect(describeSource("export default () => {}")).toBeUndefined();
  // Imports first, then a header whose first line is only the name.
  expect(describeSource(`import fs from "node:fs";\n/**\n * tidy\n *\n * Keeps the notes tidy.\n */\nexport default () => {};`, "tidy")).toBe("Keeps the notes tidy.");
});

test("the version published next is looked up under the name it is published as (#108)", async () => {
  process.env.SASACODE_HOME = join(root, "home");
  try {
    for (const base of ["my_tool.ts", "Other Tool.ts", "plain.mjs"]) {
      const file = join(root, base);
      writeFileSync(file, "export default () => {};\n");
      expect(packageNameFor(file)).toBe(stagePackage(file).pkg.name);
    }
    const asked: string[] = [];
    const doFetch = (async (url: string) => (asked.push(url), json({ "dist-tags": { latest: "0.1.3" } }))) as unknown as typeof fetch;
    const file = join(root, "my_tool.ts");
    const latest = await publishedLatest(file, undefined, doFetch);
    expect(asked[0]).toEndWith("/sasacode-plugin-my-tool");
    expect(stagePackage(file, { latest }).pkg).toMatchObject({ name: "sasacode-plugin-my-tool", version: "0.1.4" });
    await publishedLatest(file, "@me/tool", doFetch);
    expect(asked[1]).toEndWith("/@me%2ftool");
  } finally {
    delete process.env.SASACODE_HOME;
  }
});

test("the API range follows every API version's features, up to PLUGIN_API_VERSION (#108)", () => {
  const samples: [string, string][] = [
    ["^1.15.0", `api.on("before_request", (e) => { if (e.model.server === "ollama") return {}; });`],
    ["^1.15.0", `if (ctx.model?.server) {}`],
    ["^1.15.0", `if (api.agent.model()?.server === "lmstudio") {}`], // #134
    ["^1.14.0", `api.on("agent_end", (e) => { const { error } = e; });`],
    ["^1.14.0", `api.on("agent_end", (e) => { if (e.cause === "error") log(e.error); });`],
    ["^1.14.0", `api.on("agent_end", ({ cause, error }) => {});`],
    ["^1.13.0", `api.on("usage", ({ usage }) => {});`],
    ["^1.12.0", `api.ui.onPaste((text) => text.trim());`],
    ["^1.11.0", `api.on("before_request", (e) => ({ fetch: wrap(e.fetch) }));`],
    ["^1.10.0", `api.ui.input("Name?")`],
  ];
  for (const [range, source] of samples) expect(`${range} ← ${apiRangeFor(source)}`).toBe(`${range} ← ${range}`);
  // Not the newer features: an unrelated .server, agent_end's cause alone, console.error.
  expect(apiRangeFor(`cb?.server.close();`)).toBe("^1.4.0");
  expect(apiRangeFor(`api.on("agent_end", (e) => { if (e.cause === "error") console.error("x"); });`)).toBe("^1.4.0");
  // Raising PLUGIN_API_VERSION's minor needs a row for it here.
  const [major, minor] = PLUGIN_API_VERSION.split(".");
  expect(API_FEATURES[0]![0]).toBe(`^${major}.${minor}.0`);
});

test("a range is described by the version it installs, not by latest (#108)", async () => {
  const doFetch = (async () =>
    json({
      "dist-tags": { latest: "2.0.0" },
      time: { "1.2.0": "2026-03-01T00:00:00Z", "2.0.0": "2026-06-01T00:00:00Z" },
      versions: {
        "1.0.0": { description: "v1", maintainers: [{ name: "alice" }], sasacode: {} },
        "1.2.0": { description: "v1 line", maintainers: [{ name: "alice" }], dist: { fileCount: 2, unpackedSize: 1024 }, sasacode: {} },
        "2.0.0": { description: "v2 rewrite", maintainers: [{ name: "bob" }], dist: { fileCount: 3, unpackedSize: 2048 } },
      },
    })) as unknown as typeof fetch;
  const lines = (await describeNpm("sasacode-plugin-foo@^1", doFetch)).join("\n");
  expect(lines).toContain("sasacode-plugin-foo@1.2.0 (range ^1");
  expect(lines).toContain("v1 line");
  expect(lines).toContain("published by alice on 2026-03-01");
  expect(lines).toContain("2 files, 1 KB");
  expect(lines).not.toContain("v2 rewrite");
  await expect(describeNpm("sasacode-plugin-foo@^3", doFetch)).rejects.toThrow("no version of sasacode-plugin-foo matches ^3");
  // A dist-tag it does not have is no range, though Bun.semver takes it for `*` (#133).
  for (const tag of ["beta", "latest-x"]) await expect(describeNpm(`sasacode-plugin-foo@${tag}`, doFetch)).rejects.toThrow(`no version of sasacode-plugin-foo matches ${tag}`);
  expect((await describeNpm("sasacode-plugin-foo@1.x", doFetch)).join("\n")).toContain("sasacode-plugin-foo@1.2.0");
  await expect(describeNpm("sasacode-plugin-foo@latest", doFetch)).rejects.toThrow("not a sasacode plugin"); // latest itself still is checked
});

test("a package directory needs the manifest, and gets the search keyword", () => {
  const dir = join(root, "pkgdir");
  mkdirSync(dir);
  writeFileSync(join(dir, "package.json"), JSON.stringify({ name: "p" }));
  expect(() => checkPackageDir(dir)).toThrow(/no "sasacode" field/);
  writeFileSync(join(dir, "package.json"), JSON.stringify({ name: "p", keywords: ["x"], sasacode: { extensions: ["i.ts"] } }));
  expect(checkPackageDir(dir).notes[0]).toContain("keyword");
  expect(JSON.parse(readFileSync(join(dir, "package.json"), "utf8")).keywords).toEqual(["x", "sasacode-plugin"]);
  expect(checkPackageDir(dir).notes).toEqual([]);
});
