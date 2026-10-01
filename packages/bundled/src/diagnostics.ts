import { createHash } from "node:crypto";
import type { Plugin } from "@sasacode/plugin-api";

/** Deliberately has no string-valued provider data: never log prompts, headers or signed state. */
export interface WireStats {
  requestBytes: number;
  systemBytes: number;
  toolsBytes: number;
  messages: number;
  thinkingBlocks: number;
  signedBlocks: number;
}
export function wireStats(body: string): WireStats {
  const stats: WireStats = { requestBytes: Buffer.byteLength(body), systemBytes: 0, toolsBytes: 0, messages: 0, thinkingBlocks: 0, signedBlocks: 0 };
  try {
    const j = JSON.parse(body);
    stats.systemBytes = Buffer.byteLength(JSON.stringify(j.system ?? j.instructions ?? j.messages?.filter((m: any) => m.role === "system") ?? ""));
    stats.toolsBytes = Buffer.byteLength(JSON.stringify(j.tools ?? []));
    const messages = j.messages ?? j.input ?? [];
    if (!Array.isArray(messages)) return stats;
    stats.messages = messages.length;
    for (const message of messages) {
      const blocks = Array.isArray(message.content) ? message.content : [];
      for (const b of [message, ...blocks, ...(message.reasoning_details ?? [])]) {
        if (b.type === "thinking" || b.type === "redacted_thinking" || String(b.type ?? "").startsWith("reasoning")) stats.thinkingBlocks++;
        if (b.signature || b.data || b.encrypted_content) stats.signedBlocks++;
      }
      if (message.reasoning_content || message.reasoning) stats.thinkingBlocks++;
    }
  } catch { /* Unknown wire format: its byte length alone is useful. */ }
  return stats;
}
const COUNTERS = new Set(["input_tokens", "output_tokens", "prompt_tokens", "completion_tokens", "cached_tokens", "cache_write_tokens", "cache_read_input_tokens", "cache_creation_input_tokens", "prompt_cache_hit_tokens", "prompt_cache_miss_tokens", "reasoning_tokens", "cost"]);
export interface ResponseStats { usage: Record<string, number>; transformations: number; drops: Record<string, number> }
export function responseStats(event: any, stats: ResponseStats): void {
  const visit = (value: any, prefix = "") => {
    if (!value || typeof value !== "object") return;
    for (const [key, v] of Object.entries(value)) {
      if (COUNTERS.has(key) && typeof v === "number" && Number.isFinite(v) && v >= 0) stats.usage[prefix + key] = v;
      else if (["input_tokens_details", "output_tokens_details", "prompt_tokens_details", "completion_tokens_details"].includes(key)) visit(v, `${key}.`);
    }
  };
  visit(event.usage); visit(event.message?.usage); visit(event.response?.usage);
  // Only count documented drop reasons. Paths and unknown strings may contain user data.
  const transformations = event.message?.input_transformations ?? event.input_transformations;
  if (Array.isArray(transformations)) for (const t of transformations) {
    stats.transformations++;
    for (const reason of ["prefix_binding_mismatch", "organization_binding_mismatch"])
      if (t.reason === reason) stats.drops[reason] = (stats.drops[reason] ?? 0) + 1;
  }
}

