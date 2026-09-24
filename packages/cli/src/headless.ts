import type { AgentEvent, StopCause } from "@sasacode/agent";
import { textOf } from "@sasacode/ai";
import type { Harness } from "./setup.ts";

/** `sasacode -p`: run one prompt to completion. Text mode streams the answer to stdout; jsonl emits every event. */
export async function runHeadless(h: Harness, prompt: string, output: "text" | "jsonl"): Promise<number> {
  const { agent } = h;
  const write = (s: string) => process.stdout.write(s);
  const status = (s: string) => process.stderr.write(`\x1b[2m${s}\x1b[0m\n`);
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
    } else if (e.type === "tool_start") status(`● ${e.call.name}(${e.summary})`);
    else if (e.type === "tool_end" && e.result.isError) status(`  ⎿ ${textOf(e.result.content).split("\n").slice(-1)[0]}`);
    else if (e.type === "error") process.stderr.write(`error: ${e.error}\n`);
    else if (e.type === "context_limit") process.stderr.write("stopped: context window is full\n");
    else if (e.type === "plugin_error") process.stderr.write(`plugin ${e.plugin} (${e.hook}): ${e.error}\n`);
  });
  await h.loadPlugins();
  const onSigint = () => agent.abort();
  process.on("SIGINT", onSigint);
  const cause: StopCause = await agent.prompt(prompt);
  // Plugins may inject follow-ups (agent_end); wait for everything to settle.
  await agent.waitForIdle();
  process.off("SIGINT", onSigint);
  await h.shutdown();
  if (output === "text" && cause !== "done") status(`[stopped: ${cause}]`);
  return cause === "done" ? 0 : 1;
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
