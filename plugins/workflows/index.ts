import type { Plugin } from "@sasacode/plugin-api";
import { builtins, type Workflow } from "./prompts.ts";

export function loadWorkflows(lang: "ja" | "en", settings: Record<string, unknown>): Map<string, Workflow> {
  const workflows = new Map(Object.entries(builtins(lang)));
  if (settings.workflows === undefined) return workflows;
  if (!settings.workflows || typeof settings.workflows !== "object" || Array.isArray(settings.workflows)) throw new Error("workflows must be an object");
  for (const [name, raw] of Object.entries(settings.workflows)) {
    if (!/^[a-z][a-z0-9-]{0,39}$/.test(name) || ["list", "show"].includes(name)) throw new Error(`Invalid workflow name: ${name}`);
    if (raw === false) { workflows.delete(name); continue; }
    if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new Error(`Invalid workflow: ${name}`);
    const { description, prompt } = raw as Record<string, unknown>;
    if (typeof description !== "string" || !description.trim() || description.length > 200) throw new Error(`Invalid description: ${name}`);
    if (typeof prompt !== "string" || !prompt.trim() || prompt.length > 20_000) throw new Error(`Invalid prompt: ${name}`);
    if (/\{\{(?!input\}\})[^}]*\}\}/.test(prompt)) throw new Error(`Unknown placeholder in ${name}; only {{input}} is supported`);
    workflows.set(name, { description, prompt });
  }
  return workflows;
}

export function renderPrompt(workflow: Workflow, input: string): string {
  // Function replacement leaves '$&', '$1' and user-supplied braces literal; expansion is one pass.
  return workflow.prompt.includes("{{input}}")
    ? workflow.prompt.replaceAll("{{input}}", () => input)
    : workflow.prompt + (input ? `\n\n${input}` : "");
}

const plugin: Plugin = (api) => {
  const settings = api.defineSettings({ schema: {
    type: "object", additionalProperties: false, properties: {
      workflows: { type: "object", additionalProperties: { anyOf: [
        { const: false },
        { type: "object", required: ["description", "prompt"], additionalProperties: false, properties: {
          description: { type: "string", minLength: 1, maxLength: 200, pattern: "\\S" },
          prompt: { type: "string", minLength: 1, maxLength: 20000, pattern: "\\S" },
        } },
      ] } },
    },
  } });
  const workflows = loadWorkflows(api.ui.lang, settings);
  const ja = api.ui.lang === "ja";
  const listing = () => [...workflows].map(([name, w]) => `${name}: ${w.description}`).join("\n");
  api.registerCommand({
    name: "workflow", description: ja ? "定型依頼を実行・表示する" : "Run or preview a reusable request",
    argumentHint: "[list | show <name> | <name> [request]]",
    complete: (prefix) => {
      const showing = prefix.startsWith("show ");
      const start = showing ? prefix.slice(5) : prefix;
      return [...workflows].filter(([name]) => name.startsWith(start)).map(([name, w]) => ({ value: `${showing ? "show " : ""}${name}`, label: name, description: w.description }));
    },
    run: async ({ args }) => {
      const match = args.trim().match(/^(\S+)(?:\s+([\s\S]*))?$/);
      if (!match || match[1] === "list") { api.ui.notify(listing() || (ja ? "ワークフローはありません。" : "No workflows.")); return; }
      const showing = match[1] === "show";
      const preview = showing ? (match[2] ?? "").match(/^(\S+)(?:\s+([\s\S]*))?$/) : match;
      const name = preview?.[1] ?? "";
      const workflow = workflows.get(name);
      if (!workflow) { api.ui.notify(`${ja ? "不明なワークフロー" : "Unknown workflow"}: ${name}\n${listing()}`, "error"); return; }
      const prompt = renderPrompt(workflow, preview?.[2] ?? "");
      if (showing) { await api.ui.showText({ title: `/workflow ${name}`, text: prompt }); return; }
      api.session.inject(prompt, "now");
    },
  });
};
export default plugin;
