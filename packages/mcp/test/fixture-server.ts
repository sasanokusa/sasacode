// A small MCP server for tests: `bun fixture-server.ts` speaks stdio; import makeServer() for HTTP.
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  CallToolRequestSchema,
  GetPromptRequestSchema,
  ListPromptsRequestSchema,
  ListToolsRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";

export function makeServer(): Server {
  const server = new Server({ name: "fixture", version: "1.0.0" }, { capabilities: { tools: {}, prompts: {} } });
  server.setRequestHandler(ListToolsRequestSchema, async () => ({
    tools: [
      {
        name: "add",
        description: "Add two numbers",
        inputSchema: { type: "object", properties: { a: { type: "number" }, b: { type: "number" } }, required: ["a", "b"] },
        annotations: { readOnlyHint: true },
      },
      { name: "fail", description: "Always fails", inputSchema: { type: "object", properties: {} } },
      { name: "big", description: "Large output", inputSchema: { type: "object", properties: {} } },
    ],
  }));
  server.setRequestHandler(CallToolRequestSchema, async (req) => {
    const args = (req.params.arguments ?? {}) as { a: number; b: number };
    if (req.params.name === "add") return { content: [{ type: "text", text: String(args.a + args.b) }] };
    if (req.params.name === "big") return { content: [{ type: "text", text: "x".repeat(150_000) }] };
    return { content: [{ type: "text", text: "it broke" }], isError: true };
  });
  server.setRequestHandler(ListPromptsRequestSchema, async () => ({
    prompts: [{ name: "review", description: "Review a file", arguments: [{ name: "file", required: true }] }],
  }));
  server.setRequestHandler(GetPromptRequestSchema, async (req) => ({
    messages: [{ role: "user", content: { type: "text", text: `Please review ${req.params.arguments?.file}` } }],
  }));
  return server;
}

if (import.meta.main) await makeServer().connect(new StdioServerTransport());
