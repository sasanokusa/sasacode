import OpenAI from "openai";
import type {
  ChatCompletionContentPart,
  ChatCompletionCreateParamsStreaming,
  ChatCompletionMessageParam,
} from "openai/resources/chat/completions";
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
} from "./types.ts";

export function openaiClient(req: Request): OpenAI {
  return new OpenAI({
    // Local servers (Ollama, vLLM) accept any key; the SDK refuses an empty one.
    apiKey: req.apiKey || "none",
    baseURL: req.model.baseUrl,
    maxRetries: req.maxRetries ?? 8,
    defaultHeaders: req.model.headers,
  });
}

export function isContextOverflow(e: unknown): boolean {
  if (!(e instanceof OpenAI.APIError)) return false;
  return e.code === "context_length_exceeded" || /context length|context window|too many tokens|maximum context/i.test(e.message);
}

export const openaiChatProvider: Provider = {
  api: "openai-chat",
  async *stream(req: Request): AsyncGenerator<StreamEvent> {
    const { model } = req;
    const client = openaiClient(req);
    const params: ChatCompletionCreateParamsStreaming = {
      model: model.id,
      messages: [{ role: "system", content: req.system }, ...toChatMessages(req.messages)],
      stream: true,
      stream_options: { include_usage: true },
      max_completion_tokens: req.maxTokens ?? model.maxOutput,
    };
    if (req.tools.length)
      params.tools = req.tools.map((t) => ({
        type: "function",
        function: { name: t.name, description: t.description, parameters: t.parameters },
      }));
    if (model.reasoning && req.thinking && req.thinking !== "off")
      params.reasoning_effort = (req.thinking === "max" || req.thinking === "xhigh" ? "high" : req.thinking) as never;
    Object.assign(params, samplingFields(req.sampling, ["temperature", "top_p", "frequency_penalty", "presence_penalty"]));

    const out = newAssistant(model);
    yield { type: "start", partial: out };
    let thinking: ThinkingContent | undefined;
    let text: { type: "text"; text: string } | undefined;
    const calls = new Map<number, { block: ToolCall; json: string; index: number }>();
    try {
      const stream = await client.chat.completions.create(params, { signal: req.signal });
      for await (const chunk of stream) {
        if (chunk.usage) {
          const cached = chunk.usage.prompt_tokens_details?.cached_tokens ?? 0;
          out.usage.input = chunk.usage.prompt_tokens - cached;
          out.usage.cacheRead = cached;
          out.usage.output = chunk.usage.completion_tokens;
        }
        const choice = chunk.choices[0];
        if (!choice) continue;
        const delta = choice.delta as typeof choice.delta & { reasoning?: string; reasoning_content?: string };
        // Vendors disagree on the field name for reasoning text.
        const r = delta.reasoning_content ?? delta.reasoning;
        if (r) {
          if (!thinking) {
            thinking = { type: "thinking", thinking: "" };
            out.content.push(thinking);
          }
          thinking.thinking += r;
          yield { type: "thinking_delta", index: out.content.indexOf(thinking), delta: r, partial: out };
        }
        if (delta.content) {
          if (!text) {
            text = { type: "text", text: "" };
            out.content.push(text);
          }
          text.text += delta.content;
          yield { type: "text_delta", index: out.content.indexOf(text), delta: delta.content, partial: out };
        }
        for (const tc of delta.tool_calls ?? []) {
          let c = calls.get(tc.index);
          if (!c) {
            const block: ToolCall = { type: "tool_call", id: tc.id ?? `call_${tc.index}`, name: tc.function?.name ?? "", input: {} };
            out.content.push(block);
            c = { block, json: "", index: out.content.length - 1 };
            calls.set(tc.index, c);
            yield { type: "toolcall_start", index: c.index, id: block.id, name: block.name, partial: out };
          }
          if (tc.function?.arguments) {
            c.json += tc.function.arguments;
            yield { type: "toolcall_delta", index: c.index, delta: tc.function.arguments, partial: out };
          }
        }
        if (choice.finish_reason === "length") out.stopReason = "max_tokens";
        else if (choice.finish_reason === "content_filter") out.stopReason = "refusal";
      }
    } catch (e) {
      if (e instanceof OpenAI.APIUserAbortError || req.signal?.aborted) out.stopReason = "aborted";
      else if (isContextOverflow(e)) throw new ContextOverflowError((e as Error).message);
      else throw e;
    }
    for (const c of calls.values()) Object.assign(c.block, parseToolInput(c.json));
    if (out.stopReason === "stop" && calls.size) out.stopReason = "tool_use";
    out.usage.cost = computeCost(model, out.usage);
    yield { type: "done", message: out };
  },
};

function toChatMessages(messages: Message[]): ChatCompletionMessageParam[] {
  const out: ChatCompletionMessageParam[] = [];
  const pendingImages: ChatCompletionContentPart[] = [];
  for (const m of messages) {
    if (m.role === "user") {
      out.push({
        role: "user",
        content: m.content.map((c) =>
          c.type === "text"
            ? { type: "text", text: c.text }
            : { type: "image_url", image_url: { url: `data:${c.mediaType};base64,${c.data}` } },
        ),
      });
    } else if (m.role === "assistant") {
      const text = m.content.filter((c) => c.type === "text").map((c) => c.text).join("");
      const tools = m.content.filter((c) => c.type === "tool_call");
      if (!text && !tools.length) continue;
      out.push({
        role: "assistant",
        content: text || null,
        ...(tools.length
          ? {
              tool_calls: tools.map((t) => ({
                id: t.id,
                type: "function" as const,
                function: { name: t.name, arguments: t.rawInput ?? JSON.stringify(t.input) },
              })),
            }
          : {}),
      });
    } else {
      // Chat Completions tool messages are text-only; images go in a follow-up user message.
      const text = m.content.filter((c) => c.type === "text").map((c) => c.text).join("\n");
      out.push({ role: "tool", tool_call_id: m.toolCallId, content: text || (m.isError ? "error" : "ok") });
      for (const c of m.content)
        if (c.type === "image")
          pendingImages.push({ type: "image_url", image_url: { url: `data:${c.mediaType};base64,${c.data}` } });
      continue;
    }
    flush();
  }
  flush();
  return out;

  function flush() {
    if (!pendingImages.length) return;
    // Must come after all tool messages of a turn; insert before the message just pushed if it isn't a tool.
    const imgs = pendingImages.splice(0);
    const msg: ChatCompletionMessageParam = { role: "user", content: [{ type: "text", text: "Images from tool results:" }, ...imgs] };
    const last = out[out.length - 1];
    if (last && last.role !== "tool") out.splice(out.length - 1, 0, msg);
    else out.push(msg);
  }
}
