// The only surface plugins (including the built-in tools) depend on.
// Hooks, providers, ui.* and session.* are added in M2; this is the subset M0/M1 need.
import type { JSONSchema, UserContent } from "@sasacode/ai";

export type { JSONSchema, UserContent } from "@sasacode/ai";

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
  /** Files the call touches, for path-based permission rules. */
  paths?(args: Args, cwd: string): string[];
  /** String matched by permission patterns such as `bash(git *)`. Defaults to paths. */
  matchTarget?(args: Args): string;
  /** One-line label for UIs, e.g. the command or path. */
  summary?(args: Args): string;
  execute(args: Args, ctx: ToolContext): Promise<ToolResult>;
}

export interface CommandContext {
  args: string;
}

export interface CommandDefinition {
  name: string;
  description: string;
  argumentHint?: string;
  run(ctx: CommandContext): Promise<void> | void;
}

export interface PluginAPI {
  /** Adding a tool with an existing name replaces it (P3). */
  registerTool(tool: ToolDefinition<any>): void;
  registerCommand(command: CommandDefinition): void;
}

export type Plugin = (api: PluginAPI) => void | Promise<void>;

export function text(t: string): UserContent {
  return { type: "text", text: t };
}

export function errorResult(message: string): ToolResult {
  return { content: [text(message)], isError: true };
}
