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
  "apiVersion": "^1.3.0",
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

## API リファレンス

公開 API は `@sasacode/plugin-api` の `PluginAPI` で、現在の版は 1.3.0。変更の履歴は[最後の表](#api-の版)にある。

### PluginAPI

| API | できること |
| --- | --- |
| `version` / `name` / `cwd` | ホストの API の版 / このプラグインの名前 / 作業ディレクトリ |
| `settings` | 設定ファイルの `plugins.settings.<名前>` |
| `registerTool(def)` | ツールを追加する。同名を登録すると置き換わる（組み込みの `read` なども上書きできる） |
| `registerCommand(def)` | スラッシュコマンドを追加する |
| `registerProvider(name, config, impl?)` | プロバイダーを追加する。`impl` を渡すと、新しい API 形式も足せる |
| `on(hook, handler)` | フックを登録する（[フック](#フック)） |
| `ready(promise)` | 起動時の非同期処理（サーバーへの接続など）をホストに知らせる。ヘッドレス実行はこれを待ってから最初のリクエストを送り、TUI は待たない（1.1.0〜） |
| `permissions.addRules({allow, ask, deny})` | 権限ルールを足す |
| `ui.interactive` | TUI なら true。ヘッドレスでは false で、`confirm` は false、`select` は undefined を返す |
| `ui.notify(msg, level?)` / `ui.confirm(title, msg?)` / `ui.select(title, options)` | 通知 / 確認 / 選択 |
| `ui.setStatus(key, text)` | フッターのステータス行に出す（`undefined` で消す） |
| `ui.registerToolRenderer(tool, fn)` | ツール結果の表示を差し替える。`fn(call, result, {expanded, width})` が行の配列を返す |
| `session.id` | セッション ID（`--no-session` なら undefined） |
| `session.append(kind, data)` / `session.entries(kind)` | セッションに独自の状態を保存し、再開時に読み戻す |
| `session.messages()` / `session.replaceMessages(msgs)` | 会話を読む / 差し替える（圧縮など。差し替えもセッションに記録される） |
| `session.inject(text, "next" \| "now")` | ユーザーメッセージを差し込む。`next` は次のターンか次の入力で、`now` は実行中でなければすぐ実行する |
| `agent.model()` / `agent.tools()` | 現在のモデル / 登録済みのツール |
| `agent.run({prompt, systemPrompt?, tools?, excludeTools?, model?, signal?, onProgress?})` | 独立した履歴でサブループを実行し、`{text, messages, cause}` を返す（サブエージェント） |
| `agent.complete({system, messages, model?, maxTokens?, signal?})` | ツールなしで1回だけモデルを呼び、本文を返す |
| `log(...args)` | `SASACODE_DEBUG` が設定されているときだけ stderr に出す |

### ツールの定義（`registerTool`）

| フィールド | 意味 |
| --- | --- |
| `name` / `description` / `parameters` | 名前、説明、引数の JSON Schema。引数は実行前にスキーマで検証される |
| `kind` | `read` / `edit` / `exec` / `other`。権限の判定に使う。既定のモード `edits` で自動で許可されるのは、作業ディレクトリ内の read / edit だけ |
| `paths(args, cwd)` | 触るファイル。パスのルール（`edit(src/**)`）の照合と、作業ディレクトリの内側かどうかの判定に使う |
| `matchTarget(args)` | パターンのルール（`bash(git *)`）で照合する文字列 |
| `concurrent` | 同じターンの、ほかの concurrent なツールと並行して実行してよい |
| `alwaysLoad` | ツールの遅延ロードが有効でも、常に定義を送る |
| `summary(args)` | UI に出す1行の要約 |
| `execute(args, ctx)` | 本体。`ctx` は `{cwd, signal, onUpdate(text)}`。`{content, isError?, details?}` を返す（`details` は表示用で、モデルには送らない） |

ツールが例外を投げると、エラーの tool_result としてモデルに返る。ループは止まらない。

### コマンドの定義（`registerCommand`）

| フィールド | 意味 |
| --- | --- |
| `name` / `description` / `argumentHint` | `/name` で呼ぶ。説明と引数のヒントは補完の一覧に出る |
| `run({args})` | 本体。`args` は名前より後ろの文字列 |
| `complete(prefix)` | 引数の候補（`{value, label, description?}` の配列）を返すと、tab で補完できる（1.3.0〜） |

### フック

`api.on(フック名, handler)` で登録する。

- ハンドラは登録した順に直列で実行する。前のハンドラが返した値は、後のハンドラが受け取る値に反映される。
- 例外を投げたハンドラは、プラグインのエラーとして表示して飛ばす。ループは止まらない。

1回の応答では、次の順に呼ばれる。

```
before_request → stream_delta（生成中、差分ごと） → assistant_message
  → ツール呼び出しごとに: tool_call_raw → 検索・検証 → tool_call → 権限判定 → 実行 → tool_result
  → turn_end
```

| フック | いつ | 受け取るもの | 返せるもの |
| --- | --- | --- | --- |
| `session_start` | セッションを始めた・再開したとき | `{sessionId?, resumed}` | なし（状態の復元に使う） |
| `session_end` | `/clear`・`/resume`・終了の前 | `{sessionId?}` | なし（後片付けに使う） |
| `user_prompt` | ユーザー入力をモデルに送る前 | `{content}` | `{content}` で書き換える。`{handled: true}` にするとモデルに渡さない |
| `system_prompt` | リクエストごと | `{prompt}` | `{prompt}` で書き換える（普通は追記する） |
| `before_request` | モデルを呼ぶ直前 | `{messages, model, sampling}` | `{messages}` でこのリクエストのメッセージを変える（保存される履歴は変わらない）。`{sampling}` で temperature・topP・frequencyPenalty・presencePenalty・`extraBody`（プロバイダー固有の値）を変える（1.2.0〜） |
| `stream_delta` | 生成中、テキスト・thinking・ツール引数の差分ごと | `{kind, index, delta, text, message}`（`text` はそのブロックの累積） | `{stop: 理由}` で生成を止める。**同期のみ**で、Promise を返すとエラーとして無視される（1.2.0〜） |
| `assistant_message` | 応答が確定し、保存する前 | `{message, stopped?}`（`stopped` は途中で止めたプラグインと理由） | `{message}` で書き換える。`{retry: true}` で捨てて取り直す（1回の応答につき最大2回）。`{inject}` でユーザーメッセージを足して続ける（1.2.0〜） |
| `tool_call_raw` | ツールの検索と引数の検証の前 | `{call, name, input, rawInput?, tools, stopReason}`（`rawInput` は JSON として壊れていたときの生の文字列） | `{name, input, note}` で修復する。`note` は tool_result の先頭でモデルに伝わる。履歴には修復後の呼び出しを残し、元の出力はセッションに `tool_repair` として残る（署名付きの thinking を含む応答は、履歴を書き換えずに実行時だけ直す）。`max_tokens` で途切れた呼び出しでは呼ばれない（1.2.0〜） |
| `tool_call` | 検証の後、権限判定の前 | `{call, tool, args}` | `{args}` で引数を書き換える。`{decision: "allow" \| "ask" \| "deny", reason}` で判定する（複数あれば deny > ask > allow の強いほう）。プラグインの判定が置き換えるのは権限モードの既定の判定だけで、ユーザーの deny / ask ルールは常に効く |
| `tool_result` | ツールの実行後 | `{call, result}` | `{result}` で結果を書き換える・追記する |
| `turn_end` | 1ターン（応答とツールの実行）の後 | `{turn, message}` | `{inject}` でユーザーメッセージを足してループを続ける |
| `agent_end` | ループが止まったとき | `{cause}`（`done` / `aborted` / `error` / `refusal` / `context_limit` / `max_turns`） | `{inject}` で次の実行を始める |
| `context_limit` | コンテキストが上限に達したとき | `{tokens, contextWindow}` | `session.replaceMessages` で空けてから `{retry: true}` で続ける（連続2回まで） |

サブエージェント（`agent.run`）は、`system_prompt` / `before_request` / `stream_delta` / `assistant_message` / `tool_call_raw` / `tool_call` / `tool_result` のハンドラを共有する。セッション系のフック（`session_*`、`user_prompt`、`turn_end`、`agent_end`、`context_limit`）は、サブエージェントでは呼ばれない。

### API の版

| 版 | 追加されたもの |
| --- | --- |
| 1.0.0 | 最初の公開版（ツール、コマンド、プロバイダー、基本のフック、ui / session / agent） |
| 1.1.0 | `ready(promise)` |
| 1.2.0 | `stream_delta`、`assistant_message`、`tool_call_raw` の各フック。`before_request` の `sampling` |
| 1.3.0 | コマンドの `complete(prefix)`（引数の補完） |

どれも既存のプラグインを壊さない追加。`apiVersion` に `^1.0.0` と書いたプラグインは、そのまま動く。

## 信頼と安全

インプロセスプラグインは sasacode と同じ権限で動く。`~/.sasacode/plugins` のものはユーザーが自分で入れたものとして、そのまま読み込む。プロジェクトの `.sasacode/plugins` は、プロジェクト設定の信頼が必要な部分（エンドポイント、MCP サーバー、プラグインの設定など）とまとめて、起動時に1回だけ信頼の確認を出す。構成が変わると、また確認を出す。ヘッドレスでは、`--trust-project` を付けたときだけ読み込む。確認の結果は `~/.sasacode/trust.json` に保存する。

## 無効化・上書き

```json
{
  "plugins": { "disabled": ["todo", "web-fetch"], "settings": { "compaction": { "threshold": 0.7 } } },
  "tools": { "disabled": ["bash"] }
}
```

同梱プラグインの名前は `tool-repair`、`repetition-guard`、`loop-guard`、`agents-md`、`compaction`、`subagent`、`todo`、`web-fetch`、`permission-presets`、`browsr`、`skills`、`mcp`。

プラグインを作るときは、組み込みの Skill を使うのが早い。TUI で `/skill:tool-authoring <作りたいもの>` と打つと、このリファレンスを読んだうえでモデルがプラグインを書く。
