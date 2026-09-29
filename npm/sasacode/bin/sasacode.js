#!/usr/bin/env node
// sasacode from npm: a launcher, not the program. The first run downloads the single binary of
// this package's version for this machine from GitHub Releases, checks it against the release's
// SHA256SUMS, keeps it under ~/.sasacode/npm-bin, and every run then starts it. The platform rules
// are the ones scripts/install.sh uses.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { constants, homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const version = JSON.parse(readFileSync(join(here, "..", "package.json"), "utf8")).version;
const base = (process.env.SASACODE_DOWNLOAD_BASE ?? "https://github.com/sasanokusa/sasacode/releases").replace(/\/$/, "");

function die(message) {
  process.stderr.write(`sasacode (npm): ${message}\n`);
  process.exit(1);
}

function target() {
  const os = { darwin: "darwin", linux: "linux" }[process.platform];
  if (!os) die(process.platform === "win32" ? "Windows is supported through WSL: install sasacode inside a WSL shell" : `unsupported OS: ${process.platform}`);
  let arch = { x64: "x64", arm64: "arm64" }[process.arch];
  if (!arch) die(`unsupported CPU: ${process.arch}`);
  // An x64 Node under Rosetta on Apple Silicon should still get the native build.
  if (os === "darwin" && arch === "x64" && spawnSync("sysctl", ["-n", "sysctl.proc_translated"], { encoding: "utf8" }).stdout?.trim() === "1") arch = "arm64";
  if (os === "linux") {
    const musl = existsSync("/etc/alpine-release") || spawnSync("sh", ["-c", "ls /lib/ld-musl-*"], { stdio: "ignore" }).status === 0;
    if (musl) return `${os}-${arch}-musl`;
    if (arch === "x64" && !/\bavx2\b/.test(readSafe("/proc/cpuinfo"))) return `${os}-${arch}-baseline`; // CPUs without AVX2
  }
  return `${os}-${arch}`;
}

function readSafe(path) {
  try {
    return readFileSync(path, "utf8");
  } catch {
    return "";
  }
}

async function download(url) {
  const res = await fetch(url);
  if (!res.ok) die(`download failed: ${url} (${res.status})`);
  return Buffer.from(await res.arrayBuffer());
}

async function install(name, dest) {
  const url = `${base}/download/v${version}`;
  process.stderr.write(`sasacode (npm): downloading sasacode v${version} (${name.replace(/^sasacode-/, "")}) from ${url}\n`);
  const [archive, sums] = await Promise.all([download(`${url}/${name}.tar.gz`), download(`${url}/SHA256SUMS`)]);
  const expected = sums.toString("utf8").split("\n").find((l) => l.endsWith(` ${name}.tar.gz`))?.split(/\s+/)[0];
  if (!expected) die(`${name}.tar.gz is not listed in SHA256SUMS`);
  if (createHash("sha256").update(archive).digest("hex") !== expected) die(`checksum mismatch for ${name}.tar.gz`);
  const tmp = mkdtempSync(join(dirname(dest), ".download-"));
  try {
    writeFileSync(join(tmp, `${name}.tar.gz`), archive);
    if (spawnSync("tar", ["-xzf", `${name}.tar.gz`], { cwd: tmp }).status !== 0) die(`could not unpack ${name}.tar.gz`);
    chmodSync(join(tmp, name), 0o755);
    if (!spawnSync(join(tmp, name), ["--version"], { encoding: "utf8" }).stdout?.trim()) die("the downloaded binary does not run on this system");
    renameSync(join(tmp, name), dest); // atomic: a second run at the same time finds a whole file or none
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
}

const name = `sasacode-${target()}`;
const dir = join(process.env.SASACODE_HOME ?? join(homedir(), ".sasacode"), "npm-bin", `v${version}`);
const bin = join(dir, name);
if (!existsSync(bin)) {
  mkdirSync(dir, { recursive: true });
  await install(name, bin);
}
// `sasacode update` would replace this cached binary; with npm, npm does the updating.
const run = spawnSync(bin, process.argv.slice(2), { stdio: "inherit", env: { ...process.env, SASACODE_INSTALLED_BY: "npm" } });
if (run.error) die(run.error.message);
process.exit(run.status ?? 128 + (constants.signals[run.signal] ?? 0));
