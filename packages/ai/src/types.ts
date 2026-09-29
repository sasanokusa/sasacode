// Provider-neutral message format. Sessions store these, so a conversation can
// move between providers mid-session (FR-P04).

export interface TextContent {
  type: "text";
  text: string;
}

export interface ImageContent {
  type: "image";
  mediaType: string;
  data: string; // base64
}

export interface ThinkingContent {
  type: "thinking";
  thinking: string;
  /** Opaque provider state needed to replay this block (Anthropic signature, Responses reasoning item). */
  signature?: string;
  redacted?: boolean;
}

export interface ToolCall {
  type: "tool_call";
  id: string;
  name: string;
  input: Record<string, unknown>;
  /** Set when the streamed arguments could not be parsed as JSON. */
  rawInput?: string;
}

export type UserContent = TextContent | ImageContent;
export type AssistantContent = TextContent | ThinkingContent | ToolCall;

export interface UserMessage {
  role: "user";
  content: UserContent[];
  timestamp: number;
}

/** `stopped`: a plugin ended generation early (e.g. repetition). */
export type StopReason = "stop" | "tool_use" | "max_tokens" | "refusal" | "aborted" | "stopped" | "error";

/**
 * errorMessage of a reply whose stream ended without the provider saying it was done (no finish
 * reason, no usage): the connection dropped mid-reply. What arrived is kept, with stopReason "error".
 */
export const INCOMPLETE_STREAM = "the stream ended before the reply was complete";

export interface Usage {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  cost: number; // USD: what the provider reported, else the price-table estimate, 0 when there is neither
}

export interface AssistantMessage {
  role: "assistant";
  content: AssistantContent[];
  api: Api;
  provider: string;
  model: string;
  usage: Usage;
  stopReason: StopReason;
  errorMessage?: string;
  timestamp: number;
}

export interface ToolResultMessage {
  role: "tool";
  toolCallId: string;
  toolName: string;
  content: UserContent[];
  isError: boolean;
  timestamp: number;
}

export type Message = UserMessage | AssistantMessage | ToolResultMessage;

export type JSONSchema = Record<string, unknown>;

export interface ToolSpec {
  name: string;
  description: string;
  parameters: JSONSchema;
}

export type Api = "anthropic" | "openai-chat" | "openai-responses" | (string & {});

export type ThinkingLevel = "off" | "low" | "medium" | "high" | "xhigh" | "max";

export interface ModelInfo {
  id: string;
  provider: string;
  api: Api;
  baseUrl?: string;
  contextWindow: number;
  maxOutput: number;
  /** USD per 1M tokens. */
  price?: { input: number; output: number; cacheRead?: number; cacheWrite?: number };
  reasoning?: boolean;
  images?: boolean;
  headers?: Record<string, string>;
}

/** Sampling knobs. Each adapter sends what its API understands; `extraBody` is merged into the request as-is. */
export interface SamplingOptions {
  temperature?: number;
  topP?: number;
  frequencyPenalty?: number;
  presencePenalty?: number;
  /** Provider-specific fields, e.g. vLLM's `repetition_penalty`. */
  extraBody?: Record<string, unknown>;
}

export interface Request {
  model: ModelInfo;
  apiKey?: string;
  system: string;
  messages: Message[];
  tools: ToolSpec[];
  thinking?: ThinkingLevel;
  maxTokens?: number;
  maxRetries?: number;
  sampling?: SamplingOptions;
  signal?: AbortSignal;
  /**
   * Replaces the HTTP client's fetch, e.g. for a provider that wraps another and must adjust the
   * request on the wire (the openai-codex plugin drops fields its backend rejects).
   */
  fetch?: typeof fetch;
}

