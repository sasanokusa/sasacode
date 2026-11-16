// Regenerates the fixtures the Rust tests read: real `sasacode -p --output jsonl --control stdio`
// runs against the e2e fake model server, and the session logs they wrote.
//   bun crates/sasacode-protocol/tests/fixtures/generate.ts              # every scenario
//   bun crates/sasacode-protocol/tests/fixtures/generate.ts work stop    # only these
// Run it again whenever the CLI's events or the session format change, and commit the result.
//
//   run      one approval, one tool, a reply (the smallest whole run)        → run.jsonl, session.jsonl
//   work     read, edit and write with their diffs, a repaired call, a task list, two commands of
//            one reply (each is asked about right before it runs: one answered late, one denied
//            with a reason), "always", a failing command, a subagent, a message sent mid-run
//                                                                            → work.jsonl, work-session.jsonl
//   stop     aborted while a command runs                                    → stop.jsonl
//   cancel   aborted while an approval waits                                 → cancel.jsonl
//   error    the model server fails                                          → error.jsonl
//   dropped  the connection drops mid-reply and the reply is asked again     → dropped.jsonl
//   turns    the turn limit                                                  → turns.jsonl
//   loop     loop-guard stops a run that repeats one call                    → loop.jsonl
import { copyFileSync, mkdirSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fakeServer, type Reply } from "../../../../packages/e2e/test/fake-server.ts";
import { fakeConfig, type Live, sandbox } from "../../../../packages/e2e/test/harness.ts";

const here = import.meta.dir;

type Answer = { decision: "allow" | "always" | "deny"; feedback?: string };

interface Scenario {
  prompt: string;
  replies: Reply[];
  config?: Record<string, unknown>;
  args?: string[];
  /** Files the project starts with. */
  files?: Record<string, string>;
  /** Called for every event before it is answered. */
  on?(e: any, run: Live): void;
  /** How to answer an approval; "hold" leaves it waiting until the next one arrives, "none" never answers. */
  answer?(e: any): Answer | "hold" | "none";
  /** Also keep the session log, under this name. */
  session?: string;
}

const CLIENT = `export async function request(url: string) {
  let attempt = 0;
  while (true) {
    try {
      return await fetch(url);
    } catch (err) {
      attempt += 1;
      // retry twice, 500 ms apart
      if (attempt > 2) throw err;
      await new Promise((r) => setTimeout(r, 500));
    }
  }
}
`;
const RETRY = `export function backoff(attempt: number, base = 200): number {
  return base * 2 ** attempt;
}
`;
const read = (path: string): Reply => ({ toolCalls: [{ name: "read", args: { path } }] });
const bash = (command: string): Reply => ({ toolCalls: [{ name: "bash", args: { command } }] });
const todos = (...status: string[]): Reply => ({
  toolCalls: [
    {
      name: "todo_write",
      args: { todos: ["遅延の計算を切り出す", "client.tsを繋ぎ替える", "動作を確認する"].map((content, i) => ({ content, status: status[i] })) },
    },
  ],
});
const FAILING = "sh -c 'echo boom; exit 1'";

