// `sasacode web-search install` / `/web-search install`: RustXNG's own release binary, checked
// against the .sha256 published next to it, put where web_search looks after PATH.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, existsSync, mkdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { t } from "@sasacode/host";
import { sasacodeHome } from "@sasacode/plugin-api";

export const RUSTXNG_RELEASES = "https://github.com/sahenjp/RustXNG/releases";
const releases = () => process.env.SASACODE_RUSTXNG_BASE ?? RUSTXNG_RELEASES;

/** sasacode's own rustxng: kept off PATH, used when PATH has none. */
export function managedRustxng(): string {
  return join(sasacodeHome(), "bin", process.platform === "win32" ? "rustxng.exe" : "rustxng");
}

/** The release's target for this machine: RustXNG ships macOS, x86_64 Linux (musl: any distribution) and x86_64 Windows. */
export function rustxngTarget(platform: string = process.platform, arch: string = process.arch): string {
  if (platform === "darwin") return arch === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin";
  if (platform === "linux" && arch === "x64") return "x86_64-unknown-linux-musl";
  if (platform === "win32") return "x86_64-pc-windows-msvc"; // Windows on arm64 runs it emulated
  throw new Error(
    t("RustXNG の配布物に {platform}-{arch} 向けのものがありません。cargo install --git https://github.com/sahenjp/RustXNG.git で入れられます。", { platform, arch }),
  );
}

/** The newest release tag, read from the redirect of …/releases/latest. */
async function latestTag(base: string): Promise<string> {
  const r = await fetch(`${base}/latest`, { method: "HEAD", redirect: "manual" });
  const m = /\/tag\/([^/?#]+)$/.exec(r.headers.get("location") ?? "");
  if (!m) throw new Error(t("RustXNG の最新版がわかりません（{base} が {status} を返しました）", { base, status: r.status }));
  return decodeURIComponent(m[1]!);
}

async function get(url: string): Promise<Response> {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${r.status} ${url}`);
  return r;
}

/** Download, verify and install rustxng (the newest release, or `tag`); an existing one is replaced. */
export async function installRustxng(opts: { tag?: string; log?: (message: string) => void } = {}): Promise<{ path: string; version: string }> {
  const base = releases();
  const target = rustxngTarget();
  const tag = opts.tag ?? (await latestTag(base));
  const asset = `rustxng-${tag}-${target}${process.platform === "win32" ? ".exe" : ""}`;
  const url = `${base}/download/${tag}/${asset}`;
  opts.log?.(t("{url} をダウンロード中…（RustXNG、AGPL-3.0）", { url }));
  const expected = (await (await get(`${url}.sha256`)).text()).trim().split(/\s+/)[0];
  const body = Buffer.from(await (await get(url)).arrayBuffer());
  if (createHash("sha256").update(body).digest("hex") !== expected) throw new Error(t("{file} のチェックサムが一致しません", { file: asset }));
  const path = managedRustxng();
  mkdirSync(dirname(path), { recursive: true });
  const staging = `${path}.new`;
  writeFileSync(staging, body);
  chmodSync(staging, 0o755);
  // A binary that does not run here must not take the place of one that does.
  const version = spawnSync(staging, ["--version"], { encoding: "utf8", windowsHide: true, timeout: 20_000 }).stdout?.trim();
  if (!version) {
    rmSync(staging, { force: true });
    throw new Error(t("ダウンロードした rustxng がこの環境で動きません"));
  }
  if (process.platform === "win32" && existsSync(path)) {
    // A running sidecar's .exe cannot be replaced, only renamed: move it aside (the next install removes it).
    rmSync(`${path}.old`, { force: true });
    renameSync(path, `${path}.old`);
  }
  renameSync(staging, path);
  return { path, version };
}
