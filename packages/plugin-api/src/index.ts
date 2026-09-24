// The only surface plugins depend on. Built-in tools, commands, MCP and Skills use it too (P2).
// Versioned with semver: breaking changes only in a new major (see PLUGIN_API_VERSION).
import type {
  AssistantMessage,
  JSONSchema,
  Message,
  ModelInfo,
  Provider,
  ProviderConfig,
  ToolCall,
  UserContent,
} from "@sasacode/ai";

export type { AssistantMessage, JSONSchema, Message, ModelInfo, Provider, ProviderConfig, ToolCall, UserContent } from "@sasacode/ai";

export const PLUGIN_API_VERSION = "1.0.0";

// ── tools ────────────────────────────────────────────────────────────

export interface ToolContext {
  cwd: string;
  signal: AbortSignal;
  /** Stream partial output (e.g. bash stdout) to the UI while the tool runs. */
  onUpdate?(text: string): void;
}

export interface ToolResult {
  content: UserContent[];
  isError?: boolean;
  /** Structured data for renderers (diffs, exit codes). Not sent to the model. */
  details?: Record<string, unknown>;
}

/**
 * What a tool does, for the permission system:
 * read = no side effects, edit = changes files under `paths`, exec = arbitrary effects.
 */
export type ToolKind = "read" | "edit" | "exec" | "other";

export interface ToolDefinition<Args = Record<string, any>> {
  name: string;
  description: string;
  parameters: JSONSchema;
  kind: ToolKind;
  /** Safe to run alongside other concurrent tools in the same turn (FR-L04). */
  concurrent?: boolean;
  /** Always send the full definition, even when tool search defers the rest. */
  alwaysLoad?: boolean;
  /** Files the call touches, for path-based permission rules. */
  paths?(args: Args, cwd: string): string[];
  /** String matched by permission patterns such as `bash(git *)`. */
  matchTarget?(args: Args): string;
  /** One-line label for UIs, e.g. the command or path. */
  summary?(args: Args): string;
  execute(args: Args, ctx: ToolContext): Promise<ToolResult>;
}

// ── commands ─────────────────────────────────────────────────────────

export interface CommandContext {
  args: string;
}

export interface CommandDefinition {
  name: string;
  description: string;
  argumentHint?: string;
  run(ctx: CommandContext): Promise<void> | void;
}

// ── hooks (requirements 5.3) ─────────────────────────────────────────

export type Decision = "allow" | "ask" | "deny";
export type StopCause = "done" | "aborted" | "error" | "refusal" | "context_limit" | "max_turns";

/** Each hook: the payload handlers receive and what they may return. Handlers run in registration order. */
export interface HookMap {
  session_start: { event: { sessionId?: string; resumed: boolean }; result: void };
  session_end: { event: { sessionId?: string }; result: void };
  /** Transform input, or consume it (`handled`) so it never reaches the model. */
  user_prompt: { event: { content: UserContent[] }; result: { content?: UserContent[]; handled?: boolean } };
  /** Append to (or rewrite) the system prompt for this request. */
  system_prompt: { event: { prompt: string }; result: { prompt?: string } };
  /** Change the messages sent in this request (not what is stored). */
  before_request: { event: { messages: Message[]; model: ModelInfo }; result: { messages?: Message[] } };
  /** Rewrite arguments or decide permission before the core policy runs. */
  tool_call: {
    event: { call: ToolCall; tool: ToolDefinition<any>; args: Record<string, unknown> };
    result: { args?: Record<string, unknown>; decision?: Decision; reason?: string };
  };
  tool_result: { event: { call: ToolCall; result: ToolResult }; result: { result?: ToolResult } };
  /** Return `inject` to add a user message and keep the loop going. */
  turn_end: { event: { turn: number; message: AssistantMessage }; result: { inject?: string } };
  agent_end: { event: { cause: StopCause }; result: { inject?: string } };
  /** Free up context (e.g. session.replaceMessages) and return `retry` to continue. */
  context_limit: { event: { tokens: number; contextWindow: number }; result: { retry?: boolean } };
}

export type HookName = keyof HookMap;
export type HookHandler<K extends HookName> = (
  event: HookMap[K]["event"],
) => HookMap[K]["result"] | void | Promise<HookMap[K]["result"] | void>;