const scenarios: Record<string, Scenario> = {
  run: {
    prompt: "ファイルを作って",
    config: { permissions: { mode: "ask" } },
    replies: [
      { reasoning: "まずファイルを作る。", text: "ファイルを作ります。", toolCalls: [{ name: "bash", args: { command: "echo hi > made.txt" } }] },
      { text: "**完了**しました。`made.txt` を作成しています。" },
    ],
    on(e, run) {
      if (e.type === "approval_request") run.send("not json");
    },
    session: "session.jsonl",
  },
  work: {
    prompt: "再試行を指数バックオフにして",
    files: { "src/client.ts": CLIENT, "README.md": "# demo\n\nRequests retry twice with a fixed 500 ms delay.\n" },
    replies: [
      { reasoning: "まず現在の実装を読む。", text: "現在の再試行処理を確認します。", ...read("src/client.ts") },
      bash("grep -n retry src/client.ts README.md"),
      // A name and an argument no tool has: tool-repair mends both.
      { toolCalls: [{ name: "read_file", args: { file: "README.md" } }] },
      todos("in_progress", "pending", "pending"),
      {
        text: "遅延の計算を`src/retry.ts`に切り出します。",
        toolCalls: [
          { name: "write", args: { path: "src/retry.ts", content: RETRY } },
          {
            name: "edit",
            args: {
              path: "src/client.ts",
              old_string: "      await new Promise((r) => setTimeout(r, 500));",
              new_string: "      await new Promise((r) => setTimeout(r, backoff(attempt)));",
            },
          },
        ],
      },
      todos("completed", "completed", "in_progress"),
      // Two calls in one reply: both wait for an answer at the same time.
      { toolCalls: [{ name: "bash", args: { command: "echo one" } }, { name: "bash", args: { command: "echo two" } }] },
      bash(FAILING),
      // The same command again: "always" answered it for the rest of this run.
      bash(FAILING),
      { toolCalls: [{ name: "task", args: { description: "構成を調べる", prompt: "src を読んで構成を報告して" } }] },
      // The subagent's two requests.
      read("src/retry.ts"),
      { text: "`src/`には`client.ts`と`retry.ts`があります。" },
      todos("completed", "completed", "completed"),
      { text: "**完了**しました。\n\n- `src/retry.ts`を新設\n- `src/client.ts`の待ち時間を`backoff()`に置き換え" },
    ],
    on(e, run) {
      // Typed while the run is busy: the model sees it at its next turn.
      if (e.type === "tool_start" && e.call.name === "bash" && e.call.input.command.startsWith("grep")) run.send({ type: "message", text: "テストも足して" });
    },
    answer(e) {
      const command = e.args?.command;
      if (command === "echo one") return "hold";
      if (command === "echo two") return { decision: "deny", feedback: "それは不要" };
      if (command === FAILING) return { decision: "always" };
      return { decision: "allow" };
    },
    session: "work-session.jsonl",
  },
  stop: {
    prompt: "しばらく待って",
    replies: [{ text: "30秒待ちます。", ...bash("sleep 30") }],
    on(e, run) {
      if (e.type === "tool_start") run.send({ type: "abort" });
    },
  },
  cancel: {
    prompt: "ファイルを消して",
    replies: [bash("rm -f made.txt")],
    on(e, run) {
      if (e.type === "approval_request") run.send({ type: "abort" });
    },
    answer: () => "none",
  },
  error: { prompt: "こんにちは", replies: [{ status: 500 }] },
  dropped: { prompt: "こんにちは", replies: [{ text: "途中まで書いたところで", cut: true }, { text: "**やり直し**ました。" }] },
  turns: { prompt: "読んで", files: { "a.txt": "a\n" }, args: ["--max-turns", "1"], replies: [read("a.txt"), read("a.txt")] },
  loop: { prompt: "読んで", files: { "a.txt": "a\n" }, replies: Array.from({ length: 16 }, () => read("a.txt")) },
};

async function generate(name: string, sc: Scenario) {
  const server = fakeServer(sc.replies);
  const box = sandbox(fakeConfig(server.url, sc.config));
  try {
    for (const [path, content] of Object.entries(sc.files ?? {})) {
      mkdirSync(dirname(join(box.project, path)), { recursive: true });
      writeFileSync(join(box.project, path), content);
    }
    const run = box.start(["-p", sc.prompt, "--output", "jsonl", "--control", "stdio", ...(sc.args ?? [])]);
    const lines: unknown[] = [];
    const held: string[] = [];
    const release = () => {
      for (const id of held.splice(0)) run.send({ type: "approval", id, decision: "allow" });
    };
    let reading: Promise<any> | undefined;
    for (;;) {
      reading ??= run.next();
      // A held approval is answered when the next one arrives, or after a moment if none does.
      const got = await Promise.race([reading.then((value) => ({ value })), Bun.sleep(held.length ? 1500 : 60_000).then(() => undefined)]);
      if (!got) {
        if (!held.length) throw new Error(`${name}: no event for a minute`);
        release();
        continue;
      }
      reading = undefined;
      const e = got.value;
      if (e === undefined) break;
      lines.push(e);
      sc.on?.(e, run);
      if (e.type === "approval_request") {
        const a = sc.answer?.(e) ?? { decision: "allow" };
        if (a === "hold") held.push(e.id);
        else if (a !== "none") {
          run.send({ type: "approval", id: e.id, ...a });
          release();
        }
      }
      if (e.type === "agent_end") run.end();
    }
    await run.exited;
    writeFileSync(join(here, `${name}.jsonl`), lines.map((l) => JSON.stringify(l)).join("\n") + "\n");
    let wrote = `${name}.jsonl (${lines.length} events, ${server.left()} replies unused)`;
    if (sc.session) {
      const sessions = join(box.home, "sessions");
      const dir = join(sessions, readdirSync(sessions)[0]!);
      const log = readdirSync(dir).find((f) => f.endsWith(".jsonl"))!;
      copyFileSync(join(dir, log), join(here, sc.session));
      wrote += ` and ${sc.session}`;
    }
    console.log(`wrote ${wrote}`);
  } finally {
    box.cleanup();
    server.stop();
  }
}

const wanted = process.argv.slice(2);
for (const name of wanted.length ? wanted : Object.keys(scenarios)) {
  const sc = scenarios[name];
  if (!sc) throw new Error(`no scenario "${name}" (known: ${Object.keys(scenarios).join(", ")})`);
  await generate(name, sc);
}
