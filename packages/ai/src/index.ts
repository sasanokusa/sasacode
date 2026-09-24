import { anthropicProvider } from "./anthropic.ts";
import { openaiChatProvider } from "./openai-chat.ts";
import { openaiResponsesProvider } from "./openai-responses.ts";
import type { Provider } from "./types.ts";

export * from "./types.ts";
export * from "./list-models.ts";
export * from "./models.ts";
export * from "./replay.ts";

const providers = new Map<string, Provider>([
  ["anthropic", anthropicProvider],
  ["openai-chat", openaiChatProvider],
  ["openai-responses", openaiResponsesProvider],
]);

/** Register an implementation for an api name (also used by tests to install a replay provider). */
export function registerApi(api: string, provider: Provider): void {
  providers.set(api, provider);
}

export function getProvider(api: string): Provider {
  const p = providers.get(api);
  if (!p) throw new Error(`no provider registered for api "${api}"`);
  return p;
}
