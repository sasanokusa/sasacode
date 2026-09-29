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
  markIfCut,
  type Message,
  newAssistant,
  parseToolInput,
  type Provider,
  samplingFields,
  type Request,
  type StopReason,
  type StreamEvent,
  type ThinkingLevel,
  type UserContent,
} from "./types.ts";

const OFFICIAL_BASE = "https://api.anthropic.com";

/** Thinking cannot be turned off on these: effort is the only control. */
const ALWAYS_THINKS = /^claude-(opus-5-5|fable|mythos)/;
/**
 * These bind a thinking block to the system prompt, tools and messages before it, and reject it
 * when any of those changed (preserved thinking). sasacode changes them mid-conversation (plugins
 * append to the system prompt, MCP tools arrive late, compaction keeps a recent tail).
 */
const BINDS_THINKING = /^claude-(opus-5-5|fable-5-1|mythos-5-1)/;
const BINDING_BETA = "thinking-binding-controls-2026-08-01";

/** What a model turned out not to accept, so later requests go straight to what works. */
const learned = { noDisable: new Set<string>(), noXhigh: new Set<string>(), noBinding: new Set<string>() };

/** The thinking fields for a level. Only the official API gets fields beyond what was sent before. */
export function thinkingFields(id: string, level: ThinkingLevel | undefined, official: boolean): Pick<MessageCreateParamsStreaming, "thinking" | "output_config"> {
  const adaptive = (effort?: string) => ({
    thinking: { type: "adaptive", display: "summarized" } as MessageCreateParamsStreaming["thinking"],
    ...(effort ? { output_config: { effort } as MessageCreateParamsStreaming["output_config"] } : {}),
  });
  if (!level) return BINDS_THINKING.test(id) && official ? adaptive() : {};
  if (level === "off") {
    // Leaving thinking out runs adaptive thinking on Claude 5 models: off has to be said.
    if (ALWAYS_THINKS.test(id) || learned.noDisable.has(id)) return adaptive("low");
    return official && id.startsWith("claude-") ? { thinking: { type: "disabled" } } : {};
  }
  return adaptive(level === "xhigh" && learned.noXhigh.has(id) ? "high" : level);
}

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
      ...(req.fetch ? { fetch: req.fetch } : {}),
    });
    // Proxies may reject eager_input_streaming, so only send it to the official API.
    const official = !model.baseUrl || model.baseUrl.startsWith(OFFICIAL_BASE);
    const tools: Tool[] = req.tools.map((t) => ({
      name: t.name,
      description: t.description,
      input_schema: t.parameters as Tool["input_schema"],
      ...(official ? { eager_input_streaming: true } : {}),
    }));
    let stripThinking = false;
    const build = () => {
      const params: MessageCreateParamsStreaming = {
        model: model.id,
        max_tokens: req.maxTokens ?? model.maxOutput,
        system: req.system,
        messages: toAnthropicMessages(req.messages, model.id, stripThinking),
        tools,
        stream: true,
        cache_control: { type: "ephemeral" },
        ...(model.reasoning ? thinkingFields(model.id, req.thinking, official) : {}),
      };
      // Newer Claude models reject sampling parameters; only what a plugin or config set explicitly is sent.
      Object.assign(params, samplingFields(req.sampling, ["temperature", "top_p"]));
      let beta: string | undefined;
      if (official && params.thinking?.type === "adaptive" && BINDS_THINKING.test(model.id) && !learned.noBinding.has(model.id)) {
        // A block whose context changed is dropped instead of failing the whole request.
        (params.thinking as unknown as Record<string, unknown>).block_binding = { prefix_mismatch_behavior: "drop_block" };
        beta = [model.headers?.["anthropic-beta"], BINDING_BETA].filter(Boolean).join(",");
      }
      return { params, headers: beta ? { "anthropic-beta": beta } : undefined };
    };

    /**
     * Learn from a 400 about the request's own shape and say whether sending it again can help.
     * Nothing has been streamed yet when this is called.
     */
    function adjust(message: string, params: MessageCreateParamsStreaming): boolean {
      if (params.thinking?.type === "disabled" && /thinking\.type\.disabled|disabled.*not supported/i.test(message)) return !!learned.noDisable.add(params.model);
      if (params.output_config?.effort === "xhigh" && /effort/i.test(message)) return !!learned.noXhigh.add(params.model);
      if (/block_binding/.test(message) && !learned.noBinding.has(params.model)) return !!learned.noBinding.add(params.model);
      // Preserved thinking where the binding controls are not offered: send the history without thinking once.
      if (/bound to a different conversation/i.test(message) && !stripThinking) return (stripThinking = true);
      return false;
    }

    const out = newAssistant(model);
    const jsonBuf = new Map<number, string>();
    let ended = false;
    yield { type: "start", partial: out };
    for (let attempt = 0; ; attempt++) {
      const { params, headers } = build();
      let stream;
      try {
        stream = client.messages.stream(params, { signal: req.signal, headers });
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
              if (ev.delta.stop_reason) ended = true;
              break;
            case "message_stop":
              ended = true;
              break;
          }
        }
      } catch (e) {
        if (e instanceof Anthropic.APIUserAbortError || req.signal?.aborted) {
          out.stopReason = "aborted";
        } else if (e instanceof Anthropic.BadRequestError && /prompt is too long|context window/i.test(e.message)) {
          throw new ContextOverflowError(e.message);
        } else if (attempt < 3 && !out.content.length && e instanceof Anthropic.BadRequestError && adjust(e.message, params)) {
          continue;
        } else throw e;
      }
      break;
    }
    out.content = out.content.filter(Boolean);
    markIfCut(out, ended); // no stop reason and no message_stop
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

/** `noThinking` leaves out every thinking block (a model that rejected them as bound elsewhere). */
export function toAnthropicMessages(messages: Message[], modelId: string, noThinking = false): MessageParam[] {
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
          if (!sameModel || !c.signature || noThinking) continue;
          if (c.redacted) blocks.push({ type: "redacted_thinking", data: c.signature });
          else blocks.push({ type: "thinking", thinking: c.thinking, signature: c.signature });
        } else blocks.push({ type: "tool_use", id: c.id, name: c.name, input: c.input });
      }
      if (blocks.length) push("assistant", blocks);
    }
  }
  return out;
}
