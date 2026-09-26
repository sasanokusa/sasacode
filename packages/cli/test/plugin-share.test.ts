import { afterAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { apiRangeFor, checkPackageDir, describeNpm, describeSource, formatListings, parseSpec, searchPlugins, stagePackage } from "../src/plugin-share.ts";

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
        "1.1.0": { description: "does things", maintainers: [{ name: "alice" }], dist: { fileCount: 3, unpackedSize: 4096 }, dependencies: { chalk: "^5" }, scripts: { postinstall: "node x.js" } },
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
  expect(lines).toContain('no "sasacode" field');
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
  expect(apiRangeFor(`api.on("permission", () => {})`)).toBe("^1.7.0");
  expect(apiRangeFor(`api.registerTool({})`)).toBe("^1.4.0");
  expect(describeSource("// quick — does a quick thing\nexport default () => {}")).toBe("does a quick thing");
  expect(describeSource("export default () => {}")).toBeUndefined();
  // Imports first, then a header whose first line is only the name.
  expect(describeSource(`import fs from "node:fs";\n/**\n * tidy\n *\n * Keeps the notes tidy.\n */\nexport default () => {};`, "tidy")).toBe("Keeps the notes tidy.");
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
