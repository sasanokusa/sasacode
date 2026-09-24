import type { Plugin } from "@sasacode/plugin-api";
import { bashTool } from "./bash.ts";
import { editTool } from "./edit.ts";
import { readTool } from "./read.ts";
import { writeTool } from "./write.ts";

export { bashTool, editTool, readTool, writeTool };
export { lineDiff } from "./util.ts";

/** The built-in tools, registered through the same API as any plugin (P2). */
const builtinTools: Plugin = (api) => {
  for (const t of [readTool, writeTool, editTool, bashTool]) api.registerTool(t);
};
export default builtinTools;
