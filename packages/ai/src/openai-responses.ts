import OpenAI from "openai";
import type { ResponseCreateParamsStreaming, ResponseInputItem } from "openai/resources/responses/responses";
import { withEffort } from "./effort.ts";
import { isContextOverflow, openaiClient } from "./openai-chat.ts";
import {
  ContextOverflowError,
  computeCost,
  type Message,
  newAssistant,
  parseToolInput,
  type Provider,
  samplingFields,
  type Request,
  type StreamEvent,
  type ThinkingContent,
  type ToolCall,
  type UserContent,
} from "./types.ts";

export const openaiResponsesProvider: Provider = {
  api: "openai-responses",
  async *stream(req: Request): AsyncGenerator<StreamEvent> {
    const { model } = req;
    const client = openaiClient(req);
    const params: ResponseCreateParamsStreaming = {
      model: model.id,
      instructions: req.system,
      input: toResponsesInput(req.messages, model),
      stream: true,
      store: false,
      max_output_tokens: req.maxTokens ?? model.maxOutput,
    };
    if (req.tools.length)
      params.tools = req.tools.map((t) => ({
        type: "function",
        name: t.name,
        description: t.description,
        parameters: t.parameters,
        strict: false,
      }));
    Object.assign(params, samplingFields(req.sampling, ["temperature", "top_p"]));

    const out = newAssistant(model);
    yield { type: "start", partial: out };
    const items = new Map<string, { index: number; json?: string }>();
    const block = (id: string) => {
      const it = items.get(id);
      return it ? { it, b: out.content[it.index] } : undefined;
    };
    try {
      const stream = await withEffort(`${model.baseUrl} ${model.id}`, model.reasoning ? req.thinking : undefined, (effort) =>
        client.responses.create(
          // Stateless (store: false) reasoning continuity needs the encrypted reasoning items back.
          effort ? { ...params, reasoning: { effort: effort as never, summary: "auto" }, include: ["reasoning.encrypted_content"] } : params,
          { signal: req.signal },
        ),
      );
      for await (const ev of stream) {
        switch (ev.type) {
          case "response.output_item.added": {
            const item = ev.item;
            if (item.type === "function_call") {
              const b: ToolCall = { type: "tool_call", id: item.call_id, name: item.name, input: {} };
              out.content.push(b);
              items.set(item.id!, { index: out.content.length - 1, json: "" });
              yield { type: "toolcall_start", index: out.content.length - 1, id: b.id, name: b.name, partial: out };
            } else if (item.type === "reasoning") {
              out.content.push({ type: "thinking", thinking: "" });
              items.set(item.id, { index: out.content.length - 1 });
            } else if (item.type === "message") {
              out.content.push({ type: "text", text: "" });
              items.set(item.id, { index: out.content.length - 1 });
            }
            break;
          }
          case "response.output_text.delta": {
            const r = block(ev.item_id);
            if (r?.b?.type === "text") {
              r.b.text += ev.delta;
              yield { type: "text_delta", index: r.it.index, delta: ev.delta, partial: out };
            }
            break;
          }
          case "response.reasoning_summary_text.delta":
          case "response.reasoning_text.delta": {
            const r = block(ev.item_id);
            if (r?.b?.type === "thinking") {
              r.b.thinking += ev.delta;
              yield { type: "thinking_delta", index: r.it.index, delta: ev.delta, partial: out };
            }
            break;
          }
          case "response.reasoning_summary_part.done": {
            const r = block(ev.item_id);
            if (r?.b?.type === "thinking") r.b.thinking += "\n\n";
            break;
          }
          case "response.function_call_arguments.delta": {
            const r = block(ev.item_id);
            if (r) {
              r.it.json = (r.it.json ?? "") + ev.delta;
              yield { type: "toolcall_delta", index: r.it.index, delta: ev.delta, partial: out };
            }
            break;
          }
          case "response.output_item.done": {
            const r = block(ev.item.id!);
            if (!r) break;
            if (ev.item.type === "function_call" && r.b?.type === "tool_call")
              Object.assign(r.b, parseToolInput(ev.item.arguments || r.it.json || ""));
            else if (ev.item.type === "reasoning" && r.b?.type === "thinking") {
              (r.b as ThinkingContent).thinking = r.b.thinking.trim();
              (r.b as ThinkingContent).signature = JSON.stringify(ev.item);
            }
            break;
          }
          case "response.completed":
          case "response.incomplete": {
            const u = ev.response.usage;
            if (u) {
              const cached = u.input_tokens_details?.cached_tokens ?? 0;
              out.usage.input = u.input_tokens - cached;
              out.usage.cacheRead = cached;
              out.usage.output = u.output_tokens;
            }
            if (ev.type === "response.incomplete") {
              const reason = ev.response.incomplete_details?.reason;
              out.stopReason = reason === "content_filter" ? "refusal" : "max_tokens";
            }
            break;
          }
          case "response.failed":
            throw new Error(ev.response.error?.message ?? "response failed");
          case "error":
            if (/context/i.test(ev.message)) throw new ContextOverflowError(ev.message);
            throw new Error(ev.message);
        }
      }
    } catch (e) {
      if (e instanceof OpenAI.APIUserAbortError || req.signal?.aborted) out.stopReason = "aborted";
      else if (isContextOverflow(e)) throw new ContextOverflowError((e as Error).message);
      else throw e;
    }
    out.content = out.content.filter((c) => !(c.type === "text" && !c.text));
    if (out.stopReason === "stop" && out.content.some((c) => c.type === "tool_call")) out.stopReason = "tool_use";
    out.usage.cost = computeCost(model, out.usage);
    yield { type: "done", message: out };
  },
};

function inputContent(content: UserContent[]) {
  return content.map((c) =>
    c.type === "text"
      ? { type: "input_text" as const, text: c.text }
      : { type: "input_image" as const, image_url: `data:${c.mediaType};base64,${c.data}`, detail: "auto" as const },
  );
}

function toResponsesInput(messages: Message[], model: Request["model"]): ResponseInputItem[] {
  const out: ResponseInputItem[] = [];
  for (const m of messages) {
    if (m.role === "user") out.push({ role: "user", content: inputContent(m.content) });
    else if (m.role === "tool")
      out.push({ type: "function_call_output", call_id: m.toolCallId, output: inputContent(m.content) });
    else {
      // Encrypted reasoning only goes back to the service and model that produced it.
      const sameModel = m.api === model.api && m.provider === model.provider && m.model === model.id;
      for (const c of m.content) {
        if (c.type === "text") {
          if (c.text) out.push({ role: "assistant", content: c.text });
        } else if (c.type === "thinking") {
          // Reasoning items only carry over to the model that produced them.
          if (sameModel && c.signature) out.push(JSON.parse(c.signature));
        } else
          out.push({ type: "function_call", call_id: c.id, name: c.name, arguments: c.rawInput ?? JSON.stringify(c.input) });
      }
    }
  }
  return out;
}
