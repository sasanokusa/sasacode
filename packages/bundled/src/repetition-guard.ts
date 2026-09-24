import type { AssistantMessage, DeltaKind, Plugin, SamplingOptions } from "@sasacode/plugin-api";

export interface Repetition {
  /** The repeated unit. */
  unit: string;
  count: number;
}

export interface RepetitionLimits {
  /** Consecutive copies needed. */
  minRepeats: number;
  /** Total characters the copies must cover. */
  minSpan: number;
  /** Shortest unit for the normal rule (short runs like "-----" are usually intentional). */
  minUnit: number;
  /** Units shorter than minUnit ("なるほど。") stop only after covering this many characters. */
  shortMinSpan: number;
}

/**
 * Does `text` end with the same unit repeated back to back? The period is found from the nearest
 * earlier occurrence of the last few characters, then copies are counted backwards.
 */
export function detectRepetition(text: string, limits: RepetitionLimits): Repetition | undefined {
  if (text.length < limits.minSpan) return undefined;
  const probe = text.slice(-24);
  const prev = text.lastIndexOf(probe, text.length - probe.length - 1);
  if (prev < 0) return undefined;
  const period = text.length - probe.length - prev;
  const unit = text.slice(-period);
  let count = 1;
  while (text.length - (count + 1) * period >= 0 && text.slice(text.length - (count + 1) * period, text.length - count * period) === unit) count++;
  const span = count * period;
  if (period < limits.minUnit) return span >= limits.shortMinSpan && count >= 20 ? { unit, count } : undefined;
  return count >= limits.minRepeats && span >= limits.minSpan ? { unit, count } : undefined;
}

const INJECT =
  "[harness] Your previous response was stopped because it kept repeating the same text. Do not repeat it; continue the task from where it was still useful.";

/**
 * Stops a response that loops on the same text (common with small or heavily quantized models),
 * trims the repeats, and tells the model. Tool-call arguments get looser limits, since file
 * contents can repeat legitimately.
 *
 * settings: { minRepeats?, minSpan?, minUnit?, shortMinSpan?, toolcallMinRepeats?, toolcallMinSpan?,
 *             toolcallShortMinSpan?, checkEvery?, maxStopsPerRun?, samplingAfterStop?: SamplingOptions }
 */
const repetitionGuard: Plugin = (api) => {
  const num = (k: string, d: number) => (typeof api.settings[k] === "number" ? (api.settings[k] as number) : d);
  const prose: RepetitionLimits = {
    minRepeats: num("minRepeats", 4),
    minSpan: num("minSpan", 200),
    minUnit: num("minUnit", 10),
    shortMinSpan: num("shortMinSpan", 400),
  };
  const limits: Record<DeltaKind, RepetitionLimits> = {
    text: prose,
    thinking: prose,
    toolcall: {
      minRepeats: num("toolcallMinRepeats", 8),
      minSpan: num("toolcallMinSpan", 2000),
      minUnit: num("minUnit", 10),
      shortMinSpan: num("toolcallShortMinSpan", 4000),
    },
  };
  const checkEvery = num("checkEvery", 128);
  const maxStops = num("maxStopsPerRun", 2);
  const samplingAfterStop = api.settings.samplingAfterStop as SamplingOptions | undefined;

  const lastChecked = new WeakMap<Readonly<AssistantMessage>, Map<number, number>>();
  // Keyed by timestamp: the finished message is not always the same object as the streamed partial.
  const found = new Map<number, Repetition>();
  let stops = 0;
  let adjustNext = false;

  api.on("user_prompt", () => {
    stops = 0;
  });

  api.on("stream_delta", ({ kind, index, text, message }) => {
    // Checking every delta would be quadratic; look again only after `checkEvery` new characters.
    const seen = lastChecked.get(message) ?? new Map<number, number>();
    lastChecked.set(message, seen);
    if (text.length - (seen.get(index) ?? 0) < checkEvery) return;
    seen.set(index, text.length);
    const rep = detectRepetition(text, limits[kind]);
    if (!rep) return;
    found.set(message.timestamp, rep);
    return { stop: `repeating ${JSON.stringify(rep.unit.slice(0, 40))} ×${rep.count} in ${kind}` };
  });

  api.on("assistant_message", ({ message, stopped }) => {
    if (stopped?.plugin !== api.name) return;
    stops++;
    adjustNext = !!samplingAfterStop;
    const rep = found.get(message.timestamp);
    found.delete(message.timestamp);
    // Keep one copy of the repeated unit. Thinking and tool-call blocks were already dropped by the core.
    if (rep)
      for (const block of message.content) {
        if (block.type !== "text") continue;
        const at = block.text.indexOf(rep.unit + rep.unit);
        if (at >= 0) block.text = block.text.slice(0, at + rep.unit.length);
      }
    return stops <= maxStops ? { message, inject: INJECT } : { message };
  });

  api.on("before_request", () => {
    if (!adjustNext) return;
    adjustNext = false;
    return { sampling: samplingAfterStop };
  });
};
export default repetitionGuard;
