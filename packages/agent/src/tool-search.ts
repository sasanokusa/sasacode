import type { Message, ModelInfo } from "@sasacode/ai";
import type { ToolDefinition } from "@sasacode/plugin-api";

export interface ToolSearchConfig {
  /** auto: defer when the thresholds are crossed; always / never force it. */
  mode?: "auto" | "always" | "never";
  /** Defer when deferrable definitions reach this % of the context window (default 10). */
  percent?: number;
  /** ...or when there are at least this many deferrable tools (default 30). */
  count?: number;
}

export const TOOL_SEARCH_NAME = "tool_search";

/** Rough token estimate of a tool definition as sent to the model. */
export function toolTokens(t: ToolDefinition<any>): number {
  return Math.ceil((t.name.length + t.description.length + JSON.stringify(t.parameters).length) / 4);
}

export function shouldDefer(deferrable: ToolDefinition<any>[], model: ModelInfo, cfg: ToolSearchConfig = {}): boolean {
  const mode = cfg.mode ?? "auto";
  if (mode === "never" || !deferrable.length) return false;
  if (mode === "always") return true;
  const tokens = deferrable.reduce((n, t) => n + toolTokens(t), 0);
  return tokens >= (model.contextWindow * (cfg.percent ?? 10)) / 100 || deferrable.length >= (cfg.count ?? 30);
}

/** Deferred tools the conversation already used or searched for stay loaded (so resumed sessions keep them). */
export function loadedFromHistory(messages: readonly Message[]): Set<string> {
  const names = new Set<string>();
  for (const m of messages) {
    if (m.role !== "assistant") continue;
    for (const c of m.content) if (c.type === "tool_call") names.add(c.name);
  }
  for (const m of messages)
    if (m.role === "tool" && m.toolName === TOOL_SEARCH_NAME)
      for (const c of m.content) if (c.type === "text") for (const n of c.text.matchAll(/^### (\S+)$/gm)) names.add(n[1]!);
  return names;
}

export function searchTools(query: string, tools: ToolDefinition<any>[], limit = 5): ToolDefinition<any>[] {
  const q = query.trim();
  if (q.startsWith("select:")) {
    const wanted = q.slice(7).split(",").map((s) => s.trim());
    return tools.filter((t) => wanted.includes(t.name));
  }
  const terms = q.toLowerCase().split(/[\s_\-.]+/).filter(Boolean);
  return tools
    .map((t) => {
      const name = t.name.toLowerCase();
      const desc = t.description.toLowerCase();
      let score = 0;
      for (const term of terms) {
        if (name === term) score += 10;
        else if (name.includes(term)) score += 5;
        if (desc.includes(term)) score += 1;
      }
      return { t, score };
    })
    .filter((x) => x.score > 0)
    .sort((a, b) => b.score - a.score)
    .slice(0, limit)
    .map((x) => x.t);
}

/** The core search tool offered when definitions are deferred. Its description carries the catalog. */
export function makeToolSearchTool(deferred: () => ToolDefinition<any>[], onLoad: (names: string[]) => void): ToolDefinition<{ query: string; limit?: number }> {
  return {
    name: TOOL_SEARCH_NAME,
    get description() {
      const catalog = deferred()
        .map((t) => `- ${t.name}: ${t.description.split("\n")[0]!.slice(0, 160)}`)
        .join("\n");
      return `Load the full definitions of additional tools so you can call them. Query with keywords, or "select:name1,name2" for exact names. Tools available through this search:\n${catalog}`;
    },
    parameters: {
      type: "object",
      properties: {
        query: { type: "string", description: 'Keywords, or "select:<name>,<name>"' },
        limit: { type: "integer", minimum: 1, description: "Maximum results (default 5)" },
      },
      required: ["query"],
      additionalProperties: false,
    },
    kind: "read",
    concurrent: true,
    alwaysLoad: true,
    summary: (a) => a.query,
    async execute({ query, limit }) {
      const found = searchTools(query, deferred(), limit ?? 5);
      if (!found.length) return { content: [{ type: "text", text: `No tools match "${query}".` }] };
      onLoad(found.map((t) => t.name));
      const body = found
        .map((t) => `### ${t.name}\n${t.description}\nparameters: ${JSON.stringify(t.parameters)}`)
        .join("\n\n");
      return { content: [{ type: "text", text: `Loaded ${found.length} tool(s); you can call them now.\n\n${body}` }] };
    },
  };
}
