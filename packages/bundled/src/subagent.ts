import type { Plugin } from "@sasacode/plugin-api";

// A subagent may not steer other subagents, nor touch the caller's TODO list or goal.
export const SUBAGENT_EXCLUDED = ["task", "task_status", "task_send", "task_stop", "todo_write", "set_goal"];

interface Run {
  id: string;
  description: string;
  background: boolean;
  started: number;
  ended?: number;
  /** Tool calls so far, newest last. */
  trail: string[];
  status: "running" | "done" | "failed" | "stopped";
  /** Stopped because the caller was interrupted (Esc): the report waits for the next message. */
  interrupted?: boolean;
  report?: string;
  send?: (text: string) => void;
  stop?: () => void;
}

const TRAIL = 12;
const ago = (ms: number) => (ms < 60_000 ? `${Math.round(ms / 1000)}s` : `${Math.floor(ms / 60_000)}m${Math.round((ms % 60_000) / 1000)}s`);

/** What the caller gets back: the report, and when it did not finish, why and how far it got. */
export function reportOf(r: { text: string; cause: string; error?: string }, trail: string[]): string {
  if (r.cause === "done") return r.text || "(the subagent finished without a report)";
  const why = r.cause === "error" ? `error: ${r.error ?? "unknown"}` : r.cause;
  return [
    `[subagent did not finish: ${why}]`,
    r.text ? `Its last message:\n${r.text}` : "It wrote no message.",
    trail.length ? `Its last tool calls:\n${trail.slice(-TRAIL).map((t) => `- ${t}`).join("\n")}` : "It made no tool calls.",
  ].join("\n\n");
}

/**
 * `task`: hand a self-contained job to a fresh agent loop. In the foreground the caller waits for the
 * report; with `background` it keeps working and the report arrives as a message when the run ends.
 * task_status / task_send / task_stop let the caller watch, steer and stop runs; `/task` does the
 * same for the user. Interrupting the caller (Esc) stops its background runs too.
 */
