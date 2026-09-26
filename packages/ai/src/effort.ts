// The OpenAI-style `reasoning_effort` means different things to different servers: vLLM takes "none"
// to turn thinking off, Command Code refuses "none" but takes "max", older OpenAI models take no
// effort at all. Each level therefore has a list of values to try, closest first, and the one a
// server accepted is remembered so later requests go straight to it.
import OpenAI from "openai";
import type { ThinkingLevel } from "./types.ts";

/** Values to send for a level, closest first; undefined leaves the field out (the server's default). */
const CANDIDATES: Record<ThinkingLevel, (string | undefined)[]> = {
  off: ["none", "minimal", "low", undefined],
  low: ["low", "minimal", undefined],
  medium: ["medium", undefined],
  high: ["high", "xhigh", "medium", undefined],
  xhigh: ["xhigh", "high", undefined],
  max: ["max", "xhigh", "high", undefined],
};

const accepted = new Map<string, number>();

/**
 * A 400 about the effort itself (not about the prompt or the tools). vLLM sends it as the stream's
 * first event rather than as the HTTP status, so the SDK's error carries no status there.
 */
export function isEffortRejected(e: unknown): boolean {
  if (!(e instanceof OpenAI.APIError)) return false;
  const bad = e.status === 400 || e.status === 422 || (e.status === undefined && (String(e.code) === "400" || /BadRequest|invalid_request/i.test(String(e.type))));
  return bad && /reasoning|effort|thinking/i.test(`${e.param ?? ""} ${e.message}`);
}

/**
 * Run `attempt` with the closest effort the server takes for `level`. `key` names the endpoint and
 * model; `thinking` undefined sends no effort at all.
 */
export async function withEffort<T>(key: string, thinking: ThinkingLevel | undefined, attempt: (effort: string | undefined) => Promise<T>): Promise<T> {
  if (!thinking) return attempt(undefined);
  const list = CANDIDATES[thinking];
  const k = `${key} ${thinking}`;
  for (let i = accepted.get(k) ?? 0; ; i++) {
    try {
      const result = await attempt(list[i]);
      accepted.set(k, i);
      return result;
    } catch (e) {
      if (i >= list.length - 1 || !isEffortRejected(e)) throw e;
    }
  }
}
