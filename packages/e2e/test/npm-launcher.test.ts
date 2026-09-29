// The npm package's launcher (npm/sasacode), run with Node against a fake release server: it must
// fetch the binary for this machine once, refuse one whose checksum is wrong, and then behave as
// the binary itself (arguments in, exit code out).
import { afterAll, beforeAll, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const TARGETS = ["darwin-arm64", "darwin-x64", "linux-x64", "linux-x64-baseline", "linux-arm64", "linux-x64-musl", "linux-arm64-musl"];
const root = mkdtempSync(join(tmpdir(), "sasacode-npm-"));
const pkg = join(root, "pkg");
let server: ReturnType<typeof Bun.serve>;
let downloads = 0;
let corrupt = false;

beforeAll(() => {
  // The package as npm installs it, at a version the fake server has.
  mkdirSync(join(pkg, "bin"), { recursive: true });
  cpSync(resolve(import.meta.dir, "../../../npm/sasacode/bin/sasacode.js"), join(pkg, "bin", "sasacode.js"));
  writeFileSync(join(pkg, "package.json"), JSON.stringify({ name: "sasacode", version: "9.9.9", type: "module" }));
  // A "binary" per target: a script that reports its arguments and exits with a chosen code.
  const rel = join(root, "release");
  mkdirSync(rel);
  const sums: string[] = [];
  for (const t of TARGETS) {
    const name = `sasacode-${t}`;
    writeFileSync(join(rel, name), `#!/bin/sh\n[ "$1" = --version ] && { echo 9.9.9; exit 0; }\necho "args:$* by:$SASACODE_INSTALLED_BY"\n[ "$1" = fail ] && exit 3\nexit 0\n`, { mode: 0o755 });
    Bun.spawnSync(["tar", "czf", `${name}.tar.gz`, name], { cwd: rel });
    sums.push(`${createHash("sha256").update(readFileSync(join(rel, `${name}.tar.gz`))).digest("hex")}  ${name}.tar.gz`);
  }
  writeFileSync(join(rel, "SHA256SUMS"), `${sums.join("\n")}\n`);
  server = Bun.serve({
    port: 0,
    fetch(req) {
      const file = new URL(req.url).pathname.match(/^\/download\/v9\.9\.9\/(.+)$/)?.[1];
      if (!file || !existsSync(join(rel, file))) return new Response("not found", { status: 404 });
      if (file.endsWith(".tar.gz")) downloads++;
      const body = readFileSync(join(rel, file));
      return new Response(corrupt && file.endsWith(".tar.gz") ? Buffer.concat([body, Buffer.from("x")]) : body);
    },
  });
});
afterAll(() => {
  server.stop(true);
  rmSync(root, { recursive: true, force: true });
});

// Asynchronous: the fake server runs in this process and must keep answering.
async function launch(home: string, args: string[]) {
  const child = Bun.spawn(["node", join(pkg, "bin", "sasacode.js"), ...args], {
    env: { PATH: process.env.PATH ?? "", HOME: root, SASACODE_HOME: home, SASACODE_DOWNLOAD_BASE: `http://127.0.0.1:${server.port}` },
    stdout: "pipe",
    stderr: "pipe",
  });
  const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
  return { code, stdout, stderr };
}

test("the first run fetches the binary for this machine; later runs start it without downloading", async () => {
  const home = join(root, "home-a");
  const first = await launch(home, ["-p", "hello world"]);
  expect(first.stderr).toContain("downloading sasacode v9.9.9");
  expect(first.stdout.trim()).toBe("args:-p hello world by:npm");
  expect(first.code).toBe(0);
  expect(readdirSync(join(home, "npm-bin", "v9.9.9"))).toHaveLength(1);

  const before = downloads;
  const again = await launch(home, ["fail"]);
  expect(downloads).toBe(before);
  expect(again.stderr).not.toContain("downloading");
  expect(again.code).toBe(3); // the binary's exit code comes through
});

test("a download whose checksum does not match is refused and nothing is kept", async () => {
  corrupt = true;
  try {
    const home = join(root, "home-b");
    const r = await launch(home, ["--version"]);
    expect(r.code).toBe(1);
    expect(r.stderr).toContain("checksum mismatch");
    expect(readdirSync(join(home, "npm-bin", "v9.9.9"))).toEqual([]);
  } finally {
    corrupt = false;
  }
});
