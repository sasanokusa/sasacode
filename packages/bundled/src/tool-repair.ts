import type { Plugin } from "@sasacode/plugin-api";
import { repairJson } from "./repair/json.ts";
import { closestName, fitToSchema } from "./repair/schema.ts";

export { repairJson } from "./repair/json.ts";
export { closestName, editDistance, fitToSchema } from "./repair/schema.ts";

/** Prefixes some models put before function names. */
const NAME_PREFIX = /^(functions|function|tools|tool|default_api|api)[.:/]/i;

/**
 * Repairs tool calls before they are validated: broken JSON, misspelled tool names, renamed
 * arguments ("file" → "path"), and values of the wrong type ("20" → 20). Only unambiguous fixes
 * are made; the core tells the model what was changed. Unknown arguments are dropped only for
 * read-only tools; elsewhere they may carry intent, so validation sends them back to the model.
 *
 * settings: { json?: boolean, names?: boolean, schema?: boolean }  (all default true)
 */
const toolRepair: Plugin = (api) => {
  const on = (key: string) => api.settings[key] !== false;
  api.on("tool_call_raw", ({ name, input, rawInput, tools }) => {
    const notes: string[] = [];
    let tool = tools.find((t) => t.name === name);
    let newName: string | undefined;
    if (!tool && on("names")) {
      const bare = name.replace(NAME_PREFIX, "");
      const hit = tools.find((t) => t.name === bare) ?? tools.find((t) => t.name === closestName(bare, tools.map((x) => x.name)));
      if (hit) {
        tool = hit;
        newName = hit.name;
        notes.push(`tool "${name}" → "${hit.name}"`);
      }
    }

    let value = input;
    let changed = false;
    if (rawInput !== undefined) {
      const fixed = on("json") ? repairJson(rawInput) : undefined;
      if (!fixed) return newName ? { name: newName, note: notes.join("; ") } : undefined;
      value = fixed.value;
      changed = true;
      notes.push(fixed.fixes.length ? `JSON: ${fixed.fixes.join(", ")}` : "JSON re-parsed");
    }
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
