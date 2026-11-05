import type { Plugin } from "@sasacode/plugin-api";
import { repairJson } from "./repair/json.ts";
import { closestName, fitToSchema, TOOL_SYNONYMS } from "./repair/schema.ts";

export { repairJson } from "./repair/json.ts";
export { closestName, editDistance, fitToSchema, TOOL_SYNONYMS } from "./repair/schema.ts";

/** Prefixes some models put before function names. */
const NAME_PREFIX = /^(functions|function|tools|tool|default_api|api)[.:/]/i;

/** Anthropic's text editor tool (str_replace_editor) names its operation in `command`. */
const EDITOR_COMMANDS = new Map([["view", "read"], ["create", "write"], ["str_replace", "edit"]]);

/**
 * The tool a model meant by `name`, among `names`: the name itself, the name with a prefix
 * (`functions.read`) or without its MCP prefix (`get_file_contents` for
 * `mcp__github__get_file_contents`), another harness's name for it (`read_file`) or, unless
 * `typos` is false, a typo. Undefined unless exactly one tool fits.
 */
export function resolveToolName(name: string, names: string[], typos = true): string | undefined {
  if (names.includes(name)) return name;
  const bare = name.replace(NAME_PREFIX, "");
  if (names.includes(bare)) return bare;
  // An MCP tool's own name means that tool, not the built-in another harness calls the same.
  const mcp = names.filter((n) => n.endsWith(`__${bare}`));
  return mcp.length ? (mcp.length === 1 ? mcp[0] : undefined) : closestName(bare, names, TOOL_SYNONYMS, typos);
}

/**
 * Repairs tool calls before they are validated: broken JSON, misspelled tool names and other
 * harnesses' names for the built-in tools, renamed arguments ("file" → "path"), and values of the
 * wrong type ("20" → 20). Only unambiguous fixes are made; the core tells the model what was
 * changed. Unknown arguments are dropped only for read-only tools; elsewhere they may carry
 * intent, so validation sends them back to the model.
 *
 * settings: { json?: boolean, names?: boolean, schema?: boolean }  (all default true)
 */
const toolRepair: Plugin = (api) => {
  const on = (key: string) => api.settings[key] !== false;
  api.on("tool_call_raw", ({ name, input, rawInput, tools }) => {
    const notes: string[] = [];
    let value = input;
    let changed = false;
    const fixed = rawInput !== undefined && on("json") ? repairJson(rawInput) : undefined;
    if (fixed) {
      value = fixed.value;
      changed = true;
      notes.push(fixed.fixes.length ? `JSON: ${fixed.fixes.join(", ")}` : "JSON re-parsed");
    }

    let tool = tools.find((t) => t.name === name);
    let newName: string | undefined;
    const names = tools.map((t) => t.name);
    let hit = !tool && on("names") ? resolveToolName(name, names) : undefined;
    if (hit) {
      // A `command` the tool does not take, naming an editor operation, picks that operation's tool.
      const props = (tools.find((t) => t.name === hit)!.parameters.properties ?? {}) as Record<string, unknown>;
      const op = typeof value.command === "string" && !("command" in props) ? EDITOR_COMMANDS.get(value.command) : undefined;
      if (op && names.includes(op)) {
        const { command, ...rest } = value;
        notes.unshift(`tool "${name}" (command "${command}") → "${op}"`);
        hit = op;
        value = rest;
        changed = true;
      } else notes.unshift(`tool "${name}" → "${hit}"`);
      tool = tools.find((t) => t.name === hit);
      newName = hit;
    }
    if (rawInput !== undefined && !fixed) return newName ? { name: newName, note: notes.join("; ") } : undefined;

    if (tool && on("schema")) {
      const fitted = fitToSchema(value, tool.parameters, { dropUnknown: tool.kind === "read" });
      if (fitted.fixes.length) {
        value = fitted.value;
        changed = true;
        notes.push(...fitted.fixes);
      }
    }
    if (!newName && !changed) return;
    return { name: newName, input: changed ? value : undefined, note: notes.join("; ") };
  });
};
export default toolRepair;
