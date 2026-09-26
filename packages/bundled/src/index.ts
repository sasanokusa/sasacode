import type { Plugin } from "@sasacode/plugin-api";
import agentsMd from "./agents-md.ts";
import browsr from "./browsr.ts";
import compaction from "./compaction.ts";
import jevGuard from "./jev-guard.ts";
import loopGuard from "./loop-guard.ts";
import openaiCodex from "./openai-codex/index.ts";
import permissionPresets from "./permission-presets.ts";
import repetitionGuard from "./repetition-guard.ts";
import subagent from "./subagent.ts";
import todo from "./todo.ts";
import toolRepair from "./tool-repair.ts";
import webFetch from "./web-fetch.ts";

export { agentsFiles } from "./agents-md.ts";
export { compact, splitPoint, transcript } from "./compaction.ts";
export { adjust as jevAdjust, buildState as jevState, interpret as jevInterpret, parseAnswers as jevParseAnswers, pathArgs, redact, shellWords } from "./jev-guard.ts";
export { PRESETS } from "./permission-presets.ts";
export { detectRepetition } from "./repetition-guard.ts";
export { closestName, editDistance, fitToSchema, repairJson } from "./tool-repair.ts";
export { htmlToText } from "./web-fetch.ts";
export { codexLogin, codexLogout, codexStatus, readCodexAuth } from "./openai-codex/index.ts";

/** Officially shipped plugins. Each can be turned off with plugins.disabled. */
export const bundledPlugins: Record<string, Plugin> = {
  "tool-repair": toolRepair,
  "repetition-guard": repetitionGuard,
  "loop-guard": loopGuard,
  "agents-md": agentsMd,
  compaction,
  subagent,
  todo,
  "web-fetch": webFetch,
  "permission-presets": permissionPresets,
  "jev-guard": jevGuard,
  browsr,
  "openai-codex": openaiCodex,
};
