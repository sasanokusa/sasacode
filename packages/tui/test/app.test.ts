// The TUI driven through a stand-in terminal: keys in, rendered text out. No model: a replay provider.
import { expect, test } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Terminal } from "@earendil-works/pi-tui";
import { Agent, PermissionPolicy, PluginHost } from "@sasacode/agent";
import { setLang } from "@sasacode/host";
import { type AssistantContent, type AssistantMessage, emptyUsage, type ModelInfo, registerApi } from "@sasacode/ai";
import { replayProvider } from "@sasacode/testing";
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

async function start(script: AssistantMessage[], mode: "edits" | "auto" = "edits", listed: Awaited<ReturnType<TuiHost["listModels"]>> = []) {
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
    listModels: async () => listed,
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
  return { term, agent, host, calls, exited };
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

test("lang en: the approval prompt, the footer and /help are in English", async () => {
  setLang("en");
  try {
    const { term } = await start([reply([{ type: "tool_call", id: "1", name: "bash", input: { command: "echo hi" } }]), reply([{ type: "text", text: "done" }])]);
    expect(term.text()).toContain("permissions: reads and edits only");
    await term.type("run it");
    await term.send("\r");
    await Bun.sleep(100);
    expect(term.text()).toContain("Run bash?");
    expect(term.text()).toContain("Always allow (until exit)");
    await term.send("\r");
    await Bun.sleep(300);
    await term.type("/help");
    await term.send("\r");
    await Bun.sleep(100);
    expect(term.text()).toContain("show commands and keys");
    expect(term.text()).not.toMatch(/[ぁ-ん]/);
  } finally {
    setLang("ja");
  }
});

test("a model without num_ctx gets one warning, at startup and not again on /model", async () => {
  const { term } = await start([], "edits", [{ spec: "t/m", maxContext: 262_144 }]);
  await Bun.sleep(50);
  expect(term.text()).toContain("t/m には num_ctx が設定されていません");
  const before = term.out.length;
  await term.type("/model t/m");
  await term.send("\r"); // takes the completion
  await term.send("\r");
  await Bun.sleep(50);
  expect(term.text(before)).toContain("モデル: t/m");
  expect(term.text(before)).not.toContain("num_ctx が設定されていません");
});

test("a model whose size is known gets no warning", async () => {
  const { term } = await start([], "edits", [{ spec: "t/m", contextWindow: 32_768, maxContext: 262_144 }]);
  await Bun.sleep(50);
  expect(term.text()).not.toContain("num_ctx");
});


test("plugin text viewer owns input, serializes with approvals and returns to the editor", async () => {
  const { term, host, agent, exited } = await start([]);
  const api = host.api("viewer-test");
  let closed = false;
  const viewing = api.ui.showText({ title: "Long preview", text: Array.from({ length: 60 }, (_, i) => `entry-${i}`).join("\n") }).then(() => { closed = true; });
  await Bun.sleep(40);
  expect(term.text()).toContain("Long preview");
  const bash = agent.getTools().find((t) => t.name === "bash")!;
  const approval = agent.approve!({ tool: bash, args: { command: "echo queued" }, cwd: agent.cwd,
    call: { type: "tool_call", id: "queued", name: "bash", input: { command: "echo queued" } }, reason: "test" });
  const from = term.out.length;
  await term.send("G"); expect(term.text(from)).toContain("entry-59"); expect(closed).toBe(false);
  await term.send("\x1b"); await viewing;
  expect(term.text()).toContain("echo queued");
  await term.send("\x1b"); expect((await approval).decision).toBe("deny");
  await term.type("/exit"); await term.send("\r"); expect(await exited).toBe(0);
});

test("an interrupted approval closes and restores input", async () => {
  const { term, agent, exited } = await start([]);
  const controller = new AbortController();
  const bash = agent.getTools().find((t) => t.name === "bash")!;
  const approval = agent.approve!({ tool: bash, args: { command: "echo cancelled" }, cwd: agent.cwd,
    call: { type: "tool_call", id: "cancelled", name: "bash", input: {} }, reason: "test", signal: controller.signal });
  await Bun.sleep(30); controller.abort(); expect((await approval).decision).toBe("deny");
  await term.type("/exit"); await term.send("\r"); expect(await exited).toBe(0);
});
