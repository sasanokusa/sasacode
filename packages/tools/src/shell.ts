import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { delimiter, dirname, join } from "node:path";

// The bash tool runs bash everywhere. On Windows that is Git for Windows' bash: WSL's bash.exe
// (System32, WindowsApps) runs in another filesystem and would not see the Windows paths the
// model works with, so it is never picked.

let cached: string | undefined;

/** Git for Windows' bash.exe candidates, most specific first. */
export function gitBashCandidates(env: NodeJS.ProcessEnv = process.env): string[] {
  const out: string[] = [];
  if (env.SASACODE_GIT_BASH_PATH) out.push(env.SASACODE_GIT_BASH_PATH);
  // git.exe sits in <Git>\cmd or <Git>\mingw64\bin; bash.exe in <Git>\bin.
  for (const dir of (env.PATH ?? env.Path ?? "").split(delimiter).filter(Boolean)) {
    if (!existsSync(join(dir, "git.exe"))) continue;
    out.push(join(dirname(dir), "bin", "bash.exe"), join(dirname(dirname(dir)), "bin", "bash.exe"));
  }
  for (const root of [env.ProgramFiles, env["ProgramFiles(x86)"], env.ProgramW6432, env.LOCALAPPDATA && join(env.LOCALAPPDATA, "Programs")])
    if (root) out.push(join(root, "Git", "bin", "bash.exe"));
  return out;
}

/** The bash to run commands with: `bash` on macOS and Linux, Git Bash's full path on Windows. */
export function bashPath(): string {
  if (process.platform !== "win32") return "bash";
  if (cached) return cached;
  const found = gitBashCandidates().find((p) => existsSync(p));
  if (!found)
    throw new Error(
      "bash was not found: sasacode on Windows needs Git for Windows (https://git-scm.com/download/win). " +
        "If it is installed somewhere unusual, set SASACODE_GIT_BASH_PATH to its bash.exe.",
    );
  return (cached = found);
}

/**
 * Stop a process and everything it started. On macOS and Linux the process must have been spawned
 * detached (its own process group) and gets `signal`; Windows has no signals or process groups, so
 * the whole tree is ended at once with taskkill.
 */
export function killTree(pid: number, signal: NodeJS.Signals = "SIGTERM"): boolean {
  if (process.platform === "win32") {
    const r = spawnSync("taskkill", ["/pid", String(pid), "/T", "/F"], { stdio: "ignore", windowsHide: true });
    return r.status === 0;
  }
  try {
    process.kill(-pid, signal);
    return true;
  } catch {
    return false;
  }
}
