import { afterAll, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Agent, PermissionPolicy, PluginHost } from "@sasacode/agent";
import { setLang } from "@sasacode/host";
import { bundledPlugins, installRustxng, managedRustxng, rustxngTarget } from "../src/index.ts";

// The messages checked below are the Japanese ones, whatever the machine's locale.
setLang("ja");
const home = mkdtempSync(join(tmpdir(), "sasacode-rustxng-"));
const env = { SASACODE_HOME: process.env.SASACODE_HOME, SASACODE_RUSTXNG_BASE: process.env.SASACODE_RUSTXNG_BASE, PATH: process.env.PATH };
afterAll(() => {
  for (const [k, v] of Object.entries(env)) v === undefined ? delete process.env[k] : (process.env[k] = v);
  rmSync(home, { recursive: true, force: true });
});

/** A release page like GitHub's: /latest redirects to the tag, assets sit under /download/<tag>/. */
function releases(binary: string, sum = createHash("sha256").update(binary).digest("hex")) {
  const asset = `rustxng-v9.9.9-${rustxngTarget()}`;
  return Bun.serve({
    port: 0,
    fetch(req) {
      const path = new URL(req.url).pathname;
      if (path === "/latest") return new Response(null, { status: 302, headers: { location: "https://github.com/sahenjp/RustXNG/releases/tag/v9.9.9" } });
      if (path === `/download/v9.9.9/${asset}`) return new Response(binary);
      if (path === `/download/v9.9.9/${asset}.sha256`) return new Response(`${sum}  ${asset}\n`);
      return new Response("missing", { status: 404 });
    },
  });
}

test("the release target for each platform RustXNG ships", () => {
  expect(rustxngTarget("darwin", "arm64")).toBe("aarch64-apple-darwin");
  expect(rustxngTarget("darwin", "x64")).toBe("x86_64-apple-darwin");
  expect(rustxngTarget("linux", "x64")).toBe("x86_64-unknown-linux-musl");
  expect(rustxngTarget("win32", "x64")).toBe("x86_64-pc-windows-msvc");
  expect(rustxngTarget("linux", "arm64")).toBe("aarch64-unknown-linux-gnu");
  expect(() => rustxngTarget("freebsd", "x64")).toThrow("cargo install");
});

// The stand-in binary is a shell script: there is no such thing on Windows.
test.skipIf(process.platform === "win32")("install: the newest release, checksum-checked, into ~/.sasacode/bin; then web_search is offered without a restart", async () => {
  process.env.SASACODE_HOME = home;
  process.env.PATH = ""; // no rustxng on PATH
  const agent = new Agent({ model: { id: "m", provider: "t", api: "none", contextWindow: 1e5, maxOutput: 1e3 }, cwd: home, systemPrompt: "", permissions: new PermissionPolicy("edits") });
  const host = new PluginHost({ agent, cwd: home, settings: () => ({}) });
  const notes: string[] = [];
  host.setUI({ interactive: false, notify: (m) => notes.push(m), confirm: async () => false, select: async () => undefined });
  await host.load("web-search", bundledPlugins["web-search"]!);
  expect(agent.getTools().map((t) => t.name)).not.toContain("web_search");

  const bad = releases("#!/bin/sh\necho 'RustXNG 9.9.9'\n", "0".repeat(64));
  process.env.SASACODE_RUSTXNG_BASE = bad.url.href.replace(/\/$/, "");
  await expect(installRustxng()).rejects.toThrow("チェックサム");
  expect(existsSync(managedRustxng())).toBe(false);
  bad.stop(true);

  const good = releases("#!/bin/sh\necho 'RustXNG 9.9.9'\n");
  process.env.SASACODE_RUSTXNG_BASE = good.url.href.replace(/\/$/, "");
  try {
    await host.commands.find((c) => c.name === "web-search")!.run({ args: "install" });
    expect(notes.join("\n")).toContain(`RustXNG 9.9.9 を ${managedRustxng()} に入れました。web_search が使えます。`);
    expect(readFileSync(managedRustxng(), "utf8")).toContain("RustXNG 9.9.9");
    expect(agent.getTools().map((t) => t.name)).toContain("web_search");
  } finally {
    good.stop(true);
  }
});
