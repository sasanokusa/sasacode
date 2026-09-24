import { randomUUID } from "node:crypto";
import { BUILTIN_PROVIDERS } from "@sasacode/ai";
import type { Plugin } from "@sasacode/plugin-api";
import { codexProvider } from "./provider.ts";

export { codexAuthPath, readCodexAuth } from "./auth.ts";
export { codexLogin, codexLogout, codexStatus } from "./login.ts";
export { codexProvider } from "./provider.ts";

/**
 * Models from a ChatGPT plan (openai-codex/…), signed in with `sasacode login` instead of an API
 * key. The prompt cache is keyed by session, so a conversation keeps hitting it.
 */
const openaiCodex: Plugin = (api) => {
  let sessionId: string = randomUUID();
  api.on("session_start", (e) => {
    if (e.sessionId) sessionId = e.sessionId;
  });
  api.registerProvider("openai-codex", BUILTIN_PROVIDERS["openai-codex"]!, codexProvider(() => sessionId));
};
export default openaiCodex;
