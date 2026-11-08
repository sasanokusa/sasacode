import { isIP } from "node:net";
import { parseToolInput } from "@sasacode/ai";
import type { Plugin, ToolCall, ToolDefinition } from "@sasacode/plugin-api";
import { repairJson } from "./repair/json.ts";
import { resolveToolName } from "./tool-repair.ts";
import { privateAddress } from "./web-search.ts";

/** A tool call written in the reply text: the tool's name and its arguments' JSON as written. */
export interface TextCall {
  name: string;
  args: string;
}

// Strict shapes only. A closing tag may be missing at the very end (some servers stop on it) or
// before the next call: a body ends at the first `}` followed by a closing tag, the end of the text
// or the next opening tag. An opening tag elsewhere in a body (in a string argument) is content.
/** Qwen / Hermes: <tool_call>{"name": …, "arguments": …}</tool_call> */
const TAG = /<tool_call>\s*(\{[\s\S]*?\})\s*(?:<\/tool_call>|$|(?=<tool_call>))/g;
/** Llama 3.x: <function=read>{"path": …}</function> */
const FUNCTION = /<function=([\w.:-]+)>\s*(\{[\s\S]*?\})\s*(?:<\/function>|$|(?=<function=))/g;
/** A ```json fence holding nothing but {"name": …, "arguments" | "parameters": …} */
const FENCE = /```json[ \t]*\r?\n\s*(\{[\s\S]*?\})\s*```/g;
/** Prose a ```json call may follow: ordinary answers end with JSON examples too, after longer text. */
const FENCE_PROSE = 300;

/** {"name", "arguments" | "parameters"}, in either order (Qwen puts the name first, Hermes last). */
function envelope(body: string): TextCall | undefined {
  let v: unknown;
  try {
    v = JSON.parse(body);
  } catch {
    // Broken JSON: cut the arguments out as written, for tool-repair to mend.
    const first = /^\{\s*"name"\s*:\s*"([^"\\]+)"\s*,\s*"(?:arguments|parameters)"\s*:\s*([\s\S]*?)\s*\}$/.exec(body);
    if (first) return { name: first[1]!, args: first[2]! };
    const last = /^\{\s*"(?:arguments|parameters)"\s*:\s*([\s\S]*?)\s*,\s*"name"\s*:\s*"([^"\\]+)"\s*\}$/.exec(body);
    if (last) return { name: last[2]!, args: last[1]! };
    // Broken around the name as well (Python-style quotes): repaired as a whole.
    v = repairJson(body)?.value;
  }
  const o = (v && typeof v === "object" && !Array.isArray(v) ? v : {}) as Record<string, unknown>;
  const keys = Object.keys(o);
  // Exactly a name and its arguments: a tool definition (it has a description) or other JSON is no call.
  if (typeof o.name !== "string" || keys.length !== 2 || !keys.every((k) => ["name", "arguments", "parameters"].includes(k))) return;
  const args = o.arguments ?? o.parameters;
  return { name: o.name, args: typeof args === "string" ? args : JSON.stringify(args ?? {}) };
}

/**
 * JSON that parses only once a string or bracket is closed for it. A call that ends the reply
 * without its closing tag may have been cut off where a server stopped on a closing tag inside its
 * arguments (an example written into a file): repaired, it ran with what was left (#82).
 */
const cutOff = (json: string) => repairJson(json)?.fixes.some((f) => f.startsWith("closed")) ?? true;

/**
 * The tool calls a reply writes as text, when they end it: the matches at the very end of `text`,
 * with nothing but whitespace after and between them. Prose before them stays (`prose`); a call
 * that is followed by more text, such as an example inside an explanation or a code block, is not
 * one, and neither is a call inside a code block left open. A ```json call also needs the prose
 * before it to be short and to hold no other code block. `skipped` counts the readable calls left
 * in the prose outside code blocks and inline code. Undefined when the text does not end with
 * calls, or when one of them cannot be read.
 */
