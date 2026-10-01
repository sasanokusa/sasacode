// The only surface plugins depend on. Built-in tools, commands, MCP and Skills use it too (P2).
// Versioned with semver: breaking changes only in a new major (see PLUGIN_API_VERSION).
import { mkdirSync, readdirSync, statSync, unlinkSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import type {
  AssistantMessage,
  JSONSchema,
  Message,
  ModelInfo,
  Provider,
  ProviderConfig,
  SamplingOptions,
  StopReason,
  ToolCall,
  UserContent,
} from "@sasacode/ai";

export type {
  AssistantMessage,
  JSONSchema,
  Message,
  ModelInfo,
  Provider,
  ProviderConfig,
  SamplingOptions,
  StopReason,
  ToolCall,
  UserContent,
} from "@sasacode/ai";

export const PLUGIN_API_VERSION = "1.11.0";

// ── tools ────────────────────────────────────────────────────────────

export interface ToolContext {
  cwd: string;
  signal: AbortSignal;
  /** Stream partial output (e.g. bash stdout) to the UI while the tool runs. */
  onUpdate?(text: string): void;
  /** Call a tool with inherited context and its own validation/permission check (since 1.9.0); see docs/plugins.md. */
  callTool?(name: string, args: Record<string, unknown>): Promise<ToolResult>;
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
  /** Also apply the rules for this tool name (since 1.6.0); see docs/plugins.md for precedence. */
  permissionsAs?: string;
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
  /** Candidates for the argument typed so far, offered by tab completion. (since 1.3.0) */
  complete?(prefix: string): Promise<SelectOption[]> | SelectOption[];
}

// ── hooks (requirements 5.3) ─────────────────────────────────────────

export type Decision = "allow" | "ask" | "deny";
/** auto: allow everything; agent: the model judges; ask: ask for every call; edits: reads and edits in the working directory run. (since 1.7.0) */
export type PermissionMode = "auto" | "agent" | "ask" | "edits";
/**
 * Where a verdict came from: "rule" the user's (or a plugin's) allow/ask/deny rule, "soft" a
 * softDeny rule, "mode" the permission mode's default, "plugin" a tool_call hook's decision. (since 1.7.0)
 */
export type VerdictSource = "rule" | "soft" | "mode" | "plugin";
/** `stopped`: a plugin ended the run from turn_end (which one and why is in agent_end's `stopped`). (since 1.4.0) */
export type StopCause = "done" | "aborted" | "error" | "refusal" | "context_limit" | "max_turns" | "stopped";

export type DeltaKind = "text" | "thinking" | "toolcall";

/** Hooks run in registration order; lifecycle and contracts are documented in docs/plugins.md. */
export interface HookMap {
  session_start: { event: { sessionId?: string; resumed: boolean }; result: void };
  /** Session ends for "switch" or "exit"; undefined from older hosts means "exit" (since 1.8.0). */
  session_end: { event: { sessionId?: string; reason?: "switch" | "exit" }; result: void };
  /** Transform input, or consume it (`handled`) so it never reaches the model. */
  user_prompt: { event: { content: UserContent[] }; result: { content?: UserContent[]; handled?: boolean } };
  /** Append to (or rewrite) the system prompt for this request. */
  system_prompt: { event: { prompt: string }; result: { prompt?: string } };
  /** Change messages, sampling, or compose the request fetch transport (since 1.11.0; see docs/plugins.md). */
  before_request: {
    event: { messages: Message[]; model: ModelInfo; sampling: SamplingOptions; fetch?: typeof fetch };
    result: { messages?: Message[]; sampling?: SamplingOptions; fetch?: typeof fetch };
  };
  /** Synchronous, cheap streaming handler; accumulated text and early stop (since 1.2.0). */
  stream_delta: {
    event: { kind: DeltaKind; index: number; delta: string; text: string; message: Readonly<AssistantMessage> };
    result: { stop?: string };
  };
  /** Rewrite, retry, or inject after the finished response, before storage (since 1.2.0). */
  assistant_message: {
    event: { message: AssistantMessage; stopped?: { plugin: string; reason: string } };
    result: { message?: AssistantMessage; retry?: boolean; inject?: string };
  };
  /** Repair model-produced calls before validation; excludes truncated calls (since 1.2.0). */
  tool_call_raw: {
    event: {
      call: Readonly<ToolCall>;
      name: string;
      input: Record<string, unknown>;
      rawInput?: string;
      tools: ToolDefinition<any>[];
      stopReason: StopReason;
    };
    result: { name?: string; input?: Record<string, unknown>; note?: string };
  };
  /** Rewrite arguments or decide permission before the core policy runs. */
  tool_call: {
    event: { call: ToolCall; tool: ToolDefinition<any>; args: Record<string, unknown> };
    result: { args?: Record<string, unknown>; decision?: Decision; reason?: string };
  };
  /** Refine permission within `lowest`; rules and hook decisions only tighten (since 1.7.0). */
  permission: {
    event: {
      call: ToolCall;
      tool: ToolDefinition<any>;
      args: Record<string, unknown>;
      cwd: string;
      mode: PermissionMode;
      verdict: { decision: Decision; reason: string; source: VerdictSource };
      lowest: Decision;
    };
    result: { decision?: Decision; reason?: string };
  };
  /** Results include calls that never ran (`ran: false` since 1.4.0); see docs/plugins.md. */
  tool_result: { event: { call: ToolCall; result: ToolResult; ran: boolean }; result: { result?: ToolResult } };
  /** Continue via inject or stop the run with a reason (since 1.4.0; see docs/plugins.md). */
  turn_end: { event: { turn: number; message: AssistantMessage }; result: { inject?: string; stop?: string } };
  /** `stopped` is set when cause is "stopped" (since 1.4.0). */
  agent_end: { event: { cause: StopCause; stopped?: { plugin: string; reason: string } }; result: { inject?: string } };
  /** Free up context (e.g. session.replaceMessages) and return `retry` to continue. */
  context_limit: { event: { tokens: number; contextWindow: number }; result: { retry?: boolean } };
}

export type HookName = keyof HookMap;

/** Where a hook fired. (since 1.8.0) */
export interface HookContext {
  /** Undefined for main; a unique id for each subagent run (since 1.8.0). */
  agent?: string;
}

export type HookHandler<K extends HookName> = (
  event: HookMap[K]["event"],
  ctx: HookContext,
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

export interface ShowTextOptions {
  title: string;
  text: string;
  /** Presentation only; no HTML, scripts or automatic link opening. Default: text. */
  format?: "text" | "markdown";
}

/** (since 1.10.0) */
export interface InputOptions {
  /** A line under the title, e.g. what the answer is for. */
  message?: string;
  placeholder?: string;
  /** Text the field starts with. */
  initial?: string;
}

/** (since 1.10.0) */
export interface SelectManyOptions {
  /** Values checked when the list opens. */
  selected?: string[];
}

export interface SettingsDefinition<T extends Record<string, unknown> = Record<string, unknown>> {
  /** Supported JSON Schema subset is documented in docs/plugins.md. Unknown keywords fail. */
  schema: JSONSchema;
  /** Recursively merged with settings. Arrays and null replace; no coercion or mutation. */
  defaults?: Partial<T>;
}

export interface PluginUI {
  /** False in headless runs: confirm resolves false and select undefined. */
  readonly interactive: boolean;
  /** The language the user reads the interface in: show notices and command descriptions in it. (since 1.8.0) */
  readonly lang: "ja" | "en";
  notify(message: string, level?: "info" | "warning" | "error"): void;
  /** Read-only text; sanitized headless stderr, user-initiated copy only (since 1.9.0). */
  showText(options: ShowTextOptions): Promise<void>;
  confirm(title: string, message?: string): Promise<boolean>;
  select(title: string, options: SelectOption[]): Promise<string | undefined>;
  /** One-line input; undefined on cancel or headless (since 1.10.0). */
  input(title: string, options?: InputOptions): Promise<string | undefined>;
  /** Checked values in option order; undefined on cancel/headless (since 1.10.0). */
  selectMany(title: string, options: SelectOption[], opts?: SelectManyOptions): Promise<string[] | undefined>;
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
  /** Called once the loop starts: `send` adds a user message at its next turn, `stop` aborts it. (since 1.8.0) */
  onStart?(run: { send(text: string): void; stop(): void }): void;
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
  /** Run an independent agent loop with its own history (subagents). `error`: why it stopped, when cause is "error" (since 1.8.0). */
  run(opts: SubagentOptions): Promise<{ text: string; messages: Message[]; cause: StopCause; error?: string }>;
  /** One model call without tools; returns the text. */
  complete(opts: CompleteOptions): Promise<string>;
}

export interface PermissionRules {
  allow?: string[];
  ask?: string[];
  deny?: string[];
  /**
   * Denied like `deny`, unless a `permission` hook hands the call to the user (it never runs
   * unasked). For broad patterns that also catch legitimate calls. (since 1.7.0)
   */
  softDeny?: string[];
}

export interface PluginAPI {
  readonly version: string;
  /** This plugin's name. */
  readonly name: string;
  readonly cwd: string;
  /** This plugin's settings from config (`plugins.settings.<name>`). */
  readonly settings: Record<string, unknown>;
  /** Validate settings/schema/defaults before registering hooks; throws on invalid fields (since 1.9.0). */
  defineSettings<T extends Record<string, unknown> = Record<string, unknown>>(definition: SettingsDefinition<T>): T;
  /** Adding a tool with an existing name replaces it (P3). */
  registerTool(tool: ToolDefinition<any>): void;
  /** Unregister this plugin's tool; leaves other plugins' tools alone (since 1.8.0). */
  unregisterTool(name: string): void;
  registerCommand(command: CommandDefinition): void;
  /** Add a provider (`name`) and, optionally, the implementation for its api (FR-P06). */
  registerProvider(name: string, config: ProviderConfig, implementation?: Provider): void;
  on<K extends HookName>(event: K, handler: HookHandler<K>): void;
  /** Headless waits for ready work before requests; TUI remains responsive (since 1.1.0). */
  ready(work: Promise<unknown>): void;
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

// ── host helpers (since 1.8.0) ───────────────────────────────────────

/** Where sasacode keeps its files: $SASACODE_HOME, or ~/.sasacode. (since 1.8.0) */
export function sasacodeHome(): string {
  return process.env.SASACODE_HOME ?? join(homedir(), ".sasacode");
}

// On globalThis: a plugin bundling its own copy of this module shares the host's list.
const hiddenEnv: Set<string> = ((globalThis as Record<symbol, unknown>)[Symbol.for("sasacode.hiddenEnv")] ??= new Set<string>()) as Set<string>;

/** Keep these variables out of childEnv(): secrets sasacode loaded for itself (~/.sasacode/.env). (since 1.8.0) */
export function hideFromChildren(names: Iterable<string>): void {
  for (const n of names) hiddenEnv.add(n);
}

/** Child environment excludes hidden variables and merges extra overrides (since 1.8.0). */
export function childEnv(extra: Record<string, string> = {}): Record<string, string> {
  const env: Record<string, string> = {};
  for (const [k, v] of Object.entries(process.env)) if (v !== undefined && !hiddenEnv.has(k)) env[k] = v;
  return { ...env, ...extra };
}

const OUTPUT_DAYS = 7;
let pruned = false;

/** Save long output privately in sasacodeHome()/tmp; prune files older than a week (since 1.8.0). */
export function saveOutput(prefix: string, content: string): string {
  const dir = join(sasacodeHome(), "tmp");
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  if (!pruned) {
    pruned = true;
    const cutoff = Date.now() - OUTPUT_DAYS * 86_400_000;
    for (const f of readdirSync(dir))
      try {
        if (statSync(join(dir, f)).mtimeMs < cutoff) unlinkSync(join(dir, f));
      } catch {}
  }
  const file = join(dir, `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}.log`);
  writeFileSync(file, content, { mode: 0o600 });
  return file;
}
