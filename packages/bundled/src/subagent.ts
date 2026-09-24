import type { Plugin } from "@sasacode/plugin-api";

/** `task`: hand a self-contained job to a fresh agent loop and get its final report back. */
const subagent: Plugin = (api) => {
  // The subagent's own tool calls go through permissions, so starting one needs no approval.
  api.permissions.addRules({ allow: ["task"] });
  api.registerTool({
    name: "task",
    description:
      "Run a subagent with a fresh context on a self-contained task (research, a broad search, an independent change). It has the same tools except task, and returns only its final report. Several task calls in one turn run in parallel.",
    parameters: {
      type: "object",
      properties: {
        description: { type: "string", description: "Short label (3-6 words)" },
        prompt: { type: "string", description: "Complete instructions; the subagent sees nothing else" },
        model: { type: "string", description: 'Optional "<provider>/<model>"' },
      },
      required: ["description", "prompt"],
      additionalProperties: false,
    },
    kind: "other",
    concurrent: true,
    alwaysLoad: true,
    summary: (a) => a.description,
    async execute(args, ctx) {
      const r = await api.agent.run({
        prompt: `${args.prompt}\n\nWhen done, reply with a concise report of what you found or did; it is all the caller will see.`,
        model: args.model,
        excludeTools: ["task"],
        signal: ctx.signal,
        onProgress: (t) => ctx.onUpdate?.(`${t}\n`),
      });
      const text = r.text || "(the subagent returned no text)";
      return { content: [{ type: "text", text: r.cause === "done" ? text : `${text}\n[subagent stopped: ${r.cause}]` }], isError: r.cause === "error" };
    },
  });
};
export default subagent;
