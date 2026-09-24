import { afterAll, expect, test } from "bun:test";
import { join } from "node:path";
import { WebStandardStreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/webStandardStreamableHttp.js";
import { Agent, PermissionPolicy, PluginHost } from "@sasacode/agent";
import type { ModelInfo } from "@sasacode/ai";
import { convertResult, createMcpPlugin, toolName } from "../src/index.ts";
import { makeServer } from "./fixture-server.ts";

const model: ModelInfo = { id: "m", provider: "t", api: "replay", contextWindow: 100_000, maxOutput: 1000 };

// Streamable HTTP server: one transport per session, created on initialize.
const sessions = new Map<string, WebStandardStreamableHTTPServerTransport>();
const http = Bun.serve({
  port: 0,
  async fetch(req) {
    const id = req.headers.get("mcp-session-id");
    let t = id ? sessions.get(id) : undefined;
    if (!t) {
      if (req.headers.get("authorization") !== "Bearer secret-token") return new Response("unauthorized", { status: 401 });
      t = new WebStandardStreamableHTTPServerTransport({
        sessionIdGenerator: () => crypto.randomUUID(),
        onsessioninitialized: (sid) => void sessions.set(sid, t!),
      });
      await makeServer().connect(t);
    }
    return t.handleRequest(req);
  },
});
afterAll(() => http.stop(true));

async function load(servers: Parameters<typeof createMcpPlugin>[0]) {
  const agent = new Agent({ model, cwd: process.cwd(), systemPrompt: "", permissions: new PermissionPolicy("auto") });
  const host = new PluginHost({ agent, cwd: process.cwd() });
  const notes: string[] = [];
  host.setUI({ interactive: false, notify: (m) => notes.push(m), confirm: async () => false, select: async () => undefined });
  await host.load("mcp", createMcpPlugin(servers));
  const until = async (cond: () => boolean) => {
    for (let i = 0; i < 100 && !cond(); i++) await Bun.sleep(50);
  };
  return { agent, host, notes, until };
}

test("stdio server: tools are namespaced, callable, and prompts become commands", async () => {
  const { agent, host, until } = await load({ fx: { command: "bun", args: [join(import.meta.dir, "fixture-server.ts")] } });
  await until(() => agent.getTools().length === 3 && host.commands.some((c) => c.name.startsWith("mcp__fx__review")));
  const add = agent.getTools().find((t) => t.name === "mcp__fx__add")!;
  expect(add.kind).toBe("other");
  expect(add.concurrent).toBe(true);
  const ctx = { cwd: ".", signal: new AbortController().signal };
  expect(await add.execute({ a: 2, b: 3 }, ctx)).toEqual({ content: [{ type: "text", text: "5" }], isError: undefined });
  const fail = await agent.getTools().find((t) => t.name === "mcp__fx__fail")!.execute({}, ctx);
  expect(fail.isError).toBe(true);
  const big = await agent.getTools().find((t) => t.name === "mcp__fx__big")!.execute({}, ctx);
  expect((big.content[0] as { text: string }).text).toContain("Output truncated at 100000 of 150000");
  expect(host.status.get("mcp:servers")).toBe("MCP 1/1");
  await agent.hooks.run("session_end", {});
});

test("streamable HTTP server with headers from the environment", async () => {
  process.env.FIXTURE_TOKEN = "secret-token";
  const { agent, until } = await load({ web: { url: `http://localhost:${http.port}/mcp`, headers: { authorization: "Bearer ${FIXTURE_TOKEN}" } } });
  await until(() => agent.getTools().length === 3);
  const add = agent.getTools().find((t) => t.name === "mcp__web__add")!;
  const r = await add.execute({ a: 40, b: 2 }, { cwd: ".", signal: new AbortController().signal });
  expect(r.content).toEqual([{ type: "text", text: "42" }]);
  await agent.hooks.run("session_end", {});
});

test("a server that cannot start is reported without stopping anything", async () => {
  const { agent, notes, until, host } = await load({ broken: { command: "definitely-not-a-command-xyz" } });
  await until(() => notes.length > 0);
  expect(notes[0]).toContain("MCP server broken failed to start");
  expect(agent.getTools()).toHaveLength(0);
  expect(host.status.get("mcp:servers")).toBe("MCP 0/1");
});

test("names and results are normalized", () => {
  expect(toolName("my server", "do.thing")).toBe("mcp__my_server__do_thing");
  expect(convertResult({ content: [{ type: "resource", resource: { uri: "file:///a", text: "hello" } }] }, "s", "t").content).toEqual([
    { type: "text", text: "hello" },
  ]);
  expect(convertResult({ content: [], structuredContent: { ok: 1 } }, "s", "t").content).toEqual([{ type: "text", text: '{"ok":1}' }]);
});
