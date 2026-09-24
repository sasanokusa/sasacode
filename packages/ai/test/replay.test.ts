import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { type AssistantMessage, emptyUsage, type Request, recordingProvider, replayProvider, type StreamEvent } from "../src/index.ts";

const model = { id: "m", provider: "t", api: "replay", contextWindow: 1e5, maxOutput: 1e3 };
const req = (signal?: AbortSignal): Request => ({ model, system: "", messages: [], tools: [], signal });
const msg: AssistantMessage = {
  role: "assistant",
  content: [
    { type: "text", text: "hello world" },
    { type: "tool_call", id: "1", name: "read", input: { path: "a.txt" } },
  ],
  api: "replay",
  provider: "t",
  model: "m",
  usage: emptyUsage(),
  stopReason: "tool_use",
  timestamp: 0,
};
async function collect(it: AsyncGenerator<StreamEvent>) {
  const out: StreamEvent[] = [];
  for await (const e of it) out.push(e);
  return out;
}
const deltas = (events: StreamEvent[], type: string) => events.filter((e) => e.type === type).map((e) => (e as { delta: string }).delta);

test("recorded deltas are replayed as they came", async () => {
  const dir = mkdtempSync(join(tmpdir(), "replay-"));
  const file = join(dir, "rec.jsonl");
  await collect(recordingProvider(replayProvider([msg], { chunkSize: 3 }), file).stream(req()));
  const events = await collect(replayProvider(file).stream(req()));
  expect(deltas(events, "text_delta")).toEqual(["hel", "lo ", "wor", "ld"]);
  expect(deltas(events, "toolcall_delta").join("")).toBe('{"path":"a.txt"}');
  expect(events.at(-1)).toEqual({ type: "done", message: msg });
  rmSync(dir, { recursive: true, force: true });
});

test("an abort between deltas ends with the text so far and no half tool call", async () => {
  const ac = new AbortController();
  const events: StreamEvent[] = [];
  for await (const e of replayProvider([msg], { chunkSize: 2 }).stream(req(ac.signal))) {
    events.push(e);
    if (e.type === "text_delta" && deltas(events, "text_delta").length === 2) ac.abort();
  }
  const done = events.at(-1) as Extract<StreamEvent, { type: "done" }>;
  expect(done.message.stopReason).toBe("aborted");
  expect(done.message.content).toEqual([{ type: "text", text: "hell" }]);
});
