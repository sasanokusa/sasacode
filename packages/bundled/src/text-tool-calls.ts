import { parseToolInput } from "@sasacode/ai";
import type { Plugin, ToolCall } from "@sasacode/plugin-api";
import { resolveToolName } from "./tool-repair.ts";

/** A tool call written in the reply text: the tool's name and its arguments' JSON as written. */
export interface TextCall {
  name: string;
  args: string;
}

// Strict shapes only. A closing tag may be missing at the very end (some servers stop on it).
/** Qwen / Hermes: <tool_call>{"name": …, "arguments": …}</tool_call> */
const TAG = /<tool_call>\s*(\{[\s\S]*?\})\s*(?:<\/tool_call>|$)/g;
/** Llama 3.x: <function=read>{"path": …}</function> */
const FUNCTION = /<function=([\w.:-]+)>\s*(\{[\s\S]*?\})\s*(?:<\/function>|$)/g;
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
    return last ? { name: last[2]!, args: last[1]! } : undefined;
  }
  const o = (v && typeof v === "object" && !Array.isArray(v) ? v : {}) as Record<string, unknown>;
  const keys = Object.keys(o);
  // Exactly a name and its arguments: a tool definition (it has a description) or other JSON is no call.
  if (typeof o.name !== "string" || keys.length !== 2 || !keys.every((k) => ["name", "arguments", "parameters"].includes(k))) return;
  const args = o.arguments ?? o.parameters;
  return { name: o.name, args: typeof args === "string" ? args : JSON.stringify(args ?? {}) };
}

/**
 * The tool calls a reply writes as text, when they end it: the matches at the very end of `text`,
 * with nothing but whitespace after and between them. Prose before them stays (`prose`); a call
 * that is followed by more text, such as an example inside an explanation or a code block, is not
 * one. A ```json call also needs the prose before it to be short and to hold no other code block.
 * Undefined when the text does not end with calls, or when one of them cannot be read.
 */
export function extractTextCalls(text: string): { prose: string; calls: TextCall[] } | undefined {
  const found: { start: number; end: number; call?: TextCall; fence?: boolean }[] = [];
  const add = (m: RegExpExecArray, call: TextCall | undefined, fence = false) => found.push({ start: m.index, end: m.index + m[0].length, call, fence });
  for (const m of text.matchAll(TAG)) add(m, envelope(m[1]!));
  for (const m of text.matchAll(FUNCTION)) add(m, { name: m[1]!, args: m[2]! });
  for (const m of text.matchAll(FENCE)) add(m, envelope(m[1]!), true);
  found.sort((a, b) => a.end - b.end);
  const run: typeof found = [];
  let end = text.length;
  for (const f of found.reverse()) {
    if (f.end > end) continue; // inside a call already taken (a tag in a string argument)
    if (text.slice(f.end, end).trim()) break;
    run.unshift(f);
    end = f.start;
  }
  if (!run.length || run.some((f) => !f.call)) return undefined;
  const prose = text.slice(0, end).trimEnd();
  if (run.some((f) => f.fence) && (prose.length > FENCE_PROSE || prose.includes("```"))) return undefined;
  return { prose, calls: run.map((f) => f.call!) };
}

const ID = "textcall";
const NOTE = "this call was written as text in your reply and was picked up from there; call tools with native tool calls instead";

/**
 * Small models behind servers whose chat template has no tool-call format (llama.cpp and others)
 * sometimes write the call into the reply instead of making it. Such a reply would end the run as
 * a final answer; this plugin turns the calls into real ones, which then run like any other.
 *
 * Only a reply that ended by itself (stopReason "stop"), made no native call and carries no signed
 * reasoning (a rewritten turn would not replay) is looked at, and only the calls that end its text
 * (see extractTextCalls). Every name must be a known tool or resolve to one (resolveToolName);
 * otherwise the reply stays as it was. The arguments go in as written: broken JSON is left in
 * `rawInput` for tool-repair. The original text is recorded in the session (`text_tool_call`), and
 * the model is told with each result that native calls are expected.
 */
const textToolCalls: Plugin = (api) => {
  api.on("assistant_message", ({ message, stopped }, ctx) => {
    if (stopped || message.stopReason !== "stop") return;
    if (message.content.some((b) => b.type === "tool_call" || (b.type === "thinking" && b.signature))) return;
    const i = message.content.findLastIndex((b) => b.type === "text");
    const block = message.content[i];
    if (block?.type !== "text") return;
    const found = extractTextCalls(block.text);
    // The main agent's tools; a subagent that lacks one gets "Unknown tool" back, as for any call.
    const names = api.agent.tools().map((t) => t.name);
    if (!found || found.calls.some((c) => !resolveToolName(c.name, names))) return;
    const calls = found.calls.map((c): ToolCall => ({
      type: "tool_call",
      id: ID + crypto.randomUUID().replaceAll("-", "").slice(0, 16),
      name: c.name,
      ...parseToolInput(c.args),
    }));
    api.session.append("text_tool_call", { text: block.text, calls: calls.map((c) => c.id), agent: ctx.agent });
    const content = [...message.content];
    if (found.prose) content[i] = { ...block, text: found.prose };
    else content.splice(i, 1);
    return { message: { ...message, content: [...content, ...calls], stopReason: "tool_use" } };
  });

  api.on("tool_call_raw", ({ call, name, input, rawInput, tools }) => {
    if (!call.id.startsWith(ID)) return;
    // tool-repair (loaded before) has already renamed it; without tool-repair this does.
    const rename = tools.some((t) => t.name === name) ? undefined : resolveToolName(name, tools.map((t) => t.name));
    // Still broken JSON: validation reports it. Otherwise a fresh input object, since a note alone
    // (no change to the call) is not passed on to the model.
    return rawInput !== undefined ? { name: rename, note: NOTE } : { name: rename, input: { ...input }, note: NOTE };
  });
};
export default textToolCalls;
