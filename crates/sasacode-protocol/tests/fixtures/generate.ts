// Regenerates the fixtures the Rust protocol tests read: a real `sasacode -p --output jsonl
// --control stdio` run against the e2e fake model server, and the session log it wrote.
//   bun crates/sasacode-protocol/tests/fixtures/generate.ts
// Run it again whenever the CLI's events or the session format change, and commit the result.
import { copyFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fakeServer } from "../../../../packages/e2e/test/fake-server.ts";
import { fakeConfig, sandbox } from "../../../../packages/e2e/test/harness.ts";

const here = import.meta.dir;
const server = fakeServer([
  { reasoning: "まずファイルを作る。", text: "ファイルを作ります。", toolCalls: [{ name: "bash", args: { command: "echo hi > made.txt" } }] },
  { text: "**完了**しました。`made.txt` を作成しています。" },
]);
const box = sandbox(fakeConfig(server.url, { permissions: { mode: "ask" } }));
try {
  const run = box.start(["-p", "ファイルを作って", "--output", "jsonl", "--control", "stdio"]);
  const lines: unknown[] = [];
  for (let e = await run.next(); e !== undefined; e = await run.next()) {
    lines.push(e);
    if ((e as any).type === "approval_request") {
      run.send("not json");
      run.send({ type: "approval", id: (e as any).id, decision: "allow" });
    }
    if ((e as any).type === "agent_end") run.end();
  }
  await run.exited;
  writeFileSync(join(here, "run.jsonl"), lines.map((l) => JSON.stringify(l)).join("\n") + "\n");
  const sessions = join(box.home, "sessions");
  const dir = join(sessions, readdirSync(sessions)[0]!);
  const log = readdirSync(dir).find((f) => f.endsWith(".jsonl"))!;
  copyFileSync(join(dir, log), join(here, "session.jsonl"));
  console.log(`wrote run.jsonl (${lines.length} events) and session.jsonl`);
} finally {
  box.cleanup();
  server.stop();
}
