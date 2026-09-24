import { appendFileSync, readFileSync } from "node:fs";
import type { AssistantMessage, Provider, Request, StreamEvent } from "./types.ts";

/**
 * Recording format: one JSON line per request, holding the final assistant message plus
 * the deltas that produced it. Replaying needs no network, so loops and plugins can be
 * tested end to end without an LLM.
 */
export interface Recording {
  request: { model: string; messageCount: number; tools: string[] };
  deltas: { type: "text_delta" | "thinking_delta" | "toolcall_delta"; index: number; delta: string }[];
  message: AssistantMessage;
}

export function recordingProvider(inner: Provider, file: string): Provider {
  return {
    api: inner.api,
    async *stream(req: Request): AsyncGenerator<StreamEvent> {
      const deltas: Recording["deltas"] = [];
      for await (const ev of inner.stream(req)) {
        if (ev.type === "text_delta" || ev.type === "thinking_delta" || ev.type === "toolcall_delta")
          deltas.push({ type: ev.type, index: ev.index, delta: ev.delta });
        if (ev.type === "done") {
          const rec: Recording = {
            request: { model: req.model.id, messageCount: req.messages.length, tools: req.tools.map((t) => t.name) },
            deltas,
            message: ev.message,
          };
          appendFileSync(file, `${JSON.stringify(rec)}\n`);
        }
        yield ev;
      }
    },
  };
}

/** Replays recordings in order, one per request. Also accepts bare assistant messages for hand-written scripts. */
export function replayProvider(source: string | Array<Recording | AssistantMessage>, api = "replay"): Provider & { requests: Request[] } {
  const items: Array<Recording | AssistantMessage> =
    typeof source === "string"
      ? readFileSync(source, "utf8")
          .split("\n")
          .filter((l) => l.trim())
          .map((l) => JSON.parse(l))
      : [...source];
  const requests: Request[] = [];
  return {
    api,
    requests,
    async *stream(req: Request): AsyncGenerator<StreamEvent> {
      requests.push(structuredClone({ ...req, signal: undefined }));
      const item = items.shift();
      if (!item) throw new Error("replay: no more recorded responses");
      const rec: Recording = "role" in item ? { request: {} as Recording["request"], deltas: [], message: item } : item;
      const message: AssistantMessage = structuredClone(rec.message);
      const partial: AssistantMessage = { ...message, content: [] };
      yield { type: "start", partial };
      for (const [index, block] of message.content.entries()) {
        partial.content[index] = block;
        if (req.signal?.aborted) {
          partial.stopReason = "aborted";
          partial.content = partial.content.slice(0, index);
          yield { type: "done", message: partial };
          return;
        }
        if (block.type === "text") yield { type: "text_delta", index, delta: block.text, partial };
        else if (block.type === "thinking") yield { type: "thinking_delta", index, delta: block.thinking, partial };
        else yield { type: "toolcall_start", index, id: block.id, name: block.name, partial };
        await Promise.resolve();
      }
      yield { type: "done", message };
    },
  };
}
