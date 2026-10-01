import { createHash } from "node:crypto";
import { newAssistant, type AssistantMessage, type ModelInfo } from "./types.ts";

export const replayScope = (m: ModelInfo): string => createHash("sha256").update(JSON.stringify([
  m.api, m.provider, m.id, m.baseUrl ?? (m.api === "anthropic" ? "https://api.anthropic.com" : "https://api.openai.com/v1"),
])).digest("hex");
export const canReplay = (m: AssistantMessage, target: ModelInfo): boolean => m.api === target.api && m.provider === target.provider && m.model === target.id && m.replayScope === replayScope(target);
export const newReply = (model: ModelInfo): AssistantMessage => ({ ...newAssistant(model), replayScope: replayScope(model) });
