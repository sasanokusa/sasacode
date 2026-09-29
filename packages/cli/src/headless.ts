import { existsSync } from "node:fs";
import { type AgentEvent, type StopCause } from "@sasacode/agent";
import { t } from "@sasacode/host";
import { textOf } from "@sasacode/ai";
import type { Harness } from "./setup.ts";

/** `sasacode -p`: run one prompt to completion. Text mode streams the answer to stdout; jsonl emits every event. */
export async function runHeadless(h: Harness, prompt: string, output: "text" | "jsonl"): Promise<number> {
  const { agent } = h;
  // Model and tool text reaching a terminal must not carry escape sequences it would act on.
  const tty = process.stdout.isTTY;
  const write = (s: string) => process.stdout.write(tty ? plain(s) : s);
  // Dim only where it will be shown as dim: a log file or CI output must keep no escape codes (NO_COLOR too).
  const dim = process.stderr.isTTY && !process.env.NO_COLOR ? (s: string) => `\x1b[2m${s}\x1b[0m` : (s: string) => s;
  const status = (s: string) => process.stderr.write(`${dim(plain(s))}\n`);
  let atLineStart = true;

  agent.events.on((e: AgentEvent) => {
    if (output === "jsonl") {
      write(`${JSON.stringify(toJson(e))}\n`);
      return;
    }
    if (e.type === "message_update" && e.event.type === "text_delta") {
      write(e.event.delta);
      atLineStart = e.event.delta.endsWith("\n");
    } else if (e.type === "message_end" && e.message.role === "assistant" && textOf(e.message.content) && !atLineStart) {
      write("\n");
      atLineStart = true;
    } else if (e.type === "message_discarded" && e.message.stopReason === "error") {
      if (!atLineStart) write("\n");
      atLineStart = true;
      status(t("the connection dropped mid-reply; asking again"));
    } else if (e.type === "tool_start") status(`● ${e.call.name}(${e.summary})`);
    else if (e.type === "tool_end" && e.result.isError) status(`  ⎿ ${textOf(e.result.content).split("\n").slice(-1)[0]}`);
    else if (e.type === "error") process.stderr.write(`${t("error: {error}", { error: plain(e.error) })}\n`);
    else if (e.type === "tool_repaired") status(`  ${t("↻ repaired {name}: {note}", { name: e.call.name, note: e.note })}`);
    else if (e.type === "plugin_error") process.stderr.write(`${t("plugin {plugin} ({hook}): {error}", { plugin: e.plugin, hook: e.hook, error: e.error })}\n`);
  });
  const session = agent.session;
  if (output === "jsonl") write(`${JSON.stringify({ type: "session", id: session?.id ?? null, path: session?.path ?? null })}\n`);
  await h.loadPlugins();
  // Unlike the TUI, a one-shot run should start with MCP servers and similar tools connected.
  await h.host.settle();
  const onSigint = () => agent.abort();
  process.on("SIGINT", onSigint);
  let stoppedBy = "";
  agent.events.on((e) => {
    if (e.type === "agent_end" && e.stopped) stoppedBy = `${e.stopped.plugin}: ${e.stopped.reason}`;
  });
  const cause: StopCause = await agent.prompt(prompt);
  // Plugins may inject follow-ups (agent_end); wait for everything to settle.
  await agent.waitForIdle();
  process.off("SIGINT", onSigint);
  await h.shutdown();
  const hint = stoppedBy ? ` — ${stoppedBy}` : STOP_HINT[cause] ? ` — ${t(STOP_HINT[cause]!)}` : "";
  if (output === "text" && cause !== "done") status(t("[stopped: {cause}{hint}]", { cause, hint }));
  // One-shot runs are how sasacode is driven without tmux: say how to continue this conversation.
  if (output === "text" && session && existsSync(session.path)) status(t('session {id} · continue: sasacode -r {id} -p "…"', { id: session.id }));
  return cause === "done" ? 0 : 1;
}

const STOP_HINT: Partial<Record<StopCause, string>> = {
  max_turns: "turn limit reached (--max-turns or maxTurns in config; 0 = no limit)",
  context_limit: "context window is full",
};

/** ESC shows as ␛; other control characters but newline and tab are dropped. */
function plain(s: string): string {
  return s.replace(/\x1b/g, "␛").replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, "");
}

/** message_update carries the whole partial message; the delta is enough for a log. */
function toJson(e: AgentEvent): unknown {
  if (e.type === "message_update") {
    const { partial: _p, ...event } = e.event as typeof e.event & { partial?: unknown };
    return { type: e.type, event };
  }
  if (e.type === "message_start") return { type: e.type };
  return e;
}
