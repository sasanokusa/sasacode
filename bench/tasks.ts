// Realistic coding tasks run against a fresh copy of this repository.

export interface Task {
  id: string;
  kind: "explain" | "bugfix" | "feature" | "refactor" | "plugin" | "multi-turn";
  /** First prompt, then follow-ups sent to the same session with `-r <id> -p`. */
  prompts: string[];
  /** Break something before the run (a file edit inside the workspace). */
  setup?: { file: string; from: string; to: string };
  /** Shell command run in the workspace afterwards; exit 0 = success. Omitted = not graded. */
  check?: string;
}

export const TASKS: Task[] = [
  {
    id: "explain-tool-pipeline",
    kind: "explain",
    prompts: [
      "packages/agent/src のコードを読んで、モデルの応答を受け取ってからツールが実行され結果が返るまでの流れを、関係する関数名とフックの順序を挙げて説明して",
    ],
  },
  {
    id: "fix-edit-count",
    kind: "bugfix",
    setup: {
      file: "packages/tools/src/edit.ts",
      from: "const count = before.split(args.old_string).length - 1;",
      to: "const count = before.split(args.old_string).length - 2;",
    },
    prompts: ["bun test packages/tools が落ちています。原因を調べて直してください。"],
    check: "bun test packages/tools",
  },
  {
    id: "fix-permission-redirect",
    kind: "bugfix",
    setup: {
      file: "packages/agent/src/permission.ts",
      from: "const SUBSTITUTION = /\\$\\(|`|<\\(|>/;",
      to: "const SUBSTITUTION = /\\$\\(|`|<\\(/;",
    },
    prompts: ["bun test packages/agent/test/permission.test.ts が失敗します。セキュリティ上の意図を確認したうえで直してください。"],
    check: "bun test packages/agent/test/permission.test.ts",
  },
  {
    id: "feature-read-tail",
    kind: "feature",
    prompts: [
      "read ツールに tail 引数（ファイル末尾から n 行を読む。offset とは同時に使えない）を追加し、packages/tools/test にテストを追加して。bun test packages/tools と bun run typecheck が通ること。",
    ],
    check: "bun test packages/tools && bun run typecheck && grep -q tail packages/tools/src/read.ts",
  },
  {
    id: "feature-version-command",
    kind: "feature",
    prompts: ["TUI に /version コマンドを追加して、sasacode のバージョンを表示するようにして。bun run typecheck が通ること。"],
    check: "bun run typecheck && grep -q '\"version\"' packages/tui/src/app.ts",
  },
  {
    id: "refactor-html",
    kind: "refactor",
    prompts: [
      "packages/bundled/src/web-fetch.ts の htmlToText を packages/bundled/src/html.ts に切り出して。既存の export や import は壊さないこと。bun test packages/bundled と bun run typecheck が通ること。",
    ],
    check: "test -f packages/bundled/src/html.ts && bun test packages/bundled && bun run typecheck",
  },
  {
    id: "plugin-wordcount",
    kind: "plugin",
    prompts: [
      "tool-authoring の Skill を読んで、このプロジェクト用に .sasacode/plugins/wordcount.ts として、指定したファイルの行数・単語数・文字数を返す wc ツールのプラグインを作って。",
    ],
    check:
      "test -f .sasacode/plugins/wordcount.ts && bun -e 'const m = await import(\"./.sasacode/plugins/wordcount.ts\"); const tools = []; await m.default({ registerTool: (t) => tools.push(t), registerCommand() {}, on() {}, ready() {}, permissions: { addRules() {} }, ui: { interactive: false, notify() {}, setStatus() {}, registerToolRenderer() {} }, session: {}, agent: {}, settings: {}, cwd: process.cwd() }); if (!tools.length) process.exit(1)'",
  },
  {
    id: "review-then-fix",
    kind: "multi-turn",
    prompts: [
      "packages/cli/src/catalog.ts をレビューして、問題になりうる点を重要な順に3つ挙げて（まだ直さないで）",
      "1つ目を直して",
      "直した点に対するテストを packages/cli/test に追加して、bun test packages/cli が通るのを確認して",
    ],
    check: "bun test packages/cli && bun run typecheck",
  },
];
