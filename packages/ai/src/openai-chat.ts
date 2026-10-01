import OpenAI from "openai";
import type {
  ChatCompletionChunk,
  ChatCompletionContentPart,
  ChatCompletionCreateParamsStreaming,
  ChatCompletionMessageParam,
} from "openai/resources/chat/completions";
import {
  ContextOverflowError,
  computeCost,
  type Message,
  type ModelInfo,
  parseToolInput,
  type Provider,
  reportedCost,
  samplingFields,
  type Request,
  type StreamEvent,
  type ThinkingContent,
  markIfCut,
  type ToolCall,
  type Usage,
} from "./types.ts";
import { withEffort } from "./effort.ts";
import { canReplay, replayScope, newReply as newAssistant } from "./replay.ts";
const openrouter = (m: ModelInfo) => { try { return new URL(m.baseUrl ?? "").origin === "https://openrouter.ai"; } catch { return false; } };

export function openaiClient(req: Request): OpenAI {
  return new OpenAI({
    // Local servers (Ollama, vLLM) accept any key; the SDK refuses an empty one.
    apiKey: req.apiKey || "none",
    baseURL: req.model.baseUrl ?? "https://api.openai.com/v1",
    maxRetries: req.maxRetries ?? 8,
    defaultHeaders: req.model.headers,
    ...(req.fetch ? { fetch: req.fetch } : {}),
  });
}

export function isContextOverflow(e: unknown): boolean {
  if (!(e instanceof OpenAI.APIError)) return false;
  return e.code === "context_length_exceeded" || /context length|context window|too many tokens|maximum context/i.test(e.message);
}
/** A Chat Completions usage object: the standard counts, plus fields vendors add on. */
export interface ChatUsage {
  prompt_tokens?: number;
  completion_tokens?: number;
  prompt_tokens_details?: { cached_tokens?: number; cache_write_tokens?: number };
  prompt_cache_hit_tokens?: number;
  prompt_cache_miss_tokens?: number;
  /** Actual spend in USD (OpenRouter reports it, with `cost_details` alongside); unknown to the SDK's types. */
  cost?: unknown;
}
/** Streamed usage: prefer provider-reported cost, otherwise estimate. */
export function parseChatUsage(model: ModelInfo, u: ChatUsage): Usage {
  const cached = u.prompt_tokens_details?.cached_tokens ?? u.prompt_cache_hit_tokens ?? 0;
  const written = u.prompt_tokens_details?.cache_write_tokens ?? 0;
  const usage: Usage = {
    input: (u.prompt_tokens ?? ((u.prompt_cache_miss_tokens ?? 0) + cached)) - cached - written,
    output: u.completion_tokens ?? 0,
    cacheRead: cached,
    cacheWrite: written,
    cost: 0,
  };
  usage.cost = reportedCost(u) ?? computeCost(model, usage);
  return usage;
}

