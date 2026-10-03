#!/usr/bin/env node
// sasacode from npm: a launcher, not the program. The first run downloads the single binary of
// this package's version for this machine from GitHub Releases, checks it against the release's
// SHA256SUMS, keeps it under ~/.sasacode/npm-bin, and every run then starts it. The platform rules
// are the ones scripts/install.sh uses.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
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

const windows = process.platform === "win32";

function target() {
  const os = { darwin: "darwin", linux: "linux", win32: "windows" }[process.platform];
  if (!os) die(`unsupported OS: ${process.platform}`);
  let arch = { x64: "x64", arm64: "arm64" }[process.arch];
  if (!arch) die(`unsupported CPU: ${process.arch}`);
  // Windows has no cheap CPU check (PowerShell is slow, and can hang without a full environment):
  // the x64 build is tried first and the baseline one taken when it does not run (no AVX2).
  if (os === "windows") return `windows-${arch}`;
  // An x64 Node under Rosetta on Apple Silicon should still get the native build.
  if (os === "darwin" && arch === "x64" && spawnSync("sysctl", ["-n", "sysctl.proc_translated"], { encoding: "utf8" }).stdout?.trim() === "1") arch = "arm64";
  if (os === "linux") {
    const musl = existsSync("/etc/alpine-release") || spawnSync("sh", ["-c", "ls /lib/ld-musl-*"], { stdio: "ignore" }).status === 0;
    if (musl) return `${os}-${arch}-musl`;
    if (arch === "x64" && !/\bavx2\b/.test(readSafe("/proc/cpuinfo"))) return `${os}-${arch}-baseline`; // CPUs without AVX2
  }
  return `${os}-${arch}`;
}

function readdirSafe(path) {
  try {
    return readdirSync(path);
  } catch {
    return [];
  }
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

/** Download, check and keep the binary `name` at `dest`. With `mayNotRun`, false when it does not run here. */
async function install(name, dest, mayNotRun = false) {
  const url = `${base}/download/v${version}`;
  // Windows releases are a .zip holding an .exe; the rest a .tar.gz.
  const file = windows ? `${name}.zip` : `${name}.tar.gz`;
  const binary = windows ? `${name}.exe` : name;
  process.stderr.write(`sasacode (npm): downloading sasacode v${version} (${name.replace(/^sasacode-/, "")}) from ${url}\n`);
  const [archive, sums] = await Promise.all([download(`${url}/${file}`), download(`${url}/SHA256SUMS`)]);
  const expected = sums.toString("utf8").split(/\r?\n/).find((l) => l.endsWith(` ${file}`))?.split(/\s+/)[0];
  if (!expected) die(`${file} is not listed in SHA256SUMS`);
  if (createHash("sha256").update(archive).digest("hex") !== expected) die(`checksum mismatch for ${file}`);
  const tmp = mkdtempSync(join(dirname(dest), ".download-"));
  try {
    writeFileSync(join(tmp, file), archive);
    // Windows' own tar (bsdtar) reads .zip; a GNU tar earlier on the PATH (Git's) would not.
    const tar = windows ? join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe") : "tar";
    if (spawnSync(tar, [windows ? "-xf" : "-xzf", file], { cwd: tmp, windowsHide: true }).status !== 0) die(`could not unpack ${file}`);
    chmodSync(join(tmp, binary), 0o755);
    if (!spawnSync(join(tmp, binary), ["--version"], { encoding: "utf8", windowsHide: true }).stdout?.trim()) {
      if (mayNotRun) return false;
      die("the downloaded binary does not run on this system");
    }
    renameSync(join(tmp, binary), dest); // atomic: a second run at the same time finds a whole file or none
    return true;
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
}

const dir = join(process.env.SASACODE_HOME ?? join(homedir(), ".sasacode"), "npm-bin", `v${version}`);
// On Windows the build kept for this version is whichever one ran here (x64 or its baseline).
const kept = windows ? readdirSafe(dir).find((f) => /^sasacode-windows-[\w-]+\.exe$/.test(f)) : undefined;
let bin = kept && join(dir, kept);
if (!bin) {
  const name = `sasacode-${target()}`;
  bin = join(dir, windows ? `${name}.exe` : name);
  if (!existsSync(bin)) {
    mkdirSync(dir, { recursive: true });
    if (!(await install(name, bin, name === "sasacode-windows-x64"))) {
      bin = join(dir, "sasacode-windows-x64-baseline.exe"); // a CPU without AVX2
      await install("sasacode-windows-x64-baseline", bin);
    }
  }
}
// `sasacode update` would replace this cached binary; with npm, npm does the updating.
const run = spawnSync(bin, process.argv.slice(2), { stdio: "inherit", env: { ...process.env, SASACODE_INSTALLED_BY: "npm" } });
if (run.error) die(run.error.message);
process.exit(run.status ?? 128 + (constants.signals[run.signal] ?? 0));
