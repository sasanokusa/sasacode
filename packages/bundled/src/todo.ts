import { harnessNote } from "@sasacode/host";
import type { Plugin } from "@sasacode/plugin-api";

type Status = "pending" | "in_progress" | "completed";
interface Todo {
  content: string;
  status: Status;
}

const MARK: Record<Status, string> = { pending: "☐", in_progress: "◐", completed: "☑" };

/**
 * A task list the model maintains; kept in the session log and shown in the status line.
 * Models of every size write the list and then forget it: when unfinished items sit through
 * `remindAfter` tool calls without an update, a reminder follows the tool results: a user turn of its
 * own, kept in the history. At the end of a tool's output it was read as output and passed over (17
 * reminders in the session logs, one update right after).
 *
 * settings: { remindAfter?: number (default 10, 0 = off) }
 */
const todo: Plugin = (api) => {
  const remindAfter = typeof api.settings.remindAfter === "number" ? api.settings.remindAfter : 10;
  let todos: Todo[] = [];
  /** Tool calls of the main agent since the list was last written. */
  let sinceUpdate = 0;
  const show = () => {
    if (!todos.length) return api.ui.setStatus("todos", undefined);
    const done = todos.filter((t) => t.status === "completed").length;
    const current = todos.find((t) => t.status === "in_progress");
    api.ui.setStatus("todos", `TODO ${done}/${todos.length}${current ? `: ${current.content}` : ""}`);
  };
  api.on("session_start", () => {
    todos = (api.session.entries("todos").at(-1) as Todo[] | undefined) ?? [];
    sinceUpdate = 0;
    show();
  });
  api.on("tool_result", ({ call }, ctx) => {
    // Subagents have no list of their own (todo_write is kept from them).
    if (ctx.agent || !remindAfter) return;
    if (call.name === "todo_write") sinceUpdate = 0;
    else if (todos.some((t) => t.status !== "completed")) sinceUpdate++;
  });
  api.on("turn_end", ({ message }, ctx) => {
    // After a reply that called tools only: an inject after the final answer would start another turn.
    if (ctx.agent || !remindAfter || sinceUpdate < remindAfter || !message.content.some((b) => b.type === "tool_call")) return;
    const list = todos.map((t) => `${MARK[t.status]} ${t.content}`).join(", ");
    const calls = sinceUpdate;
    sinceUpdate = 0;
    return {
      inject: harnessNote(
        `Your task list has not been updated for ${calls} tool calls and still reads: ${list}. If a step is done or another has started, call todo_write now to update it; otherwise carry on.`,
      ),
    };
  });
  api.permissions.addRules({ allow: ["todo_write"] });
  api.registerTool({
    name: "todo_write",
    description:
      "Replace your task list for the current work. Useful for multi-step tasks: list the steps, keep exactly one in_progress, and mark steps completed as you finish them.",
    parameters: {
      type: "object",
      properties: {
        todos: {
          type: "array",
          items: {
            type: "object",
            properties: {
              content: { type: "string" },
              status: { type: "string", enum: ["pending", "in_progress", "completed"] },
            },
            required: ["content", "status"],
            additionalProperties: false,
          },
        },
      },
      required: ["todos"],
      additionalProperties: false,
    },
    kind: "other",
    alwaysLoad: true,
    summary: (a) => `${a.todos.length} items`,
    async execute(args: { todos: Todo[] }) {
      todos = args.todos;
      api.session.append("todos", todos);
      show();
      const list = todos.map((t) => `${MARK[t.status]} ${t.content}`).join("\n");
      return { content: [{ type: "text", text: `Task list updated:\n${list}` }], details: { todos } };
    },
  });
  api.ui.registerToolRenderer("todo_write", (_call, result) => {
    const list = result?.details?.todos as Todo[] | undefined;
    if (!list) return undefined;
    return list.map((t) => `    ${MARK[t.status]} ${t.status === "completed" ? `\x1b[9m${t.content}\x1b[29m` : t.content}`);
  });
};
export default todo;
