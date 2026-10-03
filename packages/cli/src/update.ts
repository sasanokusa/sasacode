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
    const how = process.env.SASACODE_INSTALLED_BY === "npm" ? "npm install -g sasacode@latest" : "sasacode update";
    if (latest) notify(t("a newer version is available: {latest} (this is {version}) — run `{how}`", { latest, version: VERSION, how }));
  } catch {}
}

/** Whether this Windows CPU has AVX2 (kernel32's PF_AVX2_INSTRUCTIONS_AVAILABLE); true when that cannot be told. */
export function windowsHasAvx2(): boolean {
  const kernel32 = `[DllImport("kernel32.dll")] public static extern bool IsProcessorFeaturePresent(int f);`;
  const r = spawnSync(
    "powershell.exe",
    ["-NoProfile", "-NonInteractive", "-Command", `(Add-Type -MemberDefinition '${kernel32}' -Name K -Namespace SasacodeCpu -PassThru)::IsProcessorFeaturePresent(40)`],
    // Bounded: PowerShell can hang where its environment is incomplete; unknown counts as AVX2.
    { encoding: "utf8", windowsHide: true, stdio: ["ignore", "pipe", "ignore"], timeout: 20_000 },
  );
  return r.stdout?.trim() !== "False";
}

/** The release files of a target: a .zip holding an .exe for Windows, a .tar.gz elsewhere. */
export function releaseFiles(target: string): { archive: string; binary: string } {
  const name = `sasacode-${target}`;
  return target.startsWith("windows-") ? { archive: `${name}.zip`, binary: `${name}.exe` } : { archive: `${name}.tar.gz`, binary: name };
}

/** Unpack a release archive into `dir`. Windows' own tar (bsdtar) reads .zip; Git's GNU tar would not. */
export function unpack(archive: string, dir: string): boolean {
  const tar = process.platform === "win32" ? join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe") : "tar";
  return spawnSync(tar, [archive.endsWith(".zip") ? "-xf" : "-xzf", archive], { cwd: dir, windowsHide: true }).status === 0;
}

/** The archive name for this machine, with the platform choices scripts/install.sh (install.ps1) makes. */
export function releaseTarget(): string {
  const os = { darwin: "darwin", linux: "linux", win32: "windows" }[process.platform as string];
  if (!os) throw new Error(t("unsupported OS: {platform}", { platform: process.platform }));
  let arch = { x64: "x64", arm64: "arm64" }[process.arch as string];
  if (!arch) throw new Error(t("unsupported CPU: {arch}", { arch: process.arch }));
  if (os === "windows") return arch === "x64" && !windowsHasAvx2() ? "windows-x64-baseline" : `windows-${arch}`;
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
  // The npm package's launcher keeps its own binary per version: npm does the updating there.
  if (process.env.SASACODE_INSTALLED_BY === "npm") throw new Error(t("this sasacode was installed with npm: update it with `npm install -g sasacode@latest`"));
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
  const { archive, binary } = releaseFiles(releaseTarget());
  const url = `${base}/download/${wanted}`;
  const dir = mkdtempSync(join(tmpdir(), "sasacode-update-"));
  try {
    console.log(t("downloading {url} …", { url: `${url}/${archive}` }));
    const sums = await (await fetch(`${url}/SHA256SUMS`)).text();
    const body = Buffer.from(await (await fetch(`${url}/${archive}`)).arrayBuffer());
    const expected = sums.split(/\r?\n/).find((l) => l.endsWith(` ${archive}`))?.split(/\s+/)[0];
    if (!expected) throw new Error(t("{file} is not listed in SHA256SUMS", { file: archive }));
    const actual = createHash("sha256").update(body).digest("hex");
    if (actual !== expected) throw new Error(t("checksum mismatch for {file}", { file: archive }));
    writeFileSync(join(dir, archive), body);
    if (!unpack(archive, dir)) throw new Error(t("could not unpack {file}", { file: archive }));
    // The same check the installer does: a binary that does not run here must not become the binary.
    const fresh = join(dir, binary);
    const version = spawnSync(fresh, ["--version"], { encoding: "utf8", windowsHide: true }).stdout?.trim();
    if (!version) throw new Error(t("the downloaded binary does not run on this system"));
    const staging = `${target}.new`;
    copyFileSync(fresh, staging);
    chmodSync(staging, 0o755);
    if (process.platform === "win32") {
      // Windows will not replace a running .exe, but lets it be renamed: move it aside first
      // (the next update removes it).
      const old = `${target}.old`;
      rmSync(old, { force: true });
      renameSync(target, old);
    }
    renameSync(staging, target); // the running process keeps its own file until it exits
    console.log(t("updated {from} → {to}: {target} (restart sasacode to use it)", { from: VERSION, to: version, target }));
    return 0;
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}