// ── ui, session, agent ───────────────────────────────────────────────

export interface SelectOption {
  value: string;
  label: string;
  description?: string;
}

export interface RenderOptions {
  expanded: boolean;
  width: number;
}

/** Returns body lines (ANSI allowed, each at most `width` wide) or undefined for the default rendering. */
export type ToolRenderer = (call: ToolCall, result: ToolResult | undefined, opts: RenderOptions) => string[] | undefined;

export interface PluginUI {
  /** False in headless runs: confirm resolves false and select undefined. */
  readonly interactive: boolean;
  notify(message: string, level?: "info" | "warning" | "error"): void;
  confirm(title: string, message?: string): Promise<boolean>;
  select(title: string, options: SelectOption[]): Promise<string | undefined>;
  /** Show (or clear with undefined) a status line item. */
  setStatus(key: string, text: string | undefined): void;
  registerToolRenderer(toolName: string, renderer: ToolRenderer): void;
}

export interface PluginSession {
  readonly id: string | undefined;
  /** Persist plugin state in the session log (FR-S03). */
  append(kind: string, data: unknown): void;
  /** Entries this plugin appended under `kind`, oldest first, including restored ones. */
  entries(kind: string): unknown[];
  messages(): readonly Message[];
  /** Replace the conversation (e.g. after compaction). Persisted. */
  replaceMessages(messages: Message[]): void;
  /** Add a user message: "next" delivers it with the next turn, "now" also starts a run if idle. */
  inject(content: string | UserContent[], when?: "next" | "now"): void;
}

export interface SubagentOptions {
  prompt: string;
  systemPrompt?: string;
  /** Tool names to offer; defaults to every tool except those in `excludeTools`. */
  tools?: string[];
  excludeTools?: string[];
  /** "<provider>/<model>"; defaults to the current model. */
  model?: string;
  signal?: AbortSignal;
  onProgress?(text: string): void;
}

export interface CompleteOptions {
  system: string;
  messages: Message[];
  model?: string;
  maxTokens?: number;
  signal?: AbortSignal;
}

export interface PluginAgent {
  model(): ModelInfo;
  tools(): ToolDefinition<any>[];
  /** Run an independent agent loop with its own history (subagents). */
  run(opts: SubagentOptions): Promise<{ text: string; messages: Message[]; cause: StopCause }>;
  /** One model call without tools; returns the text. */
  complete(opts: CompleteOptions): Promise<string>;
}

export interface PermissionRules {
  allow?: string[];
  ask?: string[];
  deny?: string[];
}

export interface PluginAPI {
  readonly version: string;
  /** This plugin's name. */
  readonly name: string;
  readonly cwd: string;
  /** This plugin's settings from config (`plugins.settings.<name>`). */
  readonly settings: Record<string, unknown>;
  /** Adding a tool with an existing name replaces it (P3). */
  registerTool(tool: ToolDefinition<any>): void;
  registerCommand(command: CommandDefinition): void;
  /** Add a provider (`name`) and, optionally, the implementation for its api (FR-P06). */
  registerProvider(name: string, config: ProviderConfig, implementation?: Provider): void;
  on<K extends HookName>(event: K, handler: HookHandler<K>): void;
  permissions: { addRules(rules: PermissionRules): void };
  ui: PluginUI;
  session: PluginSession;
  agent: PluginAgent;
  log(...args: unknown[]): void;
}

export type Plugin = (api: PluginAPI) => void | Promise<void>;

export function text(t: string): UserContent {
  return { type: "text", text: t };
}

export function errorResult(message: string): ToolResult {
  return { content: [text(message)], isError: true };
}

/** `^1.2.0`-style compatibility: same major, and not newer than what the host provides. */
export function isCompatible(required: string, provided = PLUGIN_API_VERSION): boolean {
  const req = required.replace(/^[\^~>=\s]+/, "").split(".").map(Number);
  const have = provided.split(".").map(Number);
  if (req[0] !== have[0]) return false;
  for (let i = 1; i < 3; i++) {
    if ((have[i] ?? 0) > (req[i] ?? 0)) return true;
    if ((have[i] ?? 0) < (req[i] ?? 0)) return false;
  }
  return true;
}
