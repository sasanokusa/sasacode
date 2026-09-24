#!/usr/bin/env bun
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { PERMISSION_MODES } from "@sasacode/agent";
import { sasacodeHome } from "./config.ts";
import { runHeadless } from "./headless.ts";
import { setup } from "./setup.ts";

const HELP = `sasacode — a small, pluggable coding agent

Usage:
  sasacode [prompt]              interactive session (optional first prompt)
  sasacode -p "<prompt>"         run headless and print the answer

Options:
  -p, --print <prompt>           headless mode (stdin is appended when piped)
      --output <text|jsonl>      headless output format (default text)
  -m, --model <provider/model>   e.g. anthropic/claude-opus-5, openai/gpt-5, ollama/qwen3.8:27b
      --permission <mode>        ${PERMISSION_MODES.join(" | ")} (default edits)
      --thinking <level>         off | low | medium | high | xhigh | max
      --max-turns <n>            stop after n turns (default unlimited)
  -c, --continue                 resume the most recent session in this directory
  -r, --resume <id>              resume a session by id
      --no-session               do not save this session
  -h, --help
  -v, --version`;

/** Bun already loads ./.env; ~/.sasacode/.env makes keys available from any directory. Existing vars win. */
function loadHomeEnv(): void {
  let text: string;
  try {
    text = readFileSync(join(sasacodeHome(), ".env"), "utf8");
  } catch {
    return;
  }
  for (const line of text.split("\n")) {
    const m = /^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$/.exec(line);
    if (!m || process.env[m[1]!]) continue;
    process.env[m[1]!] = m[2]!.replace(/^(["'])(.*)\1$/, "$2");
  }
}

async function main(): Promise<number> {
  loadHomeEnv();
  const { values, positionals } = parseArgs({
    allowPositionals: true,
    options: {
      print: { type: "string", short: "p" },
      output: { type: "string", default: "text" },
      model: { type: "string", short: "m" },
      permission: { type: "string" },
      thinking: { type: "string" },
      "max-turns": { type: "string" },
      continue: { type: "boolean", short: "c" },
      resume: { type: "string", short: "r" },
      "no-session": { type: "boolean" },
      help: { type: "boolean", short: "h" },
      version: { type: "boolean", short: "v" },
    },
  });
  if (values.help) {
    console.log(HELP);
    return 0;
  }
  if (values.version) {
    console.log((await import("../package.json")).version);
    return 0;
  }
  const harness = await setup({
    cwd: process.cwd(),
    model: values.model,
    permission: values.permission,
    thinking: values.thinking,
    maxTurns: values["max-turns"] ? Number(values["max-turns"]) : undefined,
    resume: values.continue ? "last" : values.resume,
    noSession: values["no-session"],
  });
  for (const w of harness.warnings) console.error(`warning: ${w}`);

  if (values.print !== undefined) {
    let prompt = [values.print, ...positionals].join(" ");
    if (!process.stdin.isTTY) {
      const piped = await new Response(Bun.stdin.stream()).text();
      if (piped.trim()) prompt = `${prompt}\n\n${piped}`;
    }
    if (values.output !== "text" && values.output !== "jsonl") throw new Error("--output must be text or jsonl");
    return runHeadless(harness, prompt, values.output);
  }
  const { runTui } = await import("@sasacode/tui");
  return runTui(harness, positionals.join(" "));
}

main().then(
  (code) => process.exit(code),
  (e) => {
    console.error(`sasacode: ${e instanceof Error ? e.message : e}`);
    process.exit(1);
  },
);
