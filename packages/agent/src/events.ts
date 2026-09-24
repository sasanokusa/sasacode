import type { AssistantMessage, Message, StreamEvent, ToolCall } from "@sasacode/ai";
import type { StopCause, ToolResult } from "@sasacode/plugin-api";

export type { StopCause };

export type AgentEvent =
  | { type: "agent_start" }
  | { type: "turn_start"; turn: number }
  | { type: "message_start"; message: AssistantMessage }
  | { type: "message_update"; message: AssistantMessage; event: StreamEvent }
  | { type: "message_end"; message: Message }
  | { type: "tool_start"; call: ToolCall; summary: string }
  | { type: "tool_update"; call: ToolCall; text: string }
  | { type: "tool_end"; call: ToolCall; result: ToolResult }
  | { type: "turn_end"; turn: number }
  | { type: "context_limit"; tokens: number; contextWindow: number }
  | { type: "error"; error: string }
  | { type: "plugin_error"; plugin: string; hook: string; error: string }
  | { type: "messages_replaced" }
  /** A response a plugin asked to regenerate (assistant_message → retry); UIs should drop it. */
  | { type: "message_discarded"; message: AssistantMessage }
  | { type: "tool_repaired"; call: ToolCall; from: { name: string; input: unknown; rawInput?: string }; note: string }
  | { type: "agent_end"; cause: StopCause; stopped?: { plugin: string; reason: string } };

type Listener<T> = (event: T) => void;

export class EventBus<T extends { type: string }> {
  private listeners = new Set<Listener<T>>();

  on(listener: Listener<T>): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  emit(event: T): void {
    for (const l of this.listeners) {
      try {
        l(event);
      } catch (e) {
        // A broken listener must not stop the loop.
        console.error(`event listener failed on ${event.type}:`, e);
      }
    }
  }
}