export const openaiChatProvider: Provider = {
  api: "openai-chat",
  async *stream(req: Request): AsyncGenerator<StreamEvent> {
    const { model } = req;
    const client = openaiClient(req);
    const params: ChatCompletionCreateParamsStreaming = {
      model: model.id,
      messages: [{ role: "system", content: req.system }, ...toChatMessages(req.messages, model, req.tools.length > 0)],
      stream: true,
      stream_options: { include_usage: true },
      ...(model.provider === "ollama" ? { max_tokens: req.maxTokens ?? model.maxOutput } : { max_completion_tokens: req.maxTokens ?? model.maxOutput }),
    };
    if (req.tools.length)
      params.tools = req.tools.map((t) => ({
        type: "function",
        function: { name: t.name, description: t.description, parameters: t.parameters },
      }));
    Object.assign(params, samplingFields(req.sampling, ["temperature", "top_p", "frequency_penalty", "presence_penalty"]));

    const routerReasoning = (params as unknown as { reasoning?: { effort?: string } }).reasoning;
    const out = newAssistant(model);
    yield { type: "start", partial: out };
    let thinking: ThinkingContent | undefined;
    let detailsBlock: ThinkingContent | undefined;
    const details: unknown[] = [];
    let text: { type: "text"; text: string } | undefined;
    const calls = new Map<number, { block: ToolCall; json: string; index: number }>();
    let ended = false;
    try {
      // "off" is sent too: servers such as vLLM think by default unless told "none".
      const stream = await withEffort(replayScope(model), (params.reasoning_effort ?? routerReasoning?.effort) !== undefined ? undefined : model.reasoning ? req.thinking : undefined, async (effort) => {
        const s = await client.chat.completions.create({ ...params, ...(effort ? openrouter(model) ? { reasoning: { effort, ...routerReasoning } } : { reasoning_effort: effort as never } : {}) }, { signal: req.signal });
        const it = s[Symbol.asyncIterator]();
        // A refused effort can arrive as the first event of an HTTP 200 stream (vLLM).
        return { it, first: await it.next() };
      });
      for (let next: IteratorResult<ChatCompletionChunk> = stream.first; !next.done; next = await stream.it.next()) {
        const chunk = next.value;
        if (chunk.usage) {
          out.usage = parseChatUsage(model, chunk.usage);
          ended = true;
        }
        const choice = chunk.choices[0];
        if (!choice) continue;
        const delta = choice.delta as typeof choice.delta & { reasoning?: string; reasoning_content?: string; reasoning_details?: unknown[] };
        if (openrouter(model) && delta.reasoning_details?.length) {
          if (!detailsBlock) out.content.push(detailsBlock = { type: "thinking", thinking: "", redacted: true });
          details.push(...delta.reasoning_details);
          detailsBlock.signature = JSON.stringify(details); // Consecutive chunks stay in their original order and shape.
        }
        const r = delta.reasoning_content ?? delta.reasoning;
        if (r) {
          if (!thinking) {
            thinking = { type: "thinking", thinking: "", reasoningField: delta.reasoning_content != null ? "reasoning_content" : "reasoning" };
            out.content.push(thinking);
          }
          thinking.thinking += r;
          yield { type: "thinking_delta", index: out.content.indexOf(thinking), delta: r, partial: out };
        }
        if (delta.refusal) out.stopReason = "refusal";
        const prose = delta.content ?? delta.refusal;
        if (prose) {
          if (!text) {
            text = { type: "text", text: "" };
            out.content.push(text);
          }
          text.text += prose;
          yield { type: "text_delta", index: out.content.indexOf(text), delta: prose, partial: out };
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
        if (choice.finish_reason) ended = true;
        if (choice.finish_reason === "length") out.stopReason = "max_tokens";
        else if (choice.finish_reason === "content_filter") out.stopReason = "refusal";
      }
    } catch (e) {
      if (e instanceof OpenAI.APIUserAbortError || req.signal?.aborted) out.stopReason = "aborted";
      else if (isContextOverflow(e)) throw new ContextOverflowError((e as Error).message);
      else throw e;
    }
    markIfCut(out, ended); // neither a finish reason nor the usage chunk
    for (const c of calls.values()) Object.assign(c.block, parseToolInput(c.json));
    if (out.stopReason === "stop" && calls.size) out.stopReason = "tool_use";
    yield { type: "done", message: out };
  },
};

function toChatMessages(messages: Message[], model: ModelInfo, hasTools: boolean): ChatCompletionMessageParam[] {
  const modelId = model.id;
  const id = modelId.split("/").pop();
  // DeepSeek needs all reasoning with tools; MiMo needs it on tool-call messages.
  const deepseekModel = /^deepseek-(flash|v4(?:\.1)?-flash)$/.test(id ?? "");
  const deepseek = hasTools && deepseekModel;
  const mimo = id === "mimo-v2.6-flash";
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
      const replay = canReplay(m, model) ? m.content.filter(c => c.type === "thinking") : [];
      const reasoningText = replay.filter(c => !c.redacted).map(c => c.thinking).join("");
      const reasoningDetails = openrouter(model) ? replay.flatMap(c => c.redacted && c.signature ? JSON.parse(c.signature) : []) : [];
      if (!text && !tools.length && !reasoningDetails.length) continue;
      out.push({
        role: "assistant",
        content: text || null,
        ...(openrouter(model)
          ? reasoningDetails.length ? { reasoning_details: reasoningDetails } : reasoningText ? { reasoning: reasoningText } : {}
          : deepseek || (mimo && tools.length)
          ? { reasoning_content: reasoningText }
          : !deepseekModel && !mimo && reasoningText && replay[0]?.reasoningField ? { [replay[0].reasoningField]: reasoningText }
          : {}),
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
      // Tool messages are text-only; replay images in a follow-up user message.
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
