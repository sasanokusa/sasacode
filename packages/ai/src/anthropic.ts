import Anthropic from "@anthropic-ai/sdk";
import type {
  ContentBlockParam,
  MessageCreateParamsStreaming,
  MessageParam,
  Tool,
} from "@anthropic-ai/sdk/resources/messages/messages";
import {
  type AssistantMessage,
  ContextOverflowError,
  computeCost,
  type Message,
  newAssistant,
  parseToolInput,
  type Provider,
  type Request,
  type StopReason,
  type StreamEvent,
  type UserContent,
} from "./types.ts";

const OFFICIAL_BASE = "https://api.anthropic.com";

export const anthropicProvider: Provider = {
  api: "anthropic",
  async *stream(req: Request): AsyncGenerator<StreamEvent> {
    const { model } = req;
    const client = new Anthropic({
      // Without an explicit key the SDK resolves env vars and `ant auth login` profiles itself.
      ...(req.apiKey ? { apiKey: req.apiKey } : {}),
      baseURL: model.baseUrl,
      maxRetries: req.maxRetries ?? 8,
      defaultHeaders: model.headers,
    });
    // Proxies may reject eager_input_streaming, so only send it to the official API.
    const official = !model.baseUrl || model.baseUrl.startsWith(OFFICIAL_BASE);
    const tools: Tool[] = req.tools.map((t) => ({
      name: t.name,
      description: t.description,
      input_schema: t.parameters as Tool["input_schema"],
      ...(official ? { eager_input_streaming: true } : {}),
    }));
    const params: MessageCreateParamsStreaming = {
      model: model.id,
      max_tokens: req.maxTokens ?? model.maxOutput,
      system: req.system,
      messages: toAnthropicMessages(req.messages, model.id),
      tools,
      stream: true,
      cache_control: { type: "ephemeral" },
    };
    if (model.reasoning && req.thinking && req.thinking !== "off") {
      params.thinking = { type: "adaptive", display: "summarized" };
      params.output_config = { effort: req.thinking };
    }

    const out = newAssistant(model);
    const jsonBuf = new Map<number, string>();
    yield { type: "start", partial: out };
    let stream;
    try {
      stream = client.messages.stream(params, { signal: req.signal });
      for await (const ev of stream) {
        switch (ev.type) {
          case "message_start":
            readUsage(out, ev.message.usage);
            break;
          case "content_block_start": {
            const b = ev.content_block;
            if (b.type === "text") out.content[ev.index] = { type: "text", text: "" };
            else if (b.type === "thinking") out.content[ev.index] = { type: "thinking", thinking: "", signature: "" };
            else if (b.type === "redacted_thinking")
              out.content[ev.index] = { type: "thinking", thinking: "", signature: b.data, redacted: true };
            else if (b.type === "tool_use") {
              out.content[ev.index] = { type: "tool_call", id: b.id, name: b.name, input: {} };
              jsonBuf.set(ev.index, "");
              yield { type: "toolcall_start", index: ev.index, id: b.id, name: b.name, partial: out };
            }
            break;
          }
          case "content_block_delta": {
            const b = out.content[ev.index];
            const d = ev.delta;
            if (d.type === "text_delta" && b?.type === "text") {
              b.text += d.text;
              yield { type: "text_delta", index: ev.index, delta: d.text, partial: out };
            } else if (d.type === "thinking_delta" && b?.type === "thinking") {
              b.thinking += d.thinking;
              yield { type: "thinking_delta", index: ev.index, delta: d.thinking, partial: out };
            } else if (d.type === "signature_delta" && b?.type === "thinking") {
              b.signature = (b.signature ?? "") + d.signature;
            } else if (d.type === "input_json_delta" && b?.type === "tool_call") {
              jsonBuf.set(ev.index, (jsonBuf.get(ev.index) ?? "") + d.partial_json);
              yield { type: "toolcall_delta", index: ev.index, delta: d.partial_json, partial: out };
            }
            break;
          }
          case "content_block_stop": {
            const b = out.content[ev.index];
            if (b?.type === "tool_call") Object.assign(b, parseToolInput(jsonBuf.get(ev.index) ?? ""));
            break;
          }
          case "message_delta":
            readUsage(out, ev.usage);
            if (ev.delta.stop_reason === "model_context_window_exceeded")
              throw new ContextOverflowError("model context window exceeded");
            out.stopReason = mapStop(ev.delta.stop_reason);
            break;
        }
      }
    } catch (e) {
      if (e instanceof Anthropic.APIUserAbortError || req.signal?.aborted) {
        out.stopReason = "aborted";
      } else if (e instanceof Anthropic.BadRequestError && /prompt is too long|context window/i.test(e.message)) {
        throw new ContextOverflowError(e.message);
      } else throw e;
    }
    out.content = out.content.filter(Boolean);
    out.usage.cost = computeCost(model, out.usage);
    yield { type: "done", message: out };
  },
};

function readUsage(out: AssistantMessage, u: Partial<Anthropic.Usage> | Anthropic.MessageDeltaUsage) {
  if (u.input_tokens != null) out.usage.input = u.input_tokens;
  if (u.output_tokens != null) out.usage.output = u.output_tokens;
  if (u.cache_read_input_tokens != null) out.usage.cacheRead = u.cache_read_input_tokens;
  if (u.cache_creation_input_tokens != null) out.usage.cacheWrite = u.cache_creation_input_tokens;
}

function mapStop(r: string | null): StopReason {
  if (r === "tool_use") return "tool_use";
  if (r === "max_tokens") return "max_tokens";
  if (r === "refusal") return "refusal";
  return "stop";
}

function userBlocks(content: UserContent[]): ContentBlockParam[] {
  return content.map((c) =>
    c.type === "text"
      ? { type: "text", text: c.text }
      : { type: "image", source: { type: "base64", media_type: c.mediaType as "image/png", data: c.data } },
  );
}

export function toAnthropicMessages(messages: Message[], modelId: string): MessageParam[] {
  const out: MessageParam[] = [];
  const push = (role: "user" | "assistant", blocks: ContentBlockParam[]) => {
    const last = out[out.length - 1];
    // Consecutive same-role messages (tool results + a queued user message) must be merged.
    if (last && last.role === role && Array.isArray(last.content)) last.content.push(...blocks);
    else out.push({ role, content: blocks });
  };
  for (const m of messages) {
    if (m.role === "user") push("user", userBlocks(m.content));
    else if (m.role === "tool")
      push("user", [
        { type: "tool_result", tool_use_id: m.toolCallId, content: userBlocks(m.content) as never, is_error: m.isError },
      ]);
    else {
      const sameModel = m.api === "anthropic" && m.model === modelId;
      const blocks: ContentBlockParam[] = [];
      for (const c of m.content) {
        if (c.type === "text") {
          if (c.text) blocks.push({ type: "text", text: c.text });
        } else if (c.type === "thinking") {
          // Thinking blocks are bound to the model that produced them; drop them elsewhere.
          if (!sameModel || !c.signature) continue;
          if (c.redacted) blocks.push({ type: "redacted_thinking", data: c.signature });
          else blocks.push({ type: "thinking", thinking: c.thinking, signature: c.signature });
        } else blocks.push({ type: "tool_use", id: c.id, name: c.name, input: c.input });
      }
      if (blocks.length) push("assistant", blocks);
    }
  }
  return out;
}
