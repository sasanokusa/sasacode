// sasacode update: what the newest release is, how often that is asked, and what must never be replaced.
import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setLang } from "@sasacode/host";
import { checkForUpdate, compareVersions, currentVersion, latestRelease, releaseTarget, updateCommand } from "../src/update.ts";

beforeAll(() => setLang("en")); // these assertions quote the English strings

const home = mkdtempSync(join(tmpdir(), "sasacode-update-"));
process.env.SASACODE_HOME = home;
afterAll(() => rmSync(home, { recursive: true, force: true }));

let asked = 0;
let port = 0;
const server = Bun.serve({
  port: 0,
  fetch(req) {
    asked++;
    const url = new URL(req.url);
    if (url.pathname === "/latest/download/SHA256SUMS")
      // What GitHub does for …/latest/download/…: the redirect pins the version.
      return new Response(null, { status: 302, headers: { location: `http://localhost:${port}/download/v9.9.9/SHA256SUMS` } });
    return new Response("not here", { status: 404 });
  },
});
port = server.port ?? 0;
process.env.SASACODE_DOWNLOAD_BASE = `http://localhost:${server.port}`;
afterAll(() => server.stop(true));

test("versions compare as numbers, not as text", () => {
  expect(compareVersions("v0.9.10", "0.9.9")).toBe(1);
  expect(compareVersions("0.9.4", "v0.9.4")).toBe(0);
  expect(compareVersions("v1.0.0", "0.9.9")).toBe(1);
  expect(compareVersions("0.9.4", "0.9.5")).toBe(-1);
});

test("the newest release is read from the download redirect", async () => {
  expect(await latestRelease()).toBe("v9.9.9");
});

test("a newer version is reported, and asked about once a day", async () => {
  expect(await checkForUpdate()).toBe("v9.9.9");
  const after = asked;
  expect(await checkForUpdate()).toBeUndefined(); // the cache says: already asked today
  expect(asked).toBe(after);
});

test("update in a source checkout points back at the checkout", async () => {
  expect(currentVersion()).toBeTruthy();
  expect(releaseTarget()).toMatch(/^(darwin|linux)-(arm64|x64)/);
  await expect(updateCommand([])).rejects.toThrow("runs from source");
});
