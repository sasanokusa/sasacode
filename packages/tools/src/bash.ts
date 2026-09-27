import { spawn } from "node:child_process";
import { appendFileSync } from "node:fs";
import { childEnv, saveOutput, text, type ToolDefinition } from "@sasacode/plugin-api";

const DEFAULT_TIMEOUT_S = 120;
const MAX_OUTPUT = 30_000;

interface Args {
  command: string;
  timeout?: number;
}

export const bashTool: ToolDefinition<Args> = {
  name: "bash",
  description: `Run a shell command with bash in the working directory. Returns combined stdout/stderr and the exit code. Output over ${MAX_OUTPUT} characters is cut to the tail and saved to a file whose path is returned. Use ripgrep (rg) for searching.`,
  parameters: {
    type: "object",
    properties: {
      command: { type: "string", description: "Command to run" },
      timeout: { type: "number", description: `Timeout in seconds (default ${DEFAULT_TIMEOUT_S})` },
    },
    required: ["command"],
    additionalProperties: false,
  },
  kind: "exec",
  alwaysLoad: true,
  matchTarget: (a) => a.command,
  summary: (a) => a.command,
  execute(args, ctx) {
    const timeoutS = args.timeout && args.timeout > 0 ? args.timeout : DEFAULT_TIMEOUT_S;
    return new Promise((resolve) => {
      // detached: the command gets its own process group so we can kill everything it spawned.
      const child = spawn("bash", ["-c", args.command], {
        cwd: ctx.cwd,
        detached: true,
        stdio: ["ignore", "pipe", "pipe"],
        // Without the keys sasacode read from ~/.sasacode/.env for itself.
        env: childEnv({ PAGER: "cat", GIT_PAGER: "cat", SASACODE: "1" }),
      });
      // Memory holds the tail only; past MAX_OUTPUT everything goes to a file as it comes.
      let tail = "";
      let size = 0;
      let file: string | undefined;
      const onData = (s: string) => {
        size += s.length;
        if (file) appendFileSync(file, s);
        else if (size > MAX_OUTPUT) file = saveOutput("bash", tail + s);
        tail = (tail + s).slice(-MAX_OUTPUT);
        ctx.onUpdate?.(s);
      };
      // Decoded per stream: a character split across two reads is not broken in two.
      child.stdout!.setEncoding("utf8").on("data", onData);
      child.stderr!.setEncoding("utf8").on("data", onData);

      let killedBy: "timeout" | "abort" | undefined;
      const kill = (why: "timeout" | "abort") => {
        if (killedBy || child.exitCode !== null) return;
        killedBy = why;
        const signal = (s: NodeJS.Signals) => {
          try {
            process.kill(-child.pid!, s);
          } catch {}
        };
        signal("SIGTERM");
        setTimeout(() => signal("SIGKILL"), 2000).unref();
      };
      const timer = setTimeout(() => kill("timeout"), timeoutS * 1000);
      const onAbort = () => kill("abort");
      ctx.signal.addEventListener("abort", onAbort, { once: true });
      if (ctx.signal.aborted) onAbort();

      let settled = false;
      const finish = (code: number | null, sig: NodeJS.Signals | null) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        ctx.signal.removeEventListener("abort", onAbort);
        // Background jobs may keep the pipes open; stop listening once the shell itself is gone.
        child.stdout!.destroy();
        child.stderr!.destroy();
        const output = tail;
        const notes: string[] = [];
        if (file) {
          notes.push(`[Output truncated: showing the last ${MAX_OUTPUT} of ${size} characters. Full output saved to ${file}]`);
        }
        if (killedBy === "timeout") notes.push(`[Command timed out after ${timeoutS}s and was killed]`);
        if (killedBy === "abort") notes.push("[Command was interrupted by the user]");
        notes.push(code !== null ? `[exit code ${code}]` : `[terminated by ${sig}]`);
        resolve({
          content: [text([output.trimEnd(), ...notes].filter(Boolean).join("\n"))],
          isError: code !== 0,
          details: { exitCode: code, killedBy },
        });
      };
      child.on("exit", (code, sig) => setTimeout(() => finish(code, sig), 50));
      child.on("error", (e) => {
        tail += String(e);
        finish(null, null);
      });
    });
  },
};
