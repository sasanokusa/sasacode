// Runs the real sasacode CLI as a child process against a fake model server, in a throwaway home
// and project: config loading, the adapters over HTTP, tools on real files, sessions on disk.
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const MAIN = resolve(import.meta.dir, "../../cli/src/main.ts");
// An empty bunfig, not /dev/null: Bun cannot open /dev/null (or NUL) on Windows.
const NO_BUNFIG = resolve(import.meta.dir, "../../../scripts/no-bunfig.toml");

export interface Sandbox {
  home: string;
  project: string;
  /** `sasacode <args>`; resolves when it exits. */
  run(args: string[], opts?: { stdin?: string; env?: Record<string, string> }): Promise<{ code: number; stdout: string; stderr: string }>;
  /** `sasacode -p <prompt> --output jsonl`: the events, parsed. */
  jsonl(prompt: string, extra?: string[]): Promise<{ code: number; events: any[]; stderr: string }>;
  cleanup(): void;
}

export function sandbox(config: Record<string, unknown>): Sandbox {
  const root = mkdtempSync(join(tmpdir(), "sasacode-e2e-"));
  const home = join(root, "home");
  const project = join(root, "project");
  mkdirSync(home, { recursive: true });
  mkdirSync(project, { recursive: true });
  writeFileSync(join(home, "config.json"), JSON.stringify(config, null, 2));
  writeFileSync(join(home, ".env"), ""); // no keys from the developer's machine

  const run: Sandbox["run"] = async (args, opts = {}) => {
    const env: Record<string, string> = { PATH: process.env.PATH ?? "", HOME: root, USERPROFILE: root, SASACODE_HOME: home, SASACODE_LANG: "en", NO_COLOR: "1", ...opts.env };
    const child = Bun.spawn([process.execPath, "--no-env-file", `--config=${NO_BUNFIG}`, MAIN, ...args], {
      cwd: project,
      env,
      stdin: opts.stdin !== undefined ? new TextEncoder().encode(opts.stdin) : "ignore",
      stdout: "pipe",
      stderr: "pipe",
    });
    const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
    return { code, stdout, stderr };
  };

  return {
    home,
    project,
    run,
    async jsonl(prompt, extra = []) {
      const r = await run(["-p", prompt, "--output", "jsonl", ...extra]);
      const events = r.stdout.split("\n").filter((l) => l.trim()).map((l) => JSON.parse(l));
      return { code: r.code, events, stderr: r.stderr };
    },
    cleanup: () => rmSync(root, { recursive: true, force: true }),
  };
}

/** Config for one OpenAI-compatible server named "fake". */
export const fakeConfig = (url: string, extra: Record<string, unknown> = {}) => ({
  providers: { fake: { api: "openai-chat", baseUrl: url } },
  model: "fake/m",
  maxRetries: 0,
  ...extra,
});

/** The text of the final answer in a jsonl run. */
export const answer = (events: any[]) =>
  events
    .filter((e) => e.type === "message_end" && e.message?.role === "assistant")
    .map((e) => e.message.content.filter((c: any) => c.type === "text").map((c: any) => c.text).join(""))
    .filter(Boolean)
    .at(-1) ?? "";

/** The messages the model was sent in one request, as role/content summaries. */
export const roles = (request: any): string[] => request.messages.map((m: any) => m.role);