/** Observes the fetch actually used by the SDK, including its retries, without consuming ahead. */
export function diagnosticFetch(inner: typeof fetch, record: (data: Record<string, unknown>) => void): typeof fetch {
  let attempt = 0, lastDigest: string | undefined;
  const save = (data: Record<string, unknown>) => { try { record(data); } catch { /* Diagnostics cannot fail generation. */ } };
  return (async (input: Parameters<typeof fetch>[0], init?: RequestInit) => {
    const body = typeof init?.body === "string" ? init.body : "";
    const digest = createHash("sha256").update(body).digest("hex");
    const meta = { attempt: ++attempt, repeatedBody: digest === lastDigest, ...wireStats(body) };
    lastDigest = digest;
    let response: Response;
    try { response = await inner(input, init); }
    catch (error) { save({ ...meta, outcome: init?.signal?.aborted ? "aborted" : "transport_error" }); throw error; }
    const stats: ResponseStats = { usage: {}, transformations: 0, drops: {} };
    if (!response.ok || !response.body || !response.headers.get("content-type")?.includes("text/event-stream")) {
      save({ ...meta, status: response.status }); return response;
    }
    // SSE frames may straddle UTF-8 chunks. Forward their bytes untouched; bound the parser buffer.
    const decoder = new TextDecoder();
    let pending = "";
    const observe = (text: string) => {
      pending += text;
      const lines = pending.split(/\r?\n/); pending = lines.pop() ?? "";
      if (pending.length > 1_048_576) pending = "";
      for (const line of lines) if (line.length <= 1_048_576 && line.startsWith("data:")) {
        try { responseStats(JSON.parse(line.slice(5)), stats); } catch { /* Includes [DONE]. */ }
      }
    };
    const reader = response.body.getReader();
    let saved = false;
    const finish = (outcome: string) => { if (!saved) { saved = true; save({ ...meta, status: response.status, outcome, ...stats }); } };
    const observed = new ReadableStream<Uint8Array>({
      async pull(controller) {
        try {
          const next = await reader.read();
          if (next.done) { observe(decoder.decode() + "\n"); finish("complete"); controller.close(); }
          else { observe(decoder.decode(next.value, { stream: true })); controller.enqueue(next.value); }
        } catch (error) { finish("stream_error"); controller.error(error); }
      },
      async cancel(reason) { finish("cancelled"); await reader.cancel(reason); },
    }, { highWaterMark: 0 });
    return new Response(observed, { status: response.status, statusText: response.statusText, headers: response.headers });
  }) as typeof fetch;
}
const diagnostics: Plugin = (api) => {
  const settings = api.defineSettings<{ enabled: boolean; persist: boolean }>({
    schema: { type: "object", properties: { enabled: { type: "boolean" }, persist: { type: "boolean" } }, additionalProperties: false },
    defaults: { enabled: false, persist: false },
  });
  let enabled = settings.enabled;
  const records: Record<string, unknown>[] = [];
  const record = (data: Record<string, unknown>) => {
    records.push({ at: Date.now(), ...data });
    if (records.length > 100) records.shift();
    if (settings.persist) api.session.append("diagnostic", records.at(-1));
  };
  api.on("before_request", ({ fetch: inner }, ctx) => {
    if (enabled) return { fetch: diagnosticFetch(inner ?? globalThis.fetch, data => record({ kind: "request", agent: ctx.agent ?? "main", ...data })) };
  });
  api.on("assistant_message", ({ message }, ctx) => {
    if (enabled) record({ kind: "assistant", agent: ctx.agent ?? "main", stopReason: message.stopReason, usage: Object.fromEntries(Object.entries(message.usage).filter(([k, v]) => ["input", "output", "cacheRead", "cacheWrite", "cost"].includes(k) && typeof v === "number" && Number.isFinite(v))) });
  });
  api.on("tool_result", ({ result, ran }, ctx) => {
    if (enabled) record({ kind: "tool", agent: ctx.agent ?? "main", ran, isError: !!result.isError, changed: typeof result.details?.changed === "boolean" ? result.details.changed : undefined });
  });
  api.on("session_end", () => { records.length = 0; });
  api.registerCommand({
    name: "diagnostics", description: api.ui.lang === "ja" ? "API 診断: on / off / show / clear（内容や秘密値は記録しない）" : "API diagnostics: on / off / show / clear (no contents or secrets)",
    async run({ args }) {
      if (args.trim() === "on") enabled = true;
      else if (args.trim() === "off") enabled = false;
      else if (args.trim() === "clear") records.length = 0;
      else { await api.ui.showText({ title: "API diagnostics", text: JSON.stringify({ enabled, records }, null, 2) }); return; }
      api.ui.notify(`diagnostics ${enabled ? "on" : "off"}; ${records.length} records`);
    },
  });
};
export default diagnostics;
