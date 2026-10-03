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

const windows = process.platform === "win32";

/** Whether this Windows CPU has AVX2 (kernel32's PF_AVX2_INSTRUCTIONS_AVAILABLE); true when that cannot be told. */
function windowsHasAvx2() {
  const kernel32 = `[DllImport("kernel32.dll")] public static extern bool IsProcessorFeaturePresent(int f);`;
  const script = `(Add-Type -MemberDefinition '${kernel32}' -Name K -Namespace SasacodeCpu -PassThru)::IsProcessorFeaturePresent(40)`;
  return spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], { encoding: "utf8", windowsHide: true }).stdout?.trim() !== "False";
}

function target() {
  const os = { darwin: "darwin", linux: "linux", win32: "windows" }[process.platform];
  if (!os) die(`unsupported OS: ${process.platform}`);
  let arch = { x64: "x64", arm64: "arm64" }[process.arch];
  if (!arch) die(`unsupported CPU: ${process.arch}`);
  if (os === "windows") return arch === "x64" && !windowsHasAvx2() ? "windows-x64-baseline" : `windows-${arch}`;
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
    if (!spawnSync(join(tmp, binary), ["--version"], { encoding: "utf8", windowsHide: true }).stdout?.trim()) die("the downloaded binary does not run on this system");
    renameSync(join(tmp, binary), dest); // atomic: a second run at the same time finds a whole file or none
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
}

const name = `sasacode-${target()}`;
const dir = join(process.env.SASACODE_HOME ?? join(homedir(), ".sasacode"), "npm-bin", `v${version}`);
const bin = join(dir, windows ? `${name}.exe` : name);
if (!existsSync(bin)) {
  mkdirSync(dir, { recursive: true });
  await install(name, bin);
}
// `sasacode update` would replace this cached binary; with npm, npm does the updating.
const run = spawnSync(bin, process.argv.slice(2), { stdio: "inherit", env: { ...process.env, SASACODE_INSTALLED_BY: "npm" } });
if (run.error) die(run.error.message);
process.exit(run.status ?? 128 + (constants.signals[run.signal] ?? 0));
