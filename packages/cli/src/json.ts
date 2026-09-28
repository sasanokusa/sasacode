// JSON files under ~/.sasacode and in projects. Two ways to read, chosen by what a bad file costs.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { t } from "@sasacode/host";

/**
 * A file the user edits (config.json): missing is `{}`, but one that cannot be parsed is an error,
 * so nothing is used half-read and nothing overwrites it.
 */
export function readJson(path: string): any {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return {};
    throw new Error(t("failed to read {path}: {error}", { path, error: (e as Error).message }));
  }
}

/** A file sasacode can do without (a cache, a manifest it only probes for): any failure is `fallback`. */
export function readJsonOr<T>(path: string, fallback: T): T {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch {
    return fallback;
  }
}

/** Written readable by the user only: these files hold settings, trust decisions and caches. */
export function writeJson(path: string, value: unknown): void {
  mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
}
