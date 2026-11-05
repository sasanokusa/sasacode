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
 * `remindAfter` tool calls without an update, the next result carries a one-line reminder.
 *
 * settings: { remindAfter?: number (default 15, 0 = off) }
 */
const todo: Plugin = (api) => {
  const remindAfter = typeof api.settings.remindAfter === "number" ? api.settings.remindAfter : 15;
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
  api.on("tool_result", ({ call, result }, ctx) => {
    // Subagents have no list of their own (todo_write is kept from them).
    if (ctx.agent || !remindAfter) return;
    if (call.name === "todo_write") return void (sinceUpdate = 0);
    if (!todos.some((t) => t.status !== "completed") || ++sinceUpdate < remindAfter) return;
    sinceUpdate = 0;
    const list = todos.map((t) => `${MARK[t.status]} ${t.content}`).join(", ");
    const note = `[harness] Your task list has not been updated for ${remindAfter} tool calls: ${list}. Update it with todo_write if the state changed.`;
    return { result: { ...result, content: [...result.content, { type: "text", text: note }] } };
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
