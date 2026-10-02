// The TUI driven through a stand-in terminal: keys in, rendered text out. No model: a replay provider.
import { expect, test } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Terminal } from "@earendil-works/pi-tui";
import { Agent, PermissionPolicy, PluginHost } from "@sasacode/agent";
import { currentLang, setLang } from "@sasacode/host";
import { type AssistantContent, type AssistantMessage, emptyUsage, type ModelInfo, registerApi } from "@sasacode/ai";
import { replayProvider } from "@sasacode/testing";
import { PHRASES } from "../src/activity.ts";
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
    saveLang: (lang) => void calls.push(`saveLang:${lang}`),
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

test("a long tool call shows its arguments arriving, then that its permission check is running", async () => {
  // A model writing a large file sends the content as the call's arguments for many seconds, and
  // an agent-mode judge or jev-guard then asks a model again: neither may look like a stall.
  const input = { path: "big.txt", content: "x".repeat(3000) };
  const json = JSON.stringify(input);
  let resume!: () => void;
  const midStream = new Promise<void>((r) => (resume = r));
  let allow!: () => void;
  const checked = new Promise<void>((r) => (allow = r));
  const { term, host, agent } = await start([]);
  let requests = 0;
  registerApi("replay", {
    api: "replay",
    async *stream() {
      const partial = reply([]);
      yield { type: "start", partial };
      if (requests++) return yield { type: "done", message: reply([{ type: "text", text: "written" }]) };
      partial.content.push({ type: "tool_call", id: "w1", name: "write", input: {} });
      yield { type: "toolcall_start", index: 0, id: "w1", name: "write", partial };
      yield { type: "toolcall_delta", index: 0, delta: json.slice(0, 2000), partial };
      await midStream;
      yield { type: "toolcall_delta", index: 0, delta: json.slice(2000), partial };
      yield { type: "done", message: reply([{ type: "tool_call", id: "w1", name: "write", input }]) };
    },
  });
  await host.load("slow-check", (api) => api.on("permission", async () => void (await checked)));
  await term.type("write it");
  await term.send("\r");
  await Bun.sleep(100);
  expect(term.text()).toContain("受信中… 2,000 字");
  const before = term.out.length;
  resume();
  await Bun.sleep(150);
  expect(term.text(before)).toContain("write の実行前の確認中");
  expect(agent.messages.some((m) => m.role === "tool")).toBe(false);
  allow();
  for (let i = 0; i < 40 && !term.text().includes("written"); i++) await Bun.sleep(50);
  expect(term.text()).toContain("written");
  expect(agent.messages.some((m) => m.role === "tool" && !m.isError)).toBe(true);
});

test("while the model streams, the activity line counts its tokens up, then shows the real count", async () => {
  let resume!: () => void;
  const midStream = new Promise<void>((r) => (resume = r));
  let finish!: () => void;
  const secondReply = new Promise<void>((r) => (finish = r));
  const { term } = await start([]);
  let requests = 0;
  registerApi("replay", {
    api: "replay",
    async *stream() {
      const partial = reply([]);
      yield { type: "start", partial };
      if (requests++) {
        await secondReply;
        return yield { type: "done", message: reply([{ type: "text", text: "the end" }]) };
      }
      const thinking = { type: "thinking" as const, thinking: "a".repeat(2000) }; // about 500 tokens
      partial.content.push({ ...thinking, thinking: "" });
      yield { type: "thinking_delta", index: 0, delta: thinking.thinking, partial };
      await midStream;
      const call = { type: "tool_call" as const, id: "r1", name: "read", input: { path: "notes.txt" } };
      yield { type: "done", message: reply([thinking, call], { usage: { ...emptyUsage(), output: 1234 } }) };
    },
  });
  await term.type("go");
  await term.send("\r");
  const counts = () => [...term.text().matchAll(/↓ ([\d.]+k?) トークン/g)].map((m) => (m[1]!.endsWith("k") ? parseFloat(m[1]!) * 1000 : Number(m[1])));
  await Bun.sleep(1500);
  const seen = counts();
  expect(seen.length).toBeGreaterThan(3);
  expect(seen[0]!).toBeLessThan(500); // it climbs, not jumps
  expect(Math.max(...seen)).toBe(500);
  expect(seen).toEqual([...seen].sort((x, y) => x - y));
  // The line names something to be doing, from the list for the current language.
  expect(PHRASES.ja.some((p) => term.text().includes(p))).toBe(true);
  expect(term.text()).toMatch(/\d+秒 · ↓ [\d.]+k? トークン · esc で中断/);
  // The reply ends: the provider's own count (1,234) replaces the estimate, and the next reply adds to it.
  resume();
  await Bun.sleep(1500);
  expect(term.text()).toContain("↓ 1.2k トークン");
  finish();
  await Bun.sleep(200);
  expect(term.text()).toContain("the end");
});

test("/language en switches the interface at once and saves it; /language ja switches back", async () => {
  try {
    const { term, host, calls } = await start([]);
    const plugin = host.api("probe");
    expect(plugin.ui.lang).toBe("ja");
    await term.type("/language en");
    await term.send("\r");
    await Bun.sleep(100);
    expect(currentLang()).toBe("en");
    expect(plugin.ui.lang).toBe("en"); // plugins read it live
    expect(calls).toContain("saveLang:en");
    expect(term.text()).toContain("Interface language: English");
    expect(term.text()).toContain("permissions: reads and edits only"); // the footer
    expect(host.commands.find((c) => c.name === "clear")?.description).toBe("start a new session");
    await term.type("/help");
    await term.send("\r");
    await Bun.sleep(100);
    expect(term.text()).toContain("switch the interface language");
    await term.type("/language ja");
    await term.send("\r");
    await Bun.sleep(100);
    expect(currentLang()).toBe("ja");
    expect(host.commands.find((c) => c.name === "clear")?.description).toBe("新しいセッションを始める");
    expect(calls).toContain("saveLang:ja");
    await term.type("/language klingon");
    await term.send("\r");
    await Bun.sleep(50);
    expect(term.text()).toContain("使い方: /language ja|en");
  } finally {
    setLang("ja");
  }
});
