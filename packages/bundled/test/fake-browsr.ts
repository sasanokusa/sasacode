#!/usr/bin/env bun
// Stands in for browsr-agent: `manifest` prints a manifest v1, `serve` runs an MCP server with search/open.
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { CallToolRequestSchema, ListToolsRequestSchema } from "@modelcontextprotocol/sdk/types.js";

const schemaVersion = Number(process.env.FAKE_SCHEMA ?? 1);
const tools = [
  { name: "search", description: "Search the web. Returns numbered results.", inputSchema: { type: "object", properties: { query: { type: "string" } }, required: ["query"] } },
  { name: "open", description: "Open a URL or a number from results/links. Returns page text.", inputSchema: { type: "object", properties: { url: { type: "string" } }, required: ["url"] } },
];
if (process.argv[2] === "manifest") {
  console.log(JSON.stringify({ manifest_version: 1, name: "browsr-4-agent", version: "0.2.0", tool_schema_version: schemaVersion, default_mode: "standard", modes: { standard: tools, single: [] }, entry: { command: "browsr-agent", args: ["serve"] } }));
} else if (process.argv[2] === "serve") {
  const server = new Server({ name: "fake-browsr", version: "0.2.0" }, { capabilities: { tools: {} } });
  server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools }));
  server.setRequestHandler(CallToolRequestSchema, async (r) => ({
    content: [{ type: "text", text: JSON.stringify(r.params.name === "search" ? { results: [{ id: 1, title: "Result", url: "https://example.com" }] } : { id: 1, text: `page ${(r.params.arguments as { url: string }).url}` }) }],
  }));
  await server.connect(new StdioServerTransport());
}
