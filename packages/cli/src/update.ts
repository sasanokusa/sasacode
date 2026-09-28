// `sasacode update`: the installer's download and checks, run against the binary in use. The
// installer script (scripts/install.sh) is the reference for the target names and the checksum.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, copyFileSync, existsSync, readFileSync, renameSync, rmSync, writeFileSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { t } from "@sasacode/host";
import { sasacodeHome } from "./config.ts";

const VERSION = (await import("../package.json")).version;
const DOWNLOAD_BASE = () => process.env.SASACODE_DOWNLOAD_BASE ?? "https://github.com/sasanokusa/sasacode/releases";
/** The newest release is asked for this often; a check is cheap and the answer rarely changes. */
const CHECK_EVERY_MS = 24 * 60 * 60_000;

export function currentVersion(): string {
  return VERSION;
}

/** "v0.9.5" vs "0.9.4" → 1. Versions are numbers; anything unparsable compares as 0. */
export function compareVersions(a: string, b: string): number {
  const x = a.replace(/^v/, "").split(".").map(Number);
  const y = b.replace(/^v/, "").split(".").map(Number);
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const d = (x[i] || 0) - (y[i] || 0);
    if (d) return d > 0 ? 1 : -1;
  }
  return 0;
}

/**
 * The newest release tag, read from the redirect of …/latest/download/…: the releases page can
 * lag behind the actual latest release for a while (scripts/install.sh pins versions the same way).
 */
export async function latestRelease(base = DOWNLOAD_BASE()): Promise<string> {
  const r = await fetch(`${base}/latest/download/SHA256SUMS`, { method: "HEAD", redirect: "manual" });
  const m = /\/download\/([^/]+)\/SHA256SUMS/.exec(r.headers.get("location") ?? "");
  if (!m) throw new Error(t("cannot tell the latest version ({status} from {base})", { status: r.status, base }));
  return m[1]!;
}

/** The newest release, when it is newer than this build. Asked at most once a day. */
export async function checkForUpdate(): Promise<string | undefined> {
  const cache = join(sasacodeHome(), "update-check.json");
  let checkedAt = 0;
  try {
    checkedAt = JSON.parse(readFileSync(cache, "utf8")).checkedAt ?? 0;
  } catch {}
  if (Date.now() - checkedAt < CHECK_EVERY_MS) return undefined;
  // The timestamp is written before the question, so an offline day is not retried on every start.
  try {
    writeFileSync(cache, JSON.stringify({ checkedAt: Date.now() }));
  } catch {} // a read-only home must not break startup
  const latest = await latestRelease();
  return compareVersions(latest, VERSION) > 0 ? latest : undefined;
}

/** A quiet heads-up; nothing at all when the question cannot be answered. */
export async function notifyIfNewer(notify: (message: string, level?: "info" | "warning" | "error") => void): Promise<void> {
  try {
    const latest = await checkForUpdate();
    if (latest) notify(t("a newer version is available: {latest} (this is {version}) — run `sasacode update`", { latest, version: VERSION }));
  } catch {}
}

/** The archive name for this machine, with the platform choices scripts/install.sh makes. */
export function releaseTarget(): string {
  const os = { darwin: "darwin", linux: "linux" }[process.platform as string];
  if (!os) throw new Error(t("unsupported OS: {platform} (Windows is supported through WSL)", { platform: process.platform }));
  let arch = { x64: "x64", arm64: "arm64" }[process.arch as string];
  if (!arch) throw new Error(t("unsupported CPU: {arch}", { arch: process.arch }));
  // An x64 shell under Rosetta on Apple Silicon should still get the native build.
  if (os === "darwin" && arch === "x64") {
    const translated = spawnSync("sysctl", ["-n", "sysctl.proc_translated"], { encoding: "utf8" }).stdout?.trim();
    if (translated === "1") arch = "arm64";
  }
  if (os === "linux") {
    const musl = existsSync("/etc/alpine-release") || spawnSync("sh", ["-c", "ls /lib/ld-musl-*"], { encoding: "utf8" }).status === 0;
    if (musl) arch = `${arch}-musl`;
    else if (arch === "x64" && !readFileSync("/proc/cpuinfo", "utf8").includes("avx2")) arch = "x64-baseline"; // CPUs without AVX2
  }
  return `${os}-${arch}`;
}

/** What stands in for the installed binary; refuses anything it must not touch. */
function installedBinary(): string {
  const exec = process.execPath;
  // From a source checkout the running "binary" is bun itself (or some interpreter): never that.
  if (existsSync(join(import.meta.dir, "main.ts")) || /^bun(\.exe)?$/.test(basename(exec)))
    throw new Error(t("this sasacode runs from source: update the checkout (git pull && bun install) instead of `sasacode update`"));
  return exec;
}

/** `sasacode update [--check] [tag]`: download the release for this machine and take its place. */
export async function updateCommand(args: string[]): Promise<number> {
  const only = args.filter((a) => !a.startsWith("-"));
  const tag = only[0];
  const base = DOWNLOAD_BASE();
  const wanted = tag ?? (await latestRelease(base));
  if (args.includes("--check")) {
    console.log(compareVersions(wanted, VERSION) > 0 ? t("newer version available: {wanted} (this is {version})", { wanted, version: VERSION }) : t("sasacode is up to date ({version})", { version: VERSION }));
    return 0;
  }
  const target = installedBinary(); // before downloading anything: a source checkout stops here
  if (compareVersions(wanted, VERSION) <= 0) {
    console.log(t("sasacode is up to date ({version})", { version: VERSION }));
    return 0;
  }
  const name = `sasacode-${releaseTarget()}`;
  const url = `${base}/download/${wanted}`;
  const dir = mkdtempSync(join(tmpdir(), "sasacode-update-"));
  try {
    console.log(t("downloading {url} …", { url: `${url}/${name}.tar.gz` }));
    const sums = await (await fetch(`${url}/SHA256SUMS`)).text();
    const body = Buffer.from(await (await fetch(`${url}/${name}.tar.gz`)).arrayBuffer());
    const expected = sums.split("\n").find((l) => l.endsWith(` ${name}.tar.gz`))?.split(/\s+/)[0];
    if (!expected) throw new Error(t("{file} is not listed in SHA256SUMS", { file: `${name}.tar.gz` }));
    const actual = createHash("sha256").update(body).digest("hex");
    if (actual !== expected) throw new Error(t("checksum mismatch for {file}", { file: `${name}.tar.gz` }));
    writeFileSync(join(dir, `${name}.tar.gz`), body);
    if (spawnSync("tar", ["-xzf", join(dir, `${name}.tar.gz`), "-C", dir]).status !== 0) throw new Error(t("could not unpack {file}", { file: `${name}.tar.gz` }));
    // The same check the installer does: a binary that does not run here must not become the binary.
    const fresh = join(dir, name);
    const version = spawnSync(fresh, ["--version"], { encoding: "utf8" }).stdout?.trim();
    if (!version) throw new Error(t("the downloaded binary does not run on this system"));
    const staging = `${target}.new`;
    copyFileSync(fresh, staging);
    chmodSync(staging, 0o755);
    renameSync(staging, target); // the running process keeps its own file until it exits
    console.log(t("updated {from} → {to}: {target} (restart sasacode to use it)", { from: VERSION, to: version, target }));
    return 0;
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}
