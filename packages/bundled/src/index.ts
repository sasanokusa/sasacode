import type { Plugin } from "@sasacode/plugin-api";
import agentsMd from "./agents-md.ts";
import compaction from "./compaction.ts";
import permissionPresets from "./permission-presets.ts";
import subagent from "./subagent.ts";
import todo from "./todo.ts";
import webFetch from "./web-fetch.ts";

export { agentsFiles } from "./agents-md.ts";
export { compact, splitPoint, transcript } from "./compaction.ts";
export { PRESETS } from "./permission-presets.ts";
export { htmlToText } from "./web-fetch.ts";

/** Officially shipped plugins. Each can be turned off with plugins.disabled. */
export const bundledPlugins: Record<string, Plugin> = {
  "agents-md": agentsMd,
  compaction,
  subagent,
  todo,
  "web-fetch": webFetch,
  "permission-presets": permissionPresets,
};
