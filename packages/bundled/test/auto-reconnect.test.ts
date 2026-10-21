import { afterAll, expect, test } from "bun:test";
import { Agent, PermissionPolicy, PluginHost } from "@sasacode/agent";
import { type AssistantMessage, emptyUsage, registerApi } from "@sasacode/ai";
import { bundledPlugins } from "../src/index.ts";

// The endpoint answers, so the network is up: only the model call fails.
const server = Bun.serve({ port: 0, fetch: () => new Response("ok") });
afterAll(() => server.stop(true));

/** A provider that fails with each of `errors` in turn, then answers "ok". */
function flaky(errors: string[]) {
  const calls: string[] = [];
  registerApi("flaky", {
    api: "flaky",
    async *stream(req) {
      const last = req.messages.at(-1);
      calls.push(last?.role === "user" ? JSON.stringify(last.content) : "");
      const error = errors.shift();
      if (error) throw new Error(error);
      const message: AssistantMessage = { role: "assistant", content: [{ type: "text", text: "ok" }], api: "flaky", provider: "t", model: "m", usage: emptyUsage(), stopReason: "stop", timestamp: 0 };
      yield { type: "done", message };
    },
  });
  return calls;
}

async function start(settings: Record<string, unknown>) {
  const agent = new Agent({
    model: { id: "m", provider: "t", api: "flaky", baseUrl: server.url.href, contextWindow: 1e5, maxOutput: 1e3 },
    cwd: import.meta.dir,
    systemPrompt: "",
    permissions: new PermissionPolicy("auto"),
  });
  const host = new PluginHost({ agent, cwd: agent.cwd, settings: () => ({ quiet: true, ...settings }) });
  const status: string[] = [];
  host.setUI({ interactive: false, notify: () => {}, confirm: async () => false, select: async () => undefined });
  host.onChange = () => void status.push(...host.status.values());
  await host.load("auto-reconnect", bundledPlugins["auto-reconnect"]!);
  return { agent, status };
}

const settle = async (agent: Agent, ms: number) => {
  await Bun.sleep(ms);
  await agent.waitForIdle();
};

test("a server error that outlasts the SDK's retries: waits, then the work goes on", async () => {
  const calls = flaky(["503 Upstream model provider is temporarily unavailable.", "529 overloaded_error"]);
  const { agent, status } = await start({ serverRetryMs: 10 });
  expect(await agent.prompt("go")).toBe("error");
  await settle(agent, 100);
  await settle(agent, 100);
  expect(calls).toHaveLength(3);
  expect(calls[1]).toContain("サーバーの一時的なエラー");
  expect(status[0]).toContain("1/4");
  expect(agent.messages.at(-1)).toMatchObject({ role: "assistant", content: [{ text: "ok" }] });
});

test("an error that is the request's fault is not retried", async () => {
  const calls = flaky(["401 Invalid API key"]);
  const { agent } = await start({ serverRetryMs: 10 });
  expect(await agent.prompt("go")).toBe("error");
  await settle(agent, 100);
  expect(calls).toHaveLength(1);
});

test("retries stop after serverRetries", async () => {
  const calls = flaky(Array(5).fill("503 down"));
  const { agent } = await start({ serverRetryMs: 5, serverRetries: 2 });
  await agent.prompt("go");
  for (let i = 0; i < 4; i++) await settle(agent, 60);
  expect(calls).toHaveLength(3);
});

test("the user typing during the wait cancels the retry", async () => {
  const calls = flaky(["503 down"]);
  const { agent } = await start({ serverRetryMs: 300 });
  await agent.prompt("go");
  await Bun.sleep(50);
  await agent.prompt("something else");
  await settle(agent, 400);
  expect(calls).toHaveLength(2);
  expect(calls.join()).not.toContain("サーバーの一時的なエラー");
});