export function extractTextCalls(text: string): { prose: string; calls: TextCall[]; skipped: number } | undefined {
  const found: { start: number; end: number; call?: TextCall; fence?: boolean }[] = [];
  const add = (m: RegExpExecArray, call: TextCall | undefined, fence = false) => found.push({ start: m.index, end: m.index + m[0].length, call, fence });
  // Ends the text without its closing tag, and its JSON had to be closed: cut off, not a call.
  const open = (m: RegExpExecArray, close: string, json: string) => m.index + m[0].length === text.length && !m[0].trimEnd().endsWith(close) && cutOff(json);
  for (const m of text.matchAll(TAG)) add(m, open(m, "</tool_call>", m[1]!) ? undefined : envelope(m[1]!));
  for (const m of text.matchAll(FUNCTION)) add(m, open(m, "</function>", m[2]!) ? undefined : { name: m[1]!, args: m[2]! });
  for (const m of text.matchAll(FENCE)) add(m, envelope(m[1]!), true);
  found.sort((a, b) => b.end - a.end);
  const run: typeof found = [];
  let end = text.length;
  let i = 0;
  for (; i < found.length; i++) {
    const f = found[i]!;
    if (f.end > end) continue; // inside a call already taken (a tag in a string argument)
    if (text.slice(f.end, end).trim()) break;
    run.unshift(f);
    end = f.start;
  }
  if (!run.length || run.some((f) => !f.call)) return undefined;
  const prose = text.slice(0, end).trimEnd();
  const fences = prose.split("```").length - 1;
  if (fences % 2 || (run.some((f) => f.fence) && (prose.length > FENCE_PROSE || fences))) return undefined;
  // Calls left in the prose, but not examples: one in a code block or `…`, or inside another match.
  const code = [...prose.matchAll(/```[\s\S]*?```|`[^`\n]+`/g)].map((m) => [m.index, m.index + m[0].length] as const);
  const left = found.slice(i).filter((f) => f.call && f.end <= end && !code.some(([s, e]) => s <= f.start && f.end <= e));
  const skipped = left.filter((f) => !found.some((o) => o !== f && o.start <= f.start && f.end <= o.end)).length;
  return { prose, calls: run.map((f) => f.call!), skipped };
}

/** A server on this machine or the local network: a private address, or a name without a dot or with a local suffix. */
function localServer(baseUrl: string | undefined): boolean {
  let host = "";
  try {
    host = new URL(baseUrl ?? "").hostname.replace(/^\[|\]$/g, "");
  } catch {}
  return !!host && (isIP(host) ? privateAddress(host) : !host.includes(".") || /\.(localhost|local|internal|lan)$/i.test(host));
}

const ID = "textcall";
const NOTE = "this call was written as text in your reply and was picked up from there; call tools with native tool calls instead";

/** A call picked up from text that is not a read: it may be an example, so the user decides it (agent-judge does not). */
export const asksFirst = (call: ToolCall, tool: ToolDefinition): boolean => call.id.startsWith(ID) && tool.kind !== "read";

/**
 * Small models behind servers whose chat template has no tool-call format (llama.cpp and others)
 * sometimes write the call into the reply instead of making it. Such a reply would end the run as
 * a final answer; this plugin turns the calls into real ones, which then run like any other.
 *
 * Only models served from this machine or the local network are looked at (settings.always: every
 * model); a model behind a cloud API makes native calls and writes such text only as an example.
 * Only a reply that ended by itself (stopReason "stop"), made no native call and carries no signed
 * reasoning (a rewritten turn would not replay) is looked at, and only the calls that end its text
 * (see extractTextCalls). Every name must be a known tool or resolve to one without a typo
 * (resolveToolName); otherwise the reply stays as it was. The arguments go in as written: broken
 * JSON is left in `rawInput` for tool-repair. The original text is recorded in the session
 * (`text_tool_call`), and the model is told with each result that native calls are expected.
 * Outside auto mode, a call that is not a read is asked about first (asksFirst), also where edits
 * mode would run it unasked (a write inside the working directory) or agent mode would judge it.
 *
 * settings: { always?: boolean (default false) }
 */
const textToolCalls: Plugin = (api) => {
  const always = api.settings.always === true;
  /** Per first picked-up call: how many calls earlier in the text did not run. */
  const skipped = new Map<string, number>();

  api.on("assistant_message", ({ message, stopped }, ctx) => {
    if (stopped || message.stopReason !== "stop") return;
    // The main agent's model; a subagent on another provider is not known to be local.
    const model = api.agent.model();
    if (!always && !(model.provider === message.provider && localServer(model.baseUrl))) return;
    if (message.content.some((b) => b.type === "tool_call" || (b.type === "thinking" && b.signature))) return;
    const i = message.content.findLastIndex((b) => b.type === "text");
    const block = message.content[i];
    if (block?.type !== "text") return;
    const found = extractTextCalls(block.text);
    // The main agent's tools; a subagent that lacks one gets "Unknown tool" back, as for any call.
    const names = api.agent.tools().map((t) => t.name);
    if (!found || found.calls.some((c) => !resolveToolName(c.name, names, false))) return;
    const calls = found.calls.map((c): ToolCall => ({
      type: "tool_call",
      id: ID + crypto.randomUUID().replaceAll("-", "").slice(0, 16),
      name: c.name,
      ...parseToolInput(c.args),
    }));
    if (found.skipped) skipped.set(calls[0]!.id, found.skipped);
    api.session.append("text_tool_call", { text: block.text, calls: calls.map((c) => c.id), agent: ctx.agent });
    const content = [...message.content];
    if (found.prose) content[i] = { ...block, text: found.prose };
    else content.splice(i, 1);
    return { message: { ...message, content: [...content, ...calls], stopReason: "tool_use" } };
  });

  api.on("tool_call_raw", ({ call, name, tools }) => {
    if (!call.id.startsWith(ID)) return;
    // tool-repair (loaded before) has already renamed it; without tool-repair this does.
    const rename = tools.some((t) => t.name === name) ? undefined : resolveToolName(name, tools.map((t) => t.name), false);
    const n = skipped.get(call.id);
    skipped.delete(call.id);
    const note = n ? `${NOTE}; ${n} other call${n > 1 ? "s" : ""} written earlier in the text did not run` : NOTE;
    // Still broken JSON: validation reports it, with the note.
    return { name: rename, note };
  });

  // What the mode itself decides becomes a question: edits mode would run a write inside the working
  // directory unasked, and in agent mode agent-judge leaves the call to the user (asksFirst). auto
  // mode, rules and other plugins' decisions stay as they are.
  api.on("permission", ({ call, tool, mode, verdict }) => {
    if (asksFirst(call, tool) && mode !== "auto" && verdict.source === "mode")
      return { decision: "ask", reason: "a tool call written as text in the reply, which may be an example" };
  });
};
export default textToolCalls;
