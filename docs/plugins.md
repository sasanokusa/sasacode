# プラグインの書き方

sasacodeのツール・コマンド・フックはすべてプラグインから足す。組み込みのread / write / edit / bash、コアのスラッシュコマンド、MCP、Skills、同梱プラグインも、ここで説明するのと同じ公開API（`@sasacode/plugin-api`）で登録している。特権APIはない。

## 最小のプラグイン

`~/.sasacode/plugins/hello.ts`（全プロジェクト共通）または`<project>/.sasacode/plugins/hello.ts`に置くだけで読み込まれる。ビルドは不要で、TypeScriptをそのまま実行する。

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

`@sasacode/plugin-api`はインストールしなくてもimportできる（ホストが自分の実装を渡す）。型補完が欲しければ、npmの [`@sasacode/plugin-api`](https://www.npmjs.com/package/@sasacode/plugin-api) をdevDependencyとして入れる。パッケージの版はプラグインAPIの版と同じ。

```bash
npm install --save-dev @sasacode/plugin-api
```

npmのパッケージは`bun run pack:plugin-api`で`dist/plugin-api`に作る（型と小さな関数だけで、プロバイダーのSDKは含まない）。APIの版を上げたら作り直して公開する。

## パッケージにする

複数のファイルやSkills・MCPサーバーをまとめて配るときは、ディレクトリに`plugin.json`を置くか、`package.json`に`sasacode`フィールドを書く。

```json
{
  "name": "my-plugin",
  "version": "1.0.0",
  "apiVersion": "^1.4.0",
  "extensions": ["src/index.ts"],
  "skills": "skills",
  "mcpServers": {
    "github": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" } }
  }
}
```

- `apiVersion`がホストのAPI（現在1.11.0）と互換でなければ、警告を出して読み込まない。
- `skills`は`SKILL.md`を含むフォルダが並ぶディレクトリ。
- `mcpServers`は設定ファイルの`mcpServers`と同じ形式。`${VAR}`は環境変数から展開される。

## 探す・入れる

```bash
sasacode plugin search [語]                         # sasanokusa.com のおすすめ一覧と npm から探す（語なしで全部）
sasacode plugin install sasacode-plugin-foo         # npm
sasacode plugin install https://github.com/you/plugin  # git
sasacode plugin install ./my-plugin                 # 手元のパッケージ（公開前の確認に）
sasacode plugin install <spec> --project            # .sasacode/plugins へ
sasacode plugin update [名前]                        # npm は最新版へ、git は差分を見せてから更新
sasacode plugin list
sasacode plugin remove <名前>
```

- `search`は、sasanokusa.comに置いた小さな一覧（`plugins.json`、数KB）と、npmのキーワード`sasacode-plugin`の検索を合わせて出す。一覧に載っているものには ★ が付く。どちらかに届かなくても、もう一方の結果は出る。本体はnpmやGitHubから直接取るので、sasanokusa.comを通るのは一覧だけ。
- `install`は、入れる前に中身を見せて確認を取る。npmなら版、公開者、公開日、ファイル数と大きさ、依存パッケージ、installスクリプトの有無（実行はしない）。gitならURLを見せ、入れた後にコミットを出す。端末がないとき（スクリプトなど）は`--yes`が要る。
- sasacodeのプラグインでないもの（`package.json`に`sasacode`フィールドがなく、`plugin.json`もない）は入れない。npmは取得する前、gitはcloneした後に確かめる（cloneは消す）。プラグインは`sasacode-plugin-`を省いた名前で呼ばれることが多く、省いた名前のnpmパッケージは大抵まったく違うものなので、`sasacode plugin install jev-guard`は断ったうえで`sasacode-plugin-jev-guard`を案内する。以前に入ってしまったものは`plugin list`が知らせる。
- パッケージの取得にはsasacodeに内蔵のBunを使うので、Bunやnpmを入れていなくても動く。
- `update`は、gitのプラグインなら新しいコミットの一覧と変更の規模（`git diff --stat`）を見せてから更新する。

## 公開する

1ファイルのプラグインは、そのままnpmに出せる。

```bash
sasacode plugin publish my-tool.ts --license MIT --dry-run   # 中身の確認
sasacode plugin publish my-tool.ts --license MIT             # npm publish（npm へのログインが必要）
```

- `~/.sasacode/publish/<パッケージ名>/`にnpmのパッケージを組み立てて、`npm publish --access public`を実行する。npmの2段階認証は、その場でワンタイムパスワードを聞かれる（`--otp <コード>`で先に渡してもよい）。パッケージ名は既定で`sasacode-plugin-<ファイル名>`（`--name @you/my-tool`で変えられる）。
- `package.json`には、`sasacode`フィールド（マニフェスト）、検索用のキーワード`sasacode-plugin`、ピア依存の`@sasacode/plugin-api`を入れる。READMEはファイル先頭のコメントから作る。
- 版は、npmに出ている最新版の次のパッチ（初回は0.1.0）。`--version`で指定できる。
- 説明文は、ファイルの最初のコメントから取る（`--description`で指定できる）。
- `apiVersion`は、使っている機能から決める（`before_request.fetch`なら ^1.11.0、`ui.input`・`ui.selectMany`なら ^1.10.0、`callTool`・`showText`・`defineSettings`なら ^1.9.0、フックの`ctx`や`session_end`の`reason`なら ^1.8.0、`permission`フックや`softDeny`なら ^1.7.0、`permissionsAs`なら ^1.6.0、など）。`--api`で指定できる。
- 同じフォルダのほかのファイルをimportしていると、それは含まれない（注意を出す）。そういうプラグインは、`package.json`に`sasacode`フィールドを書いたディレクトリごと`sasacode plugin publish <ディレクトリ>`で出す（キーワードがなければ足す）。
- ライセンスを付けないと、ほかの人は再利用できない。`--license MIT`のように付ける。

sasanokusa.comのおすすめ一覧に載せたいときは、[リポジトリ](https://github.com/sasanokusa/sasacode)の`site/plugins.json`に1件足すpull requestを送る。

```json
{ "name": "my-tool", "source": "sasacode-plugin-my-tool", "description": "何をするか", "author": "you" }
```

`source`は`sasacode plugin install`に渡すもの（npmのパッケージ名かgitのURL）。

## API リファレンス

公開APIは`@sasacode/plugin-api`の`PluginAPI`で、現在の版は1.11.0。変更の履歴は[最後の表](#api-の版)にある。

### PluginAPI

| API | できること |
| --- | --- |
| `version` / `name` / `cwd` | ホストのAPIの版 / このプラグインの名前 / 作業ディレクトリ |
| `settings` | 設定ファイルの`plugins.settings.<名前>` |
| `defineSettings({schema, defaults?})` | 設定を既定値とマージし、JSON Schemaの対応範囲で検証する。型変換せず、不正なら例外（1.9.0〜） |
| `registerTool(def)` | ツールを追加する。同名を登録すると置き換わる（組み込みの`read`なども上書きできる） |
| `unregisterTool(name)` | このプラグインが登録したツールを外す。ほかのプラグインのツールには触れない（1.8.0〜） |
| `registerCommand(def)` | スラッシュコマンドを追加する |
| `registerProvider(name, config, impl?)` | プロバイダーを追加する。`impl`を渡すと、新しいAPI形式も足せる |
| `on(hook, handler)` | フックを登録する（[フック](#フック)） |
| `ready(promise)` | 起動時の非同期処理（サーバーへの接続など）をホストに知らせる。ヘッドレス実行はこれを待ってから最初のリクエストを送り、TUIは待たない（1.1.0〜） |
| `permissions.addRules({allow, ask, deny, softDeny})` | 権限ルールを足す。`softDeny`はdenyと同じく拒否するが、`permission`フックが「ユーザーに確認」まで下げられる（1.7.0〜） |
| `ui.interactive` | TUIならtrue。ヘッドレスではfalseで、`confirm`はfalse、`select`・`input`・`selectMany`はundefinedを返す |
| `ui.lang` | ユーザーが読む言語（`"ja"` / `"en"`）。通知やコマンドの説明をこの言語で出す（1.8.0〜） |
| `ui.notify(msg, level?)` / `ui.confirm(title, msg?)` / `ui.select(title, options)` | 通知 / 確認 / 選択 |
| `ui.input(title, {message?, placeholder?, initial?})` | 1行の自由入力。入力された文字列（空文字もありうる）を返し、escならundefined（1.10.0〜） |
| `ui.selectMany(title, options, {selected?})` | 複数選択。spaceで切り替え、enterで決定。選ばれた値を選択肢の順で返し（0個もありうる）、escならundefined。渡した選択肢にない値は返さない（1.10.0〜） |
| `ui.showText({title, text, format?})` | 長文の閲覧パネル。スクロール・検索・ユーザー操作によるコピー。閉じるまで待つ。ヘッドレスはstderrに表示（1.9.0〜） |
| `ui.setStatus(key, text)` | フッターのステータス行に出す（`undefined`で消す） |
| `ui.registerToolRenderer(tool, fn)` | ツール結果の表示を差し替える。`fn(call, result, {expanded, width})`が行の配列を返す |
| `session.id` | セッションID（`--no-session`ならundefined） |
| `session.append(kind, data)` / `session.entries(kind)` | セッションに独自の状態を保存し、再開時に読み戻す |
| `session.messages()` / `session.replaceMessages(msgs)` | 会話を読む / 差し替える（圧縮など。差し替えもセッションに記録される） |
| `session.inject(text, "next" \| "now")` | ユーザーメッセージを差し込む。`next`は次のターンか次の入力で、`now`は実行中でなければすぐ実行する |
| `agent.model()` / `agent.tools()` | 現在のモデル / 登録済みのツール |
| `agent.run({prompt, systemPrompt?, tools?, excludeTools?, model?, signal?, onProgress?, onStart?})` | 独立した履歴でサブループを実行し、`{text, messages, cause, error?}`を返す（サブエージェント）。`onStart`は実行中のループへの`send(text)`（次のターンで届く）と`stop()`を受け取る。`error`は`cause`が`"error"`のときの理由（1.8.0〜） |
| `agent.complete({system, messages, model?, maxTokens?, signal?})` | ツールなしで1回だけモデルを呼び、本文を返す |
| `log(...args)` | `SASACODE_DEBUG`が設定されているときだけstderrに出す |

`api`のほかに、`@sasacode/plugin-api`から関数として使えるもの（1.8.0〜）：`sasacodeHome()`（`$SASACODE_HOME`か`~/.sasacode`）、`childEnv(extra?)`（コマンドを実行するときの環境変数。`~/.sasacode/.env`から読んだキーを除く）、`saveOutput(prefix, text)`（長い出力を`~/.sasacode/tmp`に本人だけが読める権限で保存し、パスを返す。1週間より古いものは消す）。コマンドを実行するプラグインは`process.env`ではなく`childEnv()`を渡す。

### ツールの定義（`registerTool`）

| フィールド | 意味 |
| --- | --- |
| `name` / `description` / `parameters` | 名前、説明、引数のJSON Schema。引数は実行前にスキーマで検証される |
| `kind` | `read` / `edit` / `exec` / `other`。権限の判定に使う。既定のモード`edits`で自動で許可されるのは、作業ディレクトリ内のread / editだけ |
| `paths(args, cwd)` | 触るファイル。パスのルール（`edit(src/**)`）の照合と、作業ディレクトリの内側かどうかの判定に使う。絶対パスで返すのが基本で、相対パスは作業ディレクトリから解決される |
| `matchTarget(args)` | パターンのルール（`bash(git *)`）で照合する文字列 |
| `permissionsAs` | 別のツール名のルールも当てる（1.6.0〜）。シェルのコマンドを別の方法で動かすツール（バックグラウンド実行、リモート実行など）に`"bash"`と書けば、利用者の`bash(sudo *)`などのdeny / ask / allowルールと`guard`プリセットが、このツールの`matchTarget`に対してそのまま適用される。自分の名前のルールも引き続き適用される |
| `concurrent` | 同じターンの、ほかのconcurrentなツールと並行して実行してよい |
| `alwaysLoad` | ツールの遅延ロードが有効でも、常に定義を送る |
| `summary(args)` | UIに出す1行の要約 |
| `execute(args, ctx)` | 本体。`ctx`は`{cwd, signal, onUpdate(text), callTool(name, args)}`（`callTool`は1.9.0〜）。`{content, isError?, details?}`を返す（`details`は表示用で、モデルには送らない） |

ツールが例外を投げると、エラーのtool_resultとしてモデルに返る。ループは止まらない。

### 別のツールを呼ぶ（1.9.0〜）

`execute`の`ctx.callTool(name, args)`は、現在のエージェントに登録されているツールを通常の実行経路で呼び、`ToolResult`（`details`を含む）を返す。たとえば名前付き検査を作るプラグインは、プロセス実行を実装し直さず`bash`を利用できる。

```ts
execute: async (_args, ctx) => {
  if (!ctx.callTool) throw new Error("plugin API 1.9+ is required");
  return await ctx.callTool("bash", { command: "bun run typecheck", timeout: 120 });
}
```

- 子ツールは、引数検証 → `tool_call` → 書き換え後の再検証 → 権限ルール・`permission`・必要に応じユーザー確認 → 実行 → `tool_result`を通る。構造化済みの呼び出しなので、モデル出力の修復用`tool_call_raw`は通らない。
- 親への許可は子への許可にならない。親と子の両方に確認が必要なら、それぞれ確認する。拒否、未登録・無効化・サブエージェント対象外のツール、不正な引数はエラー結果。ヘッドレスで必要な確認を省略することはない。
- cwd、キャンセル、サブエージェントの識別は親と同じ。確認待ちの中断も実行せずに終了する。終了後のコンテキストでは呼べない。型定義では古いホストや直接実行用のコンテキストとの互換性のため省略可能だが、1.9.0以降のホストは必ず提供する。
- 同じ`execute`からの呼び出しは順番に実行する。呼び出し元と同じツールに戻る循環と、親を含め8段を超える入れ子は拒否する。返ったPromiseは必ずawaitする。ホストも開始済みの子の完了を待ってから親を終了する。
- 子の出力は親の戻り値を通じてモデルに渡す。対応するassistant呼び出しがないtoolメッセージを履歴に追加しない。実行イベントには`parentCallId`が付き、セッションを保存している場合は`core`の`nested_tool_call` / `nested_tool_execute`（実際の引数）/ `nested_tool_result`に記録する。

### 設定を宣言する（1.9.0〜）

ツールやフックを登録する前に`api.defineSettings()`を呼ぶ。プロジェクト設定の既存の信頼確認は引き続き適用され、スキーマ検証によって権限を追加することはない。

```ts
const settings = api.defineSettings<{ timeout: number; enabled: boolean }>({
  schema: {
    type: "object",
    properties: {
      timeout: { type: "integer", minimum: 1, maximum: 3600 },
      enabled: { type: "boolean" },
    },
    required: ["timeout", "enabled"],
    additionalProperties: false,
  },
  defaults: { timeout: 120, enabled: true },
});
```

オブジェクトは再帰的にマージし、配列とnullは設定側で置き換える。返す値はコピーで、`api.settings`や`defaults`を変更しない。文字列から数値への変換はしない。不正な場合はプラグイン名と`plugins.settings.<名前>.<項目>`を含む例外になり、ホストが読み込み失敗として表示する。入力された値そのものは診断に含めない。

対応するスキーマはルートが`type: "object"`の次の範囲：`type`（型の配列も可）、`properties`、`required`、`additionalProperties`（真偽値またはスキーマ）、`items`、`enum`、`const`、`anyOf` / `oneOf` / `allOf`、`minimum` / `maximum` / `exclusiveMinimum` / `exclusiveMaximum`、`minLength` / `maxLength` / `pattern`、`minItems` / `maxItems`、`minProperties` / `maxProperties`。文字数はUnicodeコードポイント、patternはJavaScriptのUnicode正規表現。`title` / `description` / `$comment` / `examples`は注釈として受け入れる。未対応のキーワード（`$ref`、`format`、スキーマ内の`default`など）や不正なスキーマは明示的にエラーにし、外部参照は取得しない。既定値は上記の`defaults`に指定する。

### 長文を表示する（1.9.0〜）

```ts
await api.ui.showText({ title: "検査結果", text: report, format: "markdown" });
```

`format`は`text`（既定）または`markdown`。TUIは矢印・PageUp/PageDownで移動、`/`で検索、`n` / `N`で次／前、`y`で内容をコピー、Escで閉じる。検索中のEscは検索入力を閉じる。表示とコピーに使う内容の端末制御文字を無害化し、HTML・スクリプト・リンクの実行はしない。モデル呼び出しや履歴への追加はなく、コピーはユーザー操作時だけ行う。確認・選択ダイアログと順番待ちを共有し、表示中の確認を上書きしない。

ヘッドレスでは内容をstderrに表示して直ちに完了する。独自のUIブリッジは省略可能な`showText(options)`を実装でき、未実装の場合は`notify`にタイトルと本文を渡す。

### コマンドの定義（`registerCommand`）

| フィールド | 意味 |
| --- | --- |
| `name` / `description` / `argumentHint` | `/name`で呼ぶ。説明と引数のヒントは補完の一覧に出る |
| `run({args})` | 本体。`args`は名前より後ろの文字列 |
| `complete(prefix)` | 引数の候補（`{value, label, description?}`の配列）を返すと、tabで補完できる（1.3.0〜） |

### フック

`api.on(フック名, handler)`で登録する。

- ハンドラは登録した順に直列で実行する。前のハンドラが返した値は、後のハンドラが受け取る値に反映される。
- 例外を投げたハンドラは、プラグインのエラーとして表示して飛ばす。ループは止まらない。

1回の応答では、次の順に呼ばれる。

```
before_request → stream_delta（生成中、差分ごと） → assistant_message
  → ツール呼び出しごとに: tool_call_raw → 検索・検証 → tool_call → ルールとモードの判定 → permission → （agent モードのモデル判定） → 実行 → tool_result（途中で止まった呼び出しも、tool_result は呼ばれる）
  → turn_end
```

| フック | いつ | 受け取るもの | 返せるもの |
| --- | --- | --- | --- |
| `session_start` | セッションを始めた・再開したとき | `{sessionId?, resumed}` | なし（状態の復元に使う） |
| `session_end` | `/clear`・`/resume`・`/fork`・終了の前 | `{sessionId?, reason?}`。`reason`は、次のセッションが続く（`/clear`など）なら`"switch"`、sasacodeが終わるなら`"exit"`（1.8.0〜。古いホストでは付かないので、ないときは`"exit"`とみなす） | なし（後片付けに使う。外部との接続のようにセッションをまたいで使うものは`"exit"`のときだけ閉じる） |
| `user_prompt` | ユーザー入力をモデルに送る前 | `{content}` | `{content}`で書き換える。`{handled: true}`にするとモデルに渡さない |
| `system_prompt` | リクエストごと | `{prompt}` | `{prompt}`で書き換える（普通は追記する） |
| `before_request` | モデルを呼ぶ直前 | `{messages, model, sampling, fetch?}` | `{messages}`でこのリクエストのメッセージを変える（保存される履歴は変わらない）。`{sampling}`でtemperature・topP・frequencyPenalty・presencePenalty・`extraBody`（プロバイダー固有の値）を変える（1.2.0〜）。`{fetch}`は既存のfetchと合成して、SDKが実際に送るリクエストと再試行を観測できる（1.11.0〜） |
| `stream_delta` | 生成中、テキスト・thinking・ツール引数の差分ごと | `{kind, index, delta, text, message}`（`text`はそのブロックの累積） | `{stop: 理由}`で生成を止める。**同期のみ**で、Promiseを返すとエラーとして無視される（1.2.0〜） |
| `assistant_message` | 応答が確定し、保存する前 | `{message, stopped?}`（`stopped`は途中で止めたプラグインと理由） | `{message}`で書き換える。`{retry: true}`で捨てて取り直す（1回の応答につき最大2回）。`{inject}`でユーザーメッセージを足して続ける（1.2.0〜） |
| `tool_call_raw` | ツールの検索と引数の検証の前 | `{call, name, input, rawInput?, tools, stopReason}`（`rawInput`はJSONとして不正だったときの生の文字列） | `{name, input, note}`で修復する。`note`はtool_resultの先頭でモデルに伝わる。履歴には修復後の呼び出しを残し、元の出力はセッションに`tool_repair`として残る（署名付きのthinkingを含む応答は、履歴を書き換えずに実行時だけ直す）。`max_tokens`で途切れた呼び出しでは呼ばれない（1.2.0〜） |
| `tool_call` | 検証の後、権限判定の前 | `{call, tool, args}` | `{args}`で引数を書き換える。`{decision: "allow" \| "ask" \| "deny", reason}`で判定する（複数あればdeny > ask > allowの強いほう）。プラグインの判定が置き換えるのは権限モードの既定の判定だけで、ユーザーのdeny / askルールは常に適用される |
| `permission` | ルール・権限モード・`tool_call`の判定の後、ユーザーに確認する前 | `{call, tool, args, cwd, mode, verdict: {decision, reason, source}, lowest}`。`source`は`rule`（ユーザーやプラグインのallow / ask / denyルール）、`soft`（softDenyルール）、`mode`（権限モードの既定）、`plugin`（`tool_call`の判定）。`lowest`は変えられる下限 | `{decision, reason}`で判定を変える。厳しくするのはいつでもできるが、緩められるのは`lowest`まで（モードの既定はallowまで、softDenyはaskまで、ルールと`tool_call`の判定は緩められない）。`reason`は確認ダイアログとモデルへの拒否理由に出る。agentモードでどのハンドラも判定しなかった呼び出しは、この後モデルが判定する（1.7.0〜） |
| `tool_result` | 呼び出しごとの結果が決まったとき | `{call, result, ran}`。`ran: false`は実行されなかった呼び出し（未知のツール、引数の不備、拒否、中断）。1.4.0より前は、実行された呼び出しでしか呼ばれなかった | `{result}`で結果を書き換える・追記する |
| `turn_end` | 1ターン（応答とツールの実行）の後 | `{turn, message}` | `{inject}`でユーザーメッセージを足してループを続ける。`{stop: 理由}`で実行を終える（1.4.0〜）。コアは権限・中断・コンテキスト上限でしか止まらないので、それ以外の理由で止めるのはプラグインの役割 |
| `agent_end` | ループが止まったとき | `{cause}`（`done` / `aborted` / `error` / `refusal` / `context_limit` / `max_turns` / `stopped`）。`stopped`のときは`stopped: {plugin, reason}`も付く | `{inject}`で次の実行を始める |
| `context_limit` | コンテキストが上限に達したとき | `{tokens, contextWindow}` | `session.replaceMessages`で空けてから`{retry: true}`で続ける（連続2回まで） |

サブエージェント（`agent.run`）は、`system_prompt` / `before_request` / `stream_delta` / `assistant_message` / `tool_call_raw` / `tool_call` / `permission` / `tool_result`のハンドラを共有する。どのエージェントの呼び出しかは、ハンドラの第2引数`ctx`の`ctx.agent`でわかる（メインのエージェントでは`undefined`、サブエージェントでは実行ごとに違う値。1.8.0〜）。並列に動くサブエージェントを区別して状態を持つプラグイン（loop-guardなど）は、これで分ける。セッション系のフック（`session_*`、`user_prompt`、`turn_end`、`agent_end`、`context_limit`）は、サブエージェントでは呼ばれない。

### API の版

| 版 | 追加されたもの |
| --- | --- |
| 1.0.0 | 最初の公開版（ツール、コマンド、プロバイダー、基本のフック、ui / session / agent） |
| 1.1.0 | `ready(promise)` |
| 1.2.0 | `stream_delta`、`assistant_message`、`tool_call_raw`の各フック。`before_request`の`sampling` |
| 1.3.0 | コマンドの`complete(prefix)`（引数の補完） |
| 1.4.0 | `tool_result`が実行されなかった呼び出しでも呼ばれる（`ran`で区別）。`turn_end`の`stop`で実行を終えられる（停止理由`stopped`） |
| 1.5.0 | `Provider.listModels`（プロバイダーが自分のモデル一覧を返す）と`Request.fetch`（通信の差し替え）。別のプロバイダーを包むプロバイダーを書ける |
| 1.6.0 | ツールの`permissionsAs`（別のツール名の権限ルールも当てる） |
| 1.7.0 | `permission`フック（ルールとモードの判定の後に、決められた範囲で判定を変える）、`PermissionRules.softDeny`、型`PermissionMode`と`VerdictSource` |
| 1.8.0 | フックのハンドラの第2引数`ctx`（型`HookContext`。`ctx.agent`でサブエージェントを区別する）、`session_end`の`reason`（`"switch"` / `"exit"`）、関数`sasacodeHome`・`childEnv`・`hideFromChildren`・`saveOutput`、`unregisterTool`、`ui.lang`、`agent.run`の`onStart`（実行中のサブエージェントへの`send`と`stop`）と結果の`error` |
| 1.9.0 | `ToolContext.callTool`（権限・フック・中断を共有するツール呼び出し）、`PluginAPI.defineSettings`、`PluginUI.showText`、型`SettingsDefinition` / `ShowTextOptions`。引数の制約検証と`tool_call`書き換え後の再検証も強化 |
| 1.10.0 | `PluginUI.input`（1行の自由入力）、`PluginUI.selectMany`（複数選択）、型`InputOptions` / `SelectManyOptions` |
| 1.11.0 | `before_request.fetch`の受け渡し。従来のイベントと結果にoptionalフィールドを追加 |

どれも既存のプラグインを壊さない追加。ただし1.4.0から、`tool_result`を「実行された」合図として数えているプラグインは`ran`を見る必要がある。

### 互換性の約束（1.4.0 で確定）

1.4.0でAPIを締め切った。以降、1.xの間は次を守る。

- 公開するのは`@sasacode/plugin-api`がexportする型・関数・定数と、ここに書いたフックの呼ばれ方だけ。`@sasacode/agent`などの内部パッケージを直接使うコードは対象外。
- 1.xの変更は追加だけにする（新しいフック、イベントや結果の省略可能なフィールド、`StopCause`などの新しい値）。プラグインは、知らないフィールドや値を無視すること。
- 既存のものを変える・消すのは2.0だけ。その前に少なくとも1つのマイナー版で非推奨と告知し、ドキュメントと型に`@deprecated`を付ける。
- 挙動の不具合の修正（例：呼ばれるべきフックが呼ばれていなかった）は、互換を壊す変更とはみなさない。修正の内容は上の表に書く。`apiVersion`に`^1.0.0`と書いたプラグインは、そのまま動く。

## 信頼と安全

インプロセスプラグインはsasacodeと同じ権限で動く。`sasacode plugin install`は入れる前に中身を見せて確認を取る。`~/.sasacode/plugins`のものはユーザーが自分で入れたものとして、そのまま読み込む。プロジェクトの`.sasacode/plugins`は、プロジェクト設定の信頼が必要な部分（エンドポイント、MCPサーバー、プラグインの設定など）とまとめて、起動時に1回だけ信頼の確認を出す。構成が変わると、また確認を出す。ヘッドレスでは、`--trust-project`を付けたときだけ読み込む。確認の結果は`~/.sasacode/trust.json`に保存する。

## 無効化・上書き

```json
{
  "plugins": { "disabled": ["todo", "web-fetch"], "settings": { "compaction": { "threshold": 0.7 } } },
  "tools": { "disabled": ["bash"] }
}
```

同梱プラグインの名前は`tool-repair`、`repetition-guard`、`loop-guard`、`agents-md`、`compaction`、`subagent`、`todo`、`web-fetch`、`permission-presets`、`browsr`、`openai-codex`、`usage`、`goal`、`background-sessions`、`auto-reconnect`、`checkpoints`、`image-attach`、`diagnostics`、`skills`、`mcp`。同じ名前のユーザー・プロジェクトのプラグインがあれば、同梱版は読み込まない。

プラグインを作るときは、組み込みのSkillを使うのが早い。TUIで`/skill:tool-authoring <作りたいもの>`と打つと、このリファレンスを読んだうえでモデルがプラグインを書く。

## API 診断プラグイン

同梱の`diagnostics`は既定で無効。`/diagnostics on` / `off`でこの起動中だけ切り替え、`show`で直近100件を表示、`clear`で消去する。設定は`plugins.settings.diagnostics: {enabled: false, persist: false}`。`persist: true`を明示した場合だけセッションログへ診断メタデータを追加する。`plugins.disabled`にも対応する。

記録するのは、実際のリクエストのUTF-8バイト数・system/toolsのバイト数・メッセージ/推論/署名付きブロック件数、同じモデル呼び出し内のHTTP試行番号と同一本文かどうか、HTTPステータス、完了/中断、APIの使用量・キャッシュ数値、Anthropicの`input_transformations`件数と既知の破棄理由の件数、ツールが実行されたか・エラーか・変更を報告したか。本文・URL・ヘッダー・APIキー・署名・暗号化推論・任意のエラーメッセージは記録しない。未知の破棄理由は件数だけにする。数値はAPIが返した情報で、欠落を0として計上しない。

`before_request`のoptional `fetch`は前のハンドラが渡したtransportを受け取り、それを包んで返す。未指定ならホストの標準fetchを使う。リクエストごとのラッパーなので並列サブエージェントの試行は混ざらない。SDK内部の再試行も観測するが、診断自身は再試行しない。同一本文という記録だけで429等の原因を断定しない。Codexの本文調整後もこのtransportへ委譲する。SSEは先読みせず元のバイト列を渡し、キャンセルを下流へ伝える。本文が文字列でない独自transportやSSE以外は、観測できた範囲に限られる。

## 公開APIの詳細な契約

4,000行のcore上限を維持するため、以下の詳細な契約説明は型定義から移した。公開APIの動作は変えない。型の短い注釈と併せて参照する。

Call another registered tool through validation, hooks and its own permission check.
Inherits cwd, agent and cancellation. Available only while execute is running; await it.
Cycles and chains deeper than 8 tools return an error. No permission bypass. (since 1.9.0)

Also judged by the permission rules written for this tool name, e.g. "bash" for a tool that
runs shell commands another way (in the background, remotely): the user's `bash(sudo *)`
deny rules, the guard preset and `bash(git status*)` allow rules then cover it too, matched
against this tool's matchTarget / paths. Its own rules still apply. (since 1.6.0)

Each hook: the payload handlers receive and what they may return. Handlers run in registration order.
Order around one model response:
  before_request → [stream_delta …] → assistant_message → tool_call_raw (per call)
  → validation → tool_call → rules and mode → permission → execute → tool_result

`reason` (since 1.8.0): "switch" when another session follows (/clear, /resume, /fork), "exit" when
sasacode quits. Undefined from older hosts; treat it as "exit".

Called for every streamed delta. Must be synchronous and cheap (it runs on the stream).
`text` is the block's accumulated text so far. Return `stop` to end generation early. (since 1.2.0)

The finished response, before it is stored. `stopped` says which plugin ended it and why.
Rewrite it (`message`), drop it and ask again (`retry`), or keep it and add a user
message so the loop continues (`inject`). (since 1.2.0)

A tool call as the model produced it, before the tool is looked up and arguments validated.
`rawInput` is set when the arguments were not valid JSON. Return a corrected `name` / `input`
and a short `note`; the model is told about the repair. Truncated calls (stopReason
max_tokens) never reach this hook. (since 1.2.0)

The verdict of the rules, the permission mode and tool_call hooks, before the user is asked.
Return a `decision` to change it: stricter is always accepted; looser only down to `lowest`
(a mode default may go to "allow", a softDeny rule to "ask", the user's rules and tool_call
decisions only stricter). In agent mode, a call no handler decided goes to the model judge.
(since 1.7.0)

Every call's result, including calls that never ran (`ran: false`: unknown tool, invalid
arguments, denied, interrupted), so a plugin can see a model repeating the same mistake.
(`ran` since 1.4.0; before that, only calls that ran reached this hook.)

Return `inject` to add a user message and keep the loop going, or `stop` (a reason) to end the
run here, e.g. when it makes no progress (`stop` since 1.4.0). The core itself only stops for
permission, interrupts and the context limit (principle A6); anything else is a plugin's call.

Undefined for the main agent. Each subagent run (agent.run, the task tool) has its own id,
so a plugin keeping per-conversation state can keep parallel subagents apart.

Read-only text viewer. Resolves when closed; headless writes sanitized text to stderr.
Does not change messages or call a model. Copy requires a user gesture. (since 1.9.0)

One line of free text. Resolves with what was typed (possibly empty), or undefined when
cancelled or headless. (since 1.10.0)

Check any number of options. Resolves with the checked values in option order (possibly none),
or undefined when cancelled or headless. (since 1.10.0)

Validate trusted plugin settings against a schema and defaults. Throws on invalid settings
with a plugin-qualified field path. Call before registering tools/hooks. (since 1.9.0)

Take one of this plugin's tools off the model's list again (an MCP server dropped it).
Another plugin's tool is left alone. (since 1.8.0)

Report background startup work (e.g. connecting a server). Headless runs wait for it before
the first request; the TUI does not block on it. (since 1.1.0)

The environment for commands the agent runs (bash, background jobs): this process's, without the
variables hidden with hideFromChildren, plus `extra`. (since 1.8.0)

Save a long tool output the model is shown only part of, and return its path. Files go to
~/.sasacode/tmp (owner-only, not a shared /tmp); ones older than a week are removed. (since 1.8.0)
