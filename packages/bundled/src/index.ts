import type { Plugin } from "@sasacode/plugin-api";
import agentJudge from "./agent-judge.ts";
import agentsMd from "./agents-md.ts";
import autoReconnect from "./auto-reconnect.ts";
import backgroundSessions from "./background-sessions.ts";
import browsr from "./browsr.ts";
import checkpoints from "./checkpoints.ts";
import compaction from "./compaction.ts";
import diagnostics from "./diagnostics.ts";
import goal from "./goal.ts";
import imageAttach from "./image-attach.ts";
import loopGuard from "./loop-guard.ts";
import openaiCodex from "./openai-codex/index.ts";
import permissionPresets from "./permission-presets.ts";
import repetitionGuard from "./repetition-guard.ts";
import subagent from "./subagent.ts";
import textToolCalls from "./text-tool-calls.ts";
import todo from "./todo.ts";
import toolRepair from "./tool-repair.ts";
import usage from "./usage.ts";
import webFetch from "./web-fetch.ts";
import webSearch from "./web-search.ts";
import writeGuard from "./write-guard.ts";

export { agentsFiles } from "./agents-md.ts";
export { compact, splitPoint, transcript } from "./compaction.ts";
export { PRESETS } from "./permission-presets.ts";
export { detectRepetition } from "./repetition-guard.ts";
export { closestName, editDistance, fitToSchema, repairJson, resolveToolName, TOOL_SYNONYMS } from "./tool-repair.ts";
export { extractTextCalls } from "./text-tool-calls.ts";
export { htmlToText, readable, readCapped } from "./web-fetch.ts";
export { formatResults, matchesQuery, privateAddress } from "./web-search.ts";
export { stripLineNumbers } from "./write-guard.ts";
export { installRustxng, managedRustxng, rustxngTarget } from "./rustxng-install.ts";
export { byDay, graph, planLines, readRecords } from "./usage.ts";
export { codexLogin, codexLogout, codexStatus, readCodexAuth } from "./openai-codex/index.ts";

/** Officially shipped plugins. Each can be turned off with plugins.disabled. */
export const bundledPlugins: Record<string, Plugin> = {
  "tool-repair": toolRepair,
  // After tool-repair: hooks run in load order, and tool-repair mends the calls this one makes.
  "text-tool-calls": textToolCalls,
  "repetition-guard": repetitionGuard,
  "loop-guard": loopGuard,
  // After tool-repair: it strips line numbers from the repaired write.
  "write-guard": writeGuard,
  "agents-md": agentsMd,
  compaction,
  diagnostics,
  subagent,
  todo,
  "web-fetch": webFetch,
  "web-search": webSearch,
  "permission-presets": permissionPresets,
  "agent-judge": agentJudge,
  browsr,
  "openai-codex": openaiCodex,
  usage,
  goal,
  "background-sessions": backgroundSessions,
  "auto-reconnect": autoReconnect,
  checkpoints,
  "image-attach": imageAttach,
};