export type StreamEvent =
  | { type: "start"; partial: AssistantMessage }
  | { type: "text_delta"; index: number; delta: string; partial: AssistantMessage }
  | { type: "thinking_delta"; index: number; delta: string; partial: AssistantMessage }
  | { type: "toolcall_start"; index: number; id: string; name: string; partial: AssistantMessage }
  | { type: "toolcall_delta"; index: number; delta: string; partial: AssistantMessage }
  | { type: "done"; message: AssistantMessage };

/** A provider turns a request into a stream of events ending with `done`. Errors are thrown. */
export interface Provider {
  api: Api;
  stream(req: Request): AsyncGenerator<StreamEvent>;
  /**
   * The models this provider offers, for /model. Without it, the provider's Models API is asked
   * (GET /models for the OpenAI and Anthropic formats).
   */
  listModels?(config: ProviderConfig, apiKey: string | undefined, signal?: AbortSignal): Promise<{ id: string; contextWindow?: number; maxOutput?: number; price?: ModelInfo["price"] }[]>;
}

export class ContextOverflowError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ContextOverflowError";
  }
}

/** Common sampling fields in the snake_case most HTTP APIs use, plus extraBody. Unset fields are omitted. */
export function samplingFields(s: SamplingOptions | undefined, keys: ("temperature" | "top_p" | "frequency_penalty" | "presence_penalty")[]) {
  if (!s) return {};
  const all: Record<string, number | undefined> = {
    temperature: s.temperature,
    top_p: s.topP,
    frequency_penalty: s.frequencyPenalty,
    presence_penalty: s.presencePenalty,
  };
  const out: Record<string, unknown> = {};
  for (const k of keys) if (all[k] !== undefined) out[k] = all[k];
  return { ...out, ...s.extraBody };
}

export function emptyUsage(): Usage {
  return { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, cost: 0 };
}

export function computeCost(model: ModelInfo, u: Usage): number {
  const p = model.price;
  if (!p) return 0;
  return (
    (u.input * p.input +
      u.output * p.output +
      u.cacheRead * (p.cacheRead ?? p.input * 0.1) +
      u.cacheWrite * (p.cacheWrite ?? p.input * 1.25)) /
    1e6
  );
}

/**
 * Cost in USD the provider reported alongside its usage (OpenRouter's `usage.cost`). Zero, negative
 * or non-numeric means "not reported", so callers fall back to the price-table estimate.
 */
export function reportedCost(usage: object | undefined): number | undefined {
  const c = (usage as { cost?: unknown } | undefined)?.cost;
  return typeof c === "number" && Number.isFinite(c) && c > 0 ? c : undefined;
}

export function newAssistant(model: ModelInfo): AssistantMessage {
  return {
    role: "assistant",
    content: [],
    api: model.api,
    provider: model.provider,
    model: model.id,
    usage: emptyUsage(),
    stopReason: "stop",
    timestamp: Date.now(),
  };
}

export function textOf(content: readonly { type: string; text?: string }[]): string {
  return content
    .filter((c) => c.type === "text")
    .map((c) => c.text)
    .join("");
}

/** Parse streamed tool-call JSON. Returns rawInput when it is not valid JSON so the loop can report it. */
export function parseToolInput(json: string): { input: Record<string, unknown>; rawInput?: string } {
  if (!json.trim()) return { input: {} };
  try {
    const v = JSON.parse(json);
    if (v && typeof v === "object" && !Array.isArray(v)) return { input: v };
  } catch {}
  return { input: {}, rawInput: json };
}

export interface ProviderConfig {
  api: Api;
  baseUrl?: string;
  /** Environment variable holding the API key. */
  apiKeyEnv?: string;
  headers?: Record<string, string>;
  /** Shown in the generated ~/.sasacode/.env. */
  label?: string;
  /** Model used when nothing is configured and this provider's key is set. */
  defaultModel?: string;
  /** The server is Ollama: model details (context length) come from its native /api/show. */
  ollama?: boolean;
  /** The server is llama.cpp's llama-server: the context size comes from its /props. */
  llamacpp?: boolean;
}
