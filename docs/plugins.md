# プラグインの書き方

sasacode のツール・コマンド・フックはすべてプラグインから足す。組み込みの read / write / edit / bash、コアのスラッシュコマンド、MCP、Skills、同梱プラグインも、ここで説明するのと同じ公開 API（`@sasacode/plugin-api`）で登録している。特権 API はない。

## 最小のプラグイン

`~/.sasacode/plugins/hello.ts`（全プロジェクト共通）または `<project>/.sasacode/plugins/hello.ts` に置くだけで読み込まれる。ビルドは不要で、TypeScript をそのまま実行する。

```ts
import { text, type Plugin } from "@sasacode/plugin-api";

const plugin: Plugin = (api) => {
  api.registerTool({
    name: "hello",
    description: "Greet someone by name.",
    parameters: { type: "object", properties: { name: { type: "string" } }, required: ["name"] },
    kind: "read",
    execute: async ({ name }) => ({ content: [text(`Hello, ${name}!`)] }),
  });
};
export default plugin;
```

`@sasacode/plugin-api` はインストールしなくても import できる（ホストが自分の実装を渡す）。型補完が欲しければ devDependency として入れる。

## パッケージにする

複数のファイルや Skills・MCP サーバーをまとめて配るときは、ディレクトリに `plugin.json` を置くか、`package.json` に `sasacode` フィールドを書く。

```json
{
  "name": "my-plugin",
  "version": "1.0.0",
  "apiVersion": "^1.0.0",
  "extensions": ["src/index.ts"],
  "skills": "skills",
  "mcpServers": {
    "github": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" } }
  }
}
```

- `apiVersion` がホストの API（現在 1.3.0）と互換でなければ、警告を出して読み込まない。
- `skills` は `SKILL.md` を含むフォルダが並ぶディレクトリ。
- `mcpServers` は設定ファイルの `mcpServers` と同じ形式。`${VAR}` は環境変数から展開される。

インストール：

```bash
sasacode plugin install my-sasacode-plugin            # npm
sasacode plugin install https://github.com/you/plugin  # git
sasacode plugin install <spec> --project              # .sasacode/plugins へ
sasacode plugin list
sasacode plugin remove <name>
```

## API

| API | できること |
| --- | --- |
| `registerTool(def)` | ツールを追加する。同名を登録すると置き換わる（組み込みの `read` なども上書きできる） |
| `registerCommand(def)` | スラッシュコマンドを追加する。`complete(prefix)` を書くと、引数を tab で補完できる（1.3.0〜） |
| `registerProvider(name, config, impl?)` | プロバイダーを追加する。`impl` を渡すと新しい API 形式も足せる |
| `on(hook, handler)` | ライフサイクルフック（下表） |
| `permissions.addRules({allow, ask, deny})` | 権限ルールを足す |
| `ui.notify / confirm / select` | 通知・確認・選択。ヘッドレスでは `confirm` は false、`select` は undefined |
| `ui.setStatus(key, text)` | フッターのステータス行に表示する |
| `ui.registerToolRenderer(tool, fn)` | ツール結果の表示を差し替える |
| `session.append(kind, data)` / `entries(kind)` | セッションに独自の状態を保存し、再開時に読み戻す |
| `session.replaceMessages(msgs)` | 会話を差し替える（圧縮など）。セッションにも記録される |
| `session.inject(text, "next" \| "now")` | ユーザーメッセージを差し込む |
| `agent.run({prompt, tools?, excludeTools?, model?})` | 独立した履歴でサブループを実行する（サブエージェント） |
| `agent.complete({system, messages})` | ツールなしで1回だけモデルを呼ぶ |
| `settings` | 設定ファイルの `plugins.settings.<名前>` |
| `ready(promise)` | 起動時の非同期処理をホストに知らせる。ヘッドレス実行は、これを待ってから最初のリクエストを送る（1.1.0〜） |

### ツールの定義