const subagent: Plugin = (api) => {
  const runs = new Map<string, Run>();
  let seq = 0;

  const line = (r: Run) =>
    `${r.id} [${r.status}${r.background ? ", background" : ""}] ${r.description} — ${ago((r.ended ?? Date.now()) - r.started)}, ${r.trail.length} tool calls${r.trail.length ? `, last: ${r.trail.at(-1)}` : ""}`;

  function start(args: { description: string; prompt: string; model?: string }, background: boolean, signal?: AbortSignal, onUpdate?: (t: string) => void) {
    const run: Run = { id: `t${++seq}`, description: args.description, background, started: Date.now(), trail: [], status: "running" };
    runs.set(run.id, run);
    const done = api.agent
      .run({
        prompt: `${args.prompt}\n\nWhen done, reply with a concise report of what you found or did; it is all the caller will see. The caller may send you messages while you work; follow them.`,
        model: args.model,
        excludeTools: SUBAGENT_EXCLUDED,
        signal,
        onStart: (c) => Object.assign(run, c),
        onProgress: (t) => {
          run.trail.push(t);
          onUpdate?.(`${t}\n`);
        },
      })
      .catch((e) => ({ text: "", cause: "error" as const, error: e instanceof Error ? e.message : String(e) }))
      .then((r) => {
        run.ended = Date.now();
        run.status = r.cause === "done" ? "done" : r.cause === "aborted" ? "stopped" : "failed";
        run.report = reportOf(r, run.trail);
        run.send = run.stop = undefined;
        return run;
      });
    return { run, done };
  }

  // The subagent's own tool calls go through permissions, so starting or watching one needs no approval.
  api.permissions.addRules({ allow: ["task", "task_status", "task_send", "task_stop"] });
  api.registerTool({
    name: "task",
    description:
      "Run a subagent with a fresh context on a self-contained task (research, a broad search, an independent change). It has the same tools except task, todo_write and set_goal. Several task calls in one turn run in parallel. By default you wait for its report; with background: true you get its id at once, keep working, and its report arrives as a message when it ends (watch it with task_status, steer it with task_send, stop it with task_stop).",
    parameters: {
      type: "object",
      properties: {
        description: { type: "string", description: "Short label (3-6 words)" },
        prompt: { type: "string", description: "Complete instructions; the subagent sees nothing else" },
        model: { type: "string", description: 'Optional "<provider>/<model>"' },
        background: { type: "boolean", description: "Return at once and deliver the report later" },
      },
      required: ["description", "prompt"],
      additionalProperties: false,
    },
    kind: "other",
    concurrent: true,
    alwaysLoad: true,
    summary: (a) => `${a.description}${a.background ? " (background)" : ""}`,
    async execute(args, ctx) {
      if (!args.background) {
        const run = await start(args, false, ctx.signal, ctx.onUpdate).done;
        return { content: [{ type: "text", text: run.report! }], isError: run.status === "failed" };
      }
      // Not tied to this turn's signal, which ends with the turn: agent_end below stops it on Esc.
      const { run, done } = start(args, true);
      void done.then((r) => {
        api.ui.notify(`task ${r.id}「${r.description}」が終了しました（${r.status}）`, r.status === "done" ? "info" : "warning");
        // After an interruption the user decides what comes next: the report goes with their next message.
        api.session.inject(`[task ${r.id} "${r.description}" ended: ${r.status}]\n\n${r.report}`, r.interrupted ? "next" : "now");
      });
      return { content: [{ type: "text", text: `Started ${run.id} in the background. Its report will arrive as a message; use task_status / task_send / task_stop with id "${run.id}".` }] };
    },
  });

  // Esc on the caller stops what it started in the background (a subagent's own end does not).
  api.on("agent_end", (e, where) => {
    if (where.agent || e.cause !== "aborted") return;
    for (const r of runs.values()) if (r.background && r.stop) (r.interrupted = true), r.stop();
  });

  const find = (id: string) => {
    const r = runs.get(id);
    if (!r) throw new Error(`no task ${id} (known: ${[...runs.keys()].join(", ") || "none"})`);
    return r;
  };
  const idParam = { id: { type: "string", description: 'Task id, e.g. "t1"' } };

  api.registerTool({
    name: "task_status",
    description: "List subagent runs, or show one run's progress (its recent tool calls, and its report once it has ended).",
    parameters: { type: "object", properties: { id: { type: "string", description: "Omit to list all runs" } }, additionalProperties: false },
    kind: "read",
    concurrent: true,
    summary: (a) => a.id ?? "all",
    async execute(args) {
      if (!args.id) return { content: [{ type: "text", text: [...runs.values()].map(line).join("\n") || "no subagent runs" }] };
      const r = find(args.id);
      const text = [line(r), r.trail.length ? `Recent tool calls:\n${r.trail.slice(-TRAIL).map((t) => `- ${t}`).join("\n")}` : "", r.report ? `Report:\n${r.report}` : ""];
      return { content: [{ type: "text", text: text.filter(Boolean).join("\n\n") }] };
    },
  });

  api.registerTool({
    name: "task_send",
    description: "Send a message to a running background subagent (a correction, extra information, or \"wrap up now\"). It sees it at its next turn.",
    parameters: { type: "object", properties: { ...idParam, message: { type: "string" } }, required: ["id", "message"], additionalProperties: false },
    kind: "other",
    summary: (a) => a.id,
    async execute(args) {
      const r = find(args.id);
      if (!r.send) throw new Error(`${r.id} has already ended (${r.status}); start a new task instead`);
      r.send(`[message from the caller]\n${args.message}`);
      return { content: [{ type: "text", text: `sent to ${r.id}` }] };
    },
  });

  api.registerTool({
    name: "task_stop",
    description: "Stop a running subagent. Its partial progress is reported as usual.",
    parameters: { type: "object", properties: idParam, required: ["id"], additionalProperties: false },
    kind: "other",
    summary: (a) => a.id,
    async execute(args) {
      const r = find(args.id);
      if (!r.stop) return { content: [{ type: "text", text: `${r.id} has already ended (${r.status})` }] };
      r.stop();
      return { content: [{ type: "text", text: `stopping ${r.id}` }] };
    },
  });

  api.registerCommand({
    name: "task",
    description: "サブエージェントの一覧・詳細・指示・停止",
    argumentHint: "[id] | send <id> <message> | stop <id>",
    complete: (prefix) =>
      [...runs.keys(), "send", "stop"].filter((v) => v.startsWith(prefix)).map((v) => ({ value: v, label: v, description: runs.get(v)?.description ?? "" })),
    run({ args }) {
      const [sub, id, ...rest] = args.trim().split(/\s+/).filter(Boolean);
      try {
        if (!sub) api.ui.notify(runs.size ? [...runs.values()].map(line).join("\n") : "サブエージェントはまだ動いていません");
        else if (sub === "stop") {
          const r = find(id ?? "");
          r.stop ? (r.stop(), api.ui.notify(`${r.id} を停止します`)) : api.ui.notify(`${r.id} は既に終了しています (${r.status})`);
        } else if (sub === "send") {
          const r = find(id ?? "");
          if (!r.send) api.ui.notify(`${r.id} は既に終了しています (${r.status})`, "warning");
          else if (!rest.length) api.ui.notify("送る内容がありません: /task send <id> <message>", "warning");
          else (r.send(`[message from the user]\n${rest.join(" ")}`), api.ui.notify(`${r.id} に送りました`));
        } else {
          const r = find(sub);
          api.ui.notify([line(r), ...r.trail.slice(-TRAIL).map((t) => `  ${t}`), r.report ? `\n${r.report}` : ""].filter(Boolean).join("\n"));
        }
      } catch (e) {
        api.ui.notify((e as Error).message, "warning");
      }
    },
  });
};
export default subagent;
