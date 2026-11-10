// A small MCP server for tests: `bun fixture-server.ts` speaks stdio; import makeServer() for HTTP.
import { appendFileSync } from "node:fs";
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  CallToolRequestSchema,
  GetPromptRequestSchema,
  ListPromptsRequestSchema,
  ListToolsRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";

const broken = (): never => {
  throw new Error("tools are broken");
};

export function makeServer(): Server {
  // `toggle` lists and unlists the `late` tool, announcing the change like a real server would.
  let late = false;
  // FIXTURE_NO_TOOLS: prompts only, so there is no tools/list to answer (#101).
  const noTools = !!process.env.FIXTURE_NO_TOOLS;
  const server = new Server({ name: "fixture", version: "1.0.0" }, { capabilities: noTools ? { prompts: {} } : { tools: { listChanged: true }, prompts: {} } });
  // FIXTURE_TOOLS_FAIL: connects, then cannot list its tools (#101).
  if (!noTools) server.setRequestHandler(ListToolsRequestSchema, async () => ({
    tools: process.env.FIXTURE_TOOLS_FAIL ? broken() : [
      {
        name: "add",
        description: "Add two numbers",
        inputSchema: { type: "object", properties: { a: { type: "number" }, b: { type: "number" } }, required: ["a", "b"] },
        annotations: { readOnlyHint: true },
      },
      { name: "fail", description: "Always fails", inputSchema: { type: "object", properties: {} } },
      { name: "big", description: "Large output", inputSchema: { type: "object", properties: {} } },
      { name: "toggle", description: "List or unlist the late tool", inputSchema: { type: "object", properties: {} } },
      ...(late ? [{ name: "late", description: "Listed after toggle", inputSchema: { type: "object", properties: {} } }] : []),
    ],
  }));
  if (!noTools) server.setRequestHandler(CallToolRequestSchema, async (req) => {
    const args = (req.params.arguments ?? {}) as { a: number; b: number };
    if (req.params.name === "add") return { content: [{ type: "text", text: String(args.a + args.b) }] };
    if (req.params.name === "big") return { content: [{ type: "text", text: "x".repeat(150_000) }] };
    if (req.params.name === "toggle") {
      late = !late;
      await server.sendToolListChanged();
      return { content: [{ type: "text", text: `late is now ${late ? "listed" : "gone"}` }] };
    }
    return { content: [{ type: "text", text: "it broke" }], isError: true };
  });
  // Two pages, as a server with many prompts sends them.
  server.setRequestHandler(ListPromptsRequestSchema, async (req) =>
    process.env.FIXTURE_ENDLESS_PROMPTS // a broken server: always one more page (#49)
      ? { prompts: [{ name: "again", description: "Again" }], nextCursor: "again" }
      : req.params?.cursor === "page2"
      ? { prompts: [{ name: "later", description: "On the second page" }] }
      : {
          prompts: [
            { name: "review", description: "Review a file", arguments: [{ name: "file", required: true }] },
            { name: "greet", description: "Greet someone", arguments: [{ name: "who" }, { name: "greeting" }] },
          ],
          nextCursor: "page2",
        },
  );
  server.setRequestHandler(GetPromptRequestSchema, async (req) => ({
    messages: [
      {
        role: "user",
        content: { type: "text", text: req.params.name === "greet" ? `args ${JSON.stringify(req.params.arguments)}` : `Please review ${req.params.arguments?.file}` },
      },
    ],
  }));
  return server;
}

if (import.meta.main) {
  // FIXTURE_LOG: a line for every process started and ended, to count them (#101).
  const log = (what: string) => process.env.FIXTURE_LOG && appendFileSync(process.env.FIXTURE_LOG, `${what} ${process.pid}\n`);
  log("start");
  // FIXTURE_SLOW_EXIT: takes this many ms to shut down once the client goes away, as a server that
  // closes a browser or writes out a database does.
  process.stdin.on("end", () => setTimeout(() => (log("exit"), process.exit(0)), Number(process.env.FIXTURE_SLOW_EXIT ?? 0)));
  await makeServer().connect(new StdioServerTransport());
}
