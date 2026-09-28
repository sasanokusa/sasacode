import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { childEnv, text, type Plugin, type ToolContext } from "@sasacode/plugin-api";

const exec = promisify(execFile);
type Scope = "working" | "staged" | "branch";
interface Args { scope?: Scope; base?: string; patch?: boolean; maxChars?: number }

/** Git receives argv, never shell text. External diff/textconv helpers are disabled. */
async function git(cwd: string, args: string[], signal: AbortSignal): Promise<string> {
  const { stdout } = await exec("git", ["--no-pager", "-c", "core.fsmonitor=false", ...args], {
    cwd, signal, timeout: 15_000, maxBuffer: 8 * 1024 * 1024,
    env: childEnv({ GIT_OPTIONAL_LOCKS: "0", GIT_TERMINAL_PROMPT: "0", GIT_NO_LAZY_FETCH: "1", LC_ALL: "C" }),
    encoding: "utf8",
  });
  return stdout.trimEnd();
}

export async function collectContext(args: Args, ctx: ToolContext) {
  ctx.signal.throwIfAborted();
  const scope = args.scope ?? "working";
  if (!["working", "staged", "branch"].includes(scope)) throw new Error("Unknown scope");
  if (args.base !== undefined && scope !== "branch") throw new Error("base requires scope=branch");
  const limit = args.maxChars ?? 16_000;
  if (!Number.isInteger(limit) || limit < 1000 || limit > 60_000) throw new Error("maxChars must be 1000..60000");
  const revision = async (ref: string) => git(ctx.cwd, ["rev-parse", "--verify", "--end-of-options", `${ref}^{commit}`], ctx.signal);
  const head = await git(ctx.cwd, ["rev-parse", "--verify", "HEAD"], ctx.signal).catch((e) => {
    if (ctx.signal.aborted) throw e;
    return undefined;
  });
  // Also verifies that this is a working tree, including repositories before their first commit.
  const status = await git(ctx.cwd, ["status", "--short", "--branch", "--untracked-files=normal", "--", "."], ctx.signal);
  let range: string[] = [];
  let comparison: string;
  if (scope === "branch") {
    const base = args.base ?? "@{upstream}";
    const baseId = await revision(base); // Resolve first so refs beginning with '-' cannot become options.
    if (!head) throw new Error("Branch comparison requires a commit");
    const mergeBase = await git(ctx.cwd, ["merge-base", head, baseId], ctx.signal);
    range = [mergeBase, head];
    comparison = `merge-base(${base}, HEAD) ${mergeBase} -> HEAD ${head}; excludes uncommitted changes`;
  } else if (scope === "staged") {
    range = ["--cached"];
    comparison = "HEAD -> index (staged changes)";
  } else {
    // HEAD vs worktree includes staged + unstaged changes; without HEAD, all tracked files are staged.
    range = head ? [head] : ["--cached"];
    comparison = "HEAD -> working tree (combined staged + unstaged changes)";
  }
  const common = ["diff", "--no-ext-diff", "--no-textconv", "--no-color", "--ignore-submodules=all"];
  const diff = (options: string[]) => git(ctx.cwd, [...common, ...options, ...range, "--", "."], ctx.signal);
  const sections = [
    `Git context (${scope})\nComparison: ${comparison}\nScope: current directory and descendants.\nUntracked contents and submodule changes are not included. This is a live view, not an atomic snapshot.`,
    `Status\n${status || "(clean)"}`,
    `Diff statistics\n${await diff(["--stat"]) || "(no changes)"}`,
  ];
  if (args.patch !== false) sections.push(`Patch\n${await diff(["--unified=3"]) || "(no changes)"}`);
  const full = sections.join("\n\n");
  const truncated = full.length > limit;
  const note = "\n[Truncated. Request patch=false for an overview, raise maxChars, or inspect individual files.]";
  return {
    content: [text(truncated ? full.slice(0, limit - note.length) + note : full)],
    details: { scope, head: head ?? null, truncated },
  };
}

const plugin: Plugin = (api) => {
  api.registerTool({
    name: "git_context",
    description: "Read a bounded Git review context: status, diff statistics and optional patch. working combines staged and unstaged changes; staged shows the index; branch compares merge-base(base, HEAD) to HEAD, defaulting to upstream. Never fetches or changes Git state. Untracked contents and submodules are excluded.",
    kind: "read", concurrent: true,
    paths: (_args, cwd) => [cwd],
    parameters: {
      type: "object", properties: {
        scope: { type: "string", enum: ["working", "staged", "branch"] },
        base: { type: "string", minLength: 1, maxLength: 256 },
        patch: { type: "boolean" },
        maxChars: { type: "integer", minimum: 1000, maximum: 60_000 },
      }, additionalProperties: false,
    },
    execute: collectContext,
  });
};
export default plugin;