| フィールド | 意味 |
| --- | --- |
| `kind` | `read` / `edit` / `exec` / `other`。権限判定に使う。既定モード `edits` で自動許可されるのは、作業ディレクトリ内の read / edit だけ |
| `paths(args, cwd)` | 触るファイル。パスのルール（`edit(src/**)`）の照合と、作業ディレクトリ内かどうかの判定に使う |
| `matchTarget(args)` | パターンのルール（`bash(git *)`）で照合する文字列 |
| `concurrent` | 同じターンの他の concurrent なツールと並行して実行してよい |
| `alwaysLoad` | ツール遅延ロードが有効でも、常に定義を送る |
| `summary(args)` | UI に出す1行の要約 |

ツールの例外は、エラーの tool_result としてモデルに返る。ループは止まらない。

### フック

ハンドラは登録順に直列で実行する。例外は「プラグインのエラー」として表示し、そのハンドラだけ飛ばす。

| フック | 返せるもの |
| --- | --- |
| `session_start` / `session_end` | なし（状態の復元・後片付けに使う） |
| `user_prompt` | `{content}` で入力を書き換える。`{handled: true}` にするとモデルに渡さない |
| `system_prompt` | `{prompt}` でシステムプロンプトを書き換える（追記する） |
| `before_request` | `{messages}` でこのリクエストで送るメッセージ列を変える（保存される履歴は変わらない） |
| `tool_call` | `{args}` で引数を書き換える。`{decision: "allow" \| "ask" \| "deny", reason}` で権限判定をする |
| `tool_result` | `{result}` で結果を書き換える・追記する |
| `turn_end` / `agent_end` | `{inject}` でユーザーメッセージを足してループを続ける |
| `context_limit` | コンテキストを空けてから（`session.replaceMessages`）、`{retry: true}` で続行する |
| `stream_delta` | 生成中の差分ごと（同期のみ）。`{stop: 理由}` で生成を止める（1.2.0〜） |
| `assistant_message` | 確定した応答。`{message}` で書き換え、`{retry: true}` で再生成（最大2回）、`{inject}` で続行する。途中で止めた応答には `stopped: {plugin, reason}` が付く（1.2.0〜） |
| `tool_call_raw` | 検証前のツール呼び出し（`name`、`input`、壊れた JSON なら `rawInput`、`tools`）。`{name, input, note}` で修復する。`note` は tool_result の先頭でモデルに伝わる（1.2.0〜） |

1回の応答での順序は `before_request` → `stream_delta` … → `assistant_message` → `tool_call_raw` → 検証 → `tool_call` → 権限判定 → 実行 → `tool_result`。`before_request` は `{sampling}` で temperature や penalty、プロバイダー固有の値（`extraBody`）も変えられる。

サブエージェントでは、`tool_call_raw` / `tool_call` / `tool_result` / `system_prompt` / `before_request` / `stream_delta` / `assistant_message` のハンドラは共有される。セッション系のフック（`session_*`、`user_prompt`、`turn_end`、`agent_end`、`context_limit`）は呼ばれない。

## 信頼と安全

インプロセスプラグインは sasacode と同じ権限で動く。`~/.sasacode/plugins` のものはユーザーが自分で入れたものとして、そのまま読み込む。プロジェクトの `.sasacode/plugins` と、プロジェクト設定に書かれた MCP サーバーは、初回に信頼確認を出す。構成が変わると、また確認を出す。ヘッドレスでは、`--trust-project` を付けたときだけ読み込む。確認の結果は `~/.sasacode/trust.json` に保存する。

## 無効化・上書き

```json
{
  "plugins": { "disabled": ["todo", "web-fetch"], "settings": { "compaction": { "threshold": 0.7 } } },
  "tools": { "disabled": ["bash"] }
}
```

同梱プラグインの名前は `tool-repair`、`repetition-guard`、`agents-md`、`compaction`、`subagent`、`todo`、`web-fetch`、`permission-presets`、`browsr`、`skills`、`mcp`。

プラグインを作るときは、組み込みの Skill を使うのが早い。TUI で `/skill:tool-authoring <作りたいもの>` と打つと、このリファレンスを読んだうえでモデルがプラグインを書く。
