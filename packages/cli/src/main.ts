#!/usr/bin/env bun
import { join } from "node:path";
import { parseArgs } from "node:util";
import { PERMISSION_MODES } from "@sasacode/agent";
import { BUILTIN_PROVIDERS } from "@sasacode/ai";
import { sasacodeHome } from "./config.ts";
import { dropBlankKeys, ensureEnvFile, loadEnvFile } from "./env.ts";
import { runHeadless } from "./headless.ts";
import { endpointCommand } from "./endpoints.ts";
import { pluginCommand } from "./loader.ts";
import { setup } from "./setup.ts";

const HELP = `sasacode — a small, pluggable coding agent

Usage:
  sasacode [prompt]              interactive session (optional first prompt)
  sasacode -p "<prompt>"         run headless and print the answer

Options:
  -p, --print <prompt>           headless mode (stdin is appended when piped)
      --output <text|jsonl>      headless output format (default text)
  -m, --model <provider/model>   e.g. anthropic/claude-opus-5, openai/gpt-5.5, ollama/gemma4:e4b
                                 (the TUI's /model lists what your providers offer)
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
  sasacode endpoint add <name> <url> [--key-env VAR] [--project]   add a server (format detected)
  sasacode endpoint remove <name> [--project]
  sasacode endpoint list
  -h, --help
  -v, --version`;

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
  dropBlankKeys(BUILTIN_PROVIDERS);
  const envPath = join(sasacodeHome(), ".env");
  const created = ensureEnvFile(envPath, BUILTIN_PROVIDERS);
  loadEnvFile(envPath);
  if (created && process.stderr.isTTY) console.error(`created ${envPath}: add your API key there`);
  if (process.argv[2] === "plugin") return pluginCommand(process.argv.slice(3), process.cwd());
  if (process.argv[2] === "endpoint") return endpointCommand(process.argv.slice(3), process.cwd());
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
