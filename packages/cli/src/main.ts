#!/usr/bin/env bun
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { PERMISSION_MODES } from "@sasacode/agent";
import { sasacodeHome } from "./config.ts";
import { runHeadless } from "./headless.ts";
import { pluginCommand } from "./loader.ts";
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
      --trust-project            load project plugins / MCP servers without asking (headless)

Subcommands:
  sasacode plugin install <npm-spec|git-url> [--project]
  sasacode plugin remove <name> [--project]
  sasacode plugin list
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

/** Piped input is appended to -p. A pipe that stays open without data (e.g. a parent shell) is ignored. */
async function readPipedStdin(): Promise<string> {
  if (process.stdin.isTTY) return "";
  const reader = Bun.stdin.stream().getReader();
  const first = await Promise.race([reader.read(), Bun.sleep(300).then(() => undefined)]);
  if (!first || first.done) {
    reader.releaseLock();
    return "";
  }
  const chunks = [first.value];
  for (let r = await reader.read(); !r.done; r = await reader.read()) chunks.push(r.value);
  return Buffer.concat(chunks).toString("utf8");
}

async function main(): Promise<number> {
  loadHomeEnv();
  if (process.argv[2] === "plugin") return pluginCommand(process.argv.slice(3), process.cwd());
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
      "trust-project": { type: "boolean" },
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
    trustProject: values["trust-project"],
  });
  for (const w of harness.warnings) console.error(`warning: ${w}`);

  if (values.print !== undefined) {
    let prompt = [values.print, ...positionals].join(" ");
    const piped = await readPipedStdin();
    if (piped.trim()) prompt = `${prompt}\n\n${piped}`;
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
