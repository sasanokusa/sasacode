// The TUI driven through a stand-in terminal: keys in, rendered text out. No model: a replay provider.
import { expect, test } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Terminal } from "@earendil-works/pi-tui";
import { Agent, PermissionPolicy, PluginHost } from "@sasacode/agent";
import { type AssistantContent, type AssistantMessage, emptyUsage, type ModelInfo, registerApi, replayProvider } from "@sasacode/ai";
import { runTui, type TuiHost } from "../src/app.ts";

class FakeTerminal implements Terminal {
  out = "";
  private onInput?: (d: string) => void;
  start(onInput: (d: string) => void) {
    this.onInput = onInput;
  }
  stop() {}
  async drainInput() {}
  write(d: string) {
    this.out += d;
  }
  get columns() {
    return 100;
  }
  get rows() {
    return 40;
  }
  get kittyProtocolActive() {
    return false;
  }
  moveBy() {}
  hideCursor() {}
  showCursor() {}
  clearLine() {}
  clearFromCursor() {}
  clearScreen() {}
  setTitle() {}
  setProgress() {}
  /** Input as a terminal sends it; a short pause lets the app react and render. */
  async send(data: string) {
    this.onInput!(data);
    await Bun.sleep(30);
  }
  async type(text: string) {
    for (const ch of text) this.onInput!(ch);
    await Bun.sleep(30);
  }
  /** Everything written since `from`, colors removed. */
  text(from = 0) {
    return this.out.slice(from).replace(/\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07/g, "");
  }
}

const model: ModelInfo = { id: "m", provider: "t", api: "replay", contextWindow: 100_000, maxOutput: 1000 };
const reply = (content: AssistantContent[], extra: Partial<AssistantMessage> = {}): AssistantMessage => ({
  role: "assistant", content, api: "replay", provider: "t", model: "m", usage: emptyUsage(),
  stopReason: content.some((c) => c.type === "tool_call") ? "tool_use" : "stop", timestamp: 0, ...extra,
});

async function start(script: AssistantMessage[], mode: "edits" | "auto" = "edits") {
  registerApi("replay", replayProvider(script));
  const cwd = mkdtempSync(join(tmpdir(), "sasacode-tui-"));
  const agent = new Agent({ model, cwd, systemPrompt: "", permissions: new PermissionPolicy(mode) });
  const host = new PluginHost({ agent, cwd });
  const calls: string[] = [];
  const { default: builtinTools } = await import("../../tools/src/index.ts");
  await host.load("builtin-tools", builtinTools);
  const tuiHost: TuiHost = {
    agent,
    host,
    warnings: [],
    config: { tui: { altScreen: false, bell: false } },
    resolve: () => model,
    sessionsDir: cwd,
    listModels: async () => [],
    loadPlugins: async (ui) => host.setUI(ui),
    loadSession: async () => void calls.push("loadSession"),
    newSession: async () => {
      calls.push("newSession");
      agent.messages = [];
    },
    fork: async () => void calls.push("fork"),
    shutdown: async () => void calls.push("shutdown"),
  };
  const term = new FakeTerminal();
  const exited = runTui(tuiHost, "", term);
  await Bun.sleep(50);
  return { term, agent, calls, exited };
}

test("a prompt is sent and the answer shown; /clear starts a new session; /exit quits", async () => {
  const { term, calls, exited } = await start([reply([{ type: "text", text: "hello from the model" }])]);
  await term.type("hi");
  await term.send("\r");
  await Bun.sleep(100);
  expect(term.text()).toContain("hello from the model");
  await term.type("/clear");
  await term.send("\r");
  expect(calls).toContain("newSession");
  await term.type("/exit");
  await term.send("\r");
  expect(await exited).toBe(0);
  expect(calls).toContain("shutdown");
});

test("an approval prompt: the tool runs only after it is allowed", async () => {
  const { term, agent } = await start([reply([{ type: "tool_call", id: "1", name: "bash", input: { command: "echo approved-run" } }]), reply([{ type: "text", text: "done" }])]);
  await term.type("run it");
  await term.send("\r");
  await Bun.sleep(100);
  expect(term.text()).toContain("bash を実行しますか？");
  expect(agent.messages.some((m) => m.role === "tool")).toBe(false);
  await term.send("\r"); // はい
  await Bun.sleep(300);
  expect(JSON.stringify(agent.messages.find((m) => m.role === "tool")?.content)).toContain("approved-run");
});

test("shift+tab cycles the permission mode; auto is shown in red", async () => {
  const { term, agent } = await start([]);
  for (let i = 0; i < 3; i++) await term.send("\x1b[Z");
  expect(agent.permissions.mode).toBe("auto");
  expect(term.out).toMatch(/\x1b\[31m(\x1b\[1m)?権限: 自動許可/);
  expect(term.text()).toContain("権限モード auto");
});

test("the context-limit notice appears only when the run really stops there", async () => {
  const full = reply([{ type: "text", text: "long" }], { usage: { ...emptyUsage(), input: 99_000, output: 10 } });
  const { term } = await start([full]);
  await term.type("go");
  await term.send("\r");
  await Bun.sleep(100);
  expect(term.text()).toContain("コンテキストの上限に達したため停止しました");
});
