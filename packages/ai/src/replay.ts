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

export interface ReplayOptions {
  api?: string;
  /**
   * For responses without recorded deltas: stream text, thinking and tool-call JSON in chunks of
   * this many characters (default: one delta per block).
   */
  chunkSize?: number;
}

type Delta = Recording["deltas"][number];

function chunks(s: string, size: number | undefined): string[] {
  if (!size || size <= 0 || s.length <= size) return [s];
  const out: string[] = [];
  for (let i = 0; i < s.length; i += size) out.push(s.slice(i, i + size));
  return out;
}

/**
 * Replays recordings in order, one per request: the recorded deltas when there are any,
 * otherwise each block split by `chunkSize`. Also accepts bare assistant messages for
 * hand-written scripts. An abort is honoured between deltas, like a real stream.
 */
export function replayProvider(
  source: string | Array<Recording | AssistantMessage>,
  options: string | ReplayOptions = {},
): Provider & { requests: Request[] } {
  const opts: ReplayOptions = typeof options === "string" ? { api: options } : options;
  const items: Array<Recording | AssistantMessage> =
    typeof source === "string"
      ? readFileSync(source, "utf8")
          .split("\n")
          .filter((l) => l.trim())
          .map((l) => JSON.parse(l))
      : [...source];
  const requests: Request[] = [];
  return {
    api: opts.api ?? "replay",
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
        const recorded = rec.deltas.filter((d) => d.index === index);
        const source = block.type === "text" ? block.text : block.type === "thinking" ? block.thinking : (block.rawInput ?? JSON.stringify(block.input));
        const kind: Delta["type"] = block.type === "text" ? "text_delta" : block.type === "thinking" ? "thinking_delta" : "toolcall_delta";
        const deltas = recorded.length ? recorded.map((d) => d.delta) : chunks(source, opts.chunkSize);
        if (block.type === "tool_call") {
          partial.content[index] = { type: "tool_call", id: block.id, name: block.name, input: {} };
          yield { type: "toolcall_start", index, id: block.id, name: block.name, partial };
        } else partial.content[index] = block.type === "text" ? { type: "text", text: "" } : { type: "thinking", thinking: "" };
        for (const delta of deltas) {
          await Promise.resolve();
          if (req.signal?.aborted) {
            partial.stopReason = "aborted";
            // Like a real stream: finished blocks and the text so far stay; a half-streamed call does not.
            partial.content = partial.content.filter((c, i) => i < index || c.type !== "tool_call");
            yield { type: "done", message: partial };
            return;
          }
          const b = partial.content[index]!;
          if (b.type === "text") b.text += delta;
          else if (b.type === "thinking") b.thinking += delta;
          yield { type: kind, index, delta, partial } as StreamEvent;
        }
        partial.content[index] = block; // complete (signature, parsed input)
      }
      yield { type: "done", message };
    },
  };
}
