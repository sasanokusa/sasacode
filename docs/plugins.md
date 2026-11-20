# 拡張のしかた

sasacodeのツール・コマンド・フックは、どれもプラグインが登録している。組み込みのread / write / edit / bash、MCP、Skills、同梱プラグインも、同じプラグインAPI（`crates/sasacode-plugin-api`）を通す。0.20からは外部のプラグインを読み込まないので、自分で機能を足す方法は次の2つになる。

- **MCPサーバー**：コードを動かすツール。設定の`mcpServers`に書く。
- **Agent Skills**：手順や知識。`SKILL.md`を決まったフォルダに置く。

同梱プラグインは、設定で外したり、既定で無効のものを有効にしたり、設定を渡したりできる。

## MCPサーバー

`~/.sasacode/config.json`か、プロジェクトの`.sasacode/config.json`の`mcpServers`に、サーバーの名前ごとに書く。

```json
{
  "mcpServers": {
    "github": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" } },
    "remote": { "url": "https://example.com/mcp", "headers": { "Authorization": "Bearer ${REMOTE_TOKEN}" }, "timeout": 60000 }
  }
}
```

| キー | 意味 |
| --- | --- |
| `command` / `args` | stdioのサーバーを起動するコマンドと引数。`command`があればstdioのサーバー、なければ`url`のサーバーとして読む |
| `env` | stdioのサーバーに渡す環境変数。サーバーが受け取るのは、`HOME`・`LOGNAME`・`PATH`・`SHELL`・`TERM`・`USER`（Windowsでは`APPDATA`・`PATH`・`USERPROFILE`など）と、ここに書いたものだけ。値の`${VAR}`は起動時に環境変数から展開する |
| `cwd` | stdioのサーバーの作業ディレクトリ。既定は、グローバルの`~/.sasacode/config.json`に書いたサーバーなら、プロジェクトの外にある専用のフォルダ（`<sasacodeのホーム>/tmp/mcp/<サーバー名>`。名前の英数字・`_`・`-`以外は`%XX`にする。Unixでは0700）。プロジェクトの`.sasacode/config.json`に、起動のしかた（`command`、または`url`）まで書いたサーバー（信頼したあと）なら、sasacodeの作業ディレクトリ。グローバルのサーバーに`disabled`や`timeout`だけを足したときは、グローバルのサーバーのまま。`${SASACODE_PROJECT}`と書くと、sasacodeの作業ディレクトリ（プロジェクト）になる（`"${SASACODE_PROJECT}/tools"`のように続けてもよい）。このほかの`${VAR}`は展開しない。相対パスは、sasacodeを起動したディレクトリから見る |
| `url` / `headers` | Streamable HTTPのサーバー。どちらも`${VAR}`を展開する |
| `disabled` | `true`なら接続しない |
| `alwaysLoad` | ツールの遅延ロードが有効でも、このサーバーのツールの定義を常に送る |
| `timeout` | 1回のツール呼び出しの上限（ミリ秒）。既定は600000（10分） |
| `toolNames` | `"plain"`なら、ツールをサーバーが付けた名前のまま登録する（同じ名前のツールがあれば置き換わる）。既定は`mcp__<server>__<tool>`で、英数字・`_`・`-`以外の文字は`_`になり、64文字で切る |

- サーバーはバックグラウンドで接続するので、TUIの起動を待たせない。ヘッドレスでは、接続を待ってから最初のリクエストを送る。
- ツールは通常のツールと同じ権限確認とフックを通る。種類は`other`なので、editsモードでも実行の前に確認する（allowルールで許可できる）。サーバーが`readOnlyHint`を付けたツールは、ほかのツールと並行して実行する。
- サーバーのpromptsは`mcp__<server>__<prompt>`という名前のスラッシュコマンドになる。
- 落ちたサーバーは、間隔を空けて5回まで自動で再接続する（`plugins.settings.mcp.reconnectDelayMs`で間隔を縮められる）。`/mcp`で接続の状態を見て、`/mcp reconnect [name]`で手動でつなぎ直す。フッターには`MCP <接続中>/<全体>`を出す。
- サーバーのツールの一覧が変わったら、一覧から外れたツールはモデルに渡さなくなる。
- 既定の作業ディレクトリをプロジェクトの外にするのは、`npx`や`uvx`がカレントの`node_modules`、`uv.toml`、`pyproject.toml`を先に見るためである。グローバルのサーバーでプロジェクトの中身（ファイルや`node_modules`）に頼るものには、`"cwd": "${SASACODE_PROJECT}"`を足す。同梱の`free-search`も`browsr`も、同じ専用のフォルダで起動する。
- プロジェクトの`.sasacode/config.json`に書いたサーバーは、プロジェクトを信頼するまで起動しない（[README](../README.md#プロジェクトの信頼)）。信頼したあとでサーバーの設定が変わると、また確認を出す。
- `plugins.disabled`に`mcp`と書くと、`mcpServers`のサーバーを1つもつながない。

## Skills

標準のAgent Skills形式のフォルダを、次の順に読む。同じ名前のSkillは、後から読んだものが優先する。

1. 組み込みのSkill（`~/.sasacode/builtin-skills`。起動時にsasacodeが書き出す。いまは`tool-authoring`）
2. `~/.sasacode/skills/<フォルダ>/SKILL.md`
3. プロジェクトの`.sasacode/skills/<フォルダ>/SKILL.md`

```markdown
---
name: release-notes
description: Write release notes from the commits since the last tag.
---

1. `git describe --tags --abbrev=0`で前のタグを調べる。
2. …
```

- 使うのはfrontmatterの`name`と`description`だけで、どちらかが空のフォルダは読まない。値は、そのままの文字列、引用符で囲んだ文字列、`>` / `|`（`>-` / `|-`も）で折り返した複数行のどれでもよい。`license`や`allowed-tools`などほかのキーは読み飛ばす（ツールを制限する機能はない）。
- システムプロンプトには名前・説明・`SKILL.md`のパスだけを載せ、本文はモデルが必要なときにreadツールで読む。Skillのフォルダを読むときは確認しない（`read(<フォルダ>/**)`のallowルールを足す）。
- `/skill:<name> [依頼]`で明示的に使わせる。依頼を付ければすぐに、付けなければ次のメッセージと一緒に`SKILL.md`を送る。`/skills`で一覧を出す。
- プロジェクトのSkillは、信頼の確認なしで読む。
- `plugins.disabled`に`skills`と書くと、Skillを1つも読まない。0.15まで読んでいたプラグインの`skills`ディレクトリは、0.20からはない。

## 同梱プラグインの設定

```json
{
  "plugins": {
    "enabled": ["git-context", "project-checks"],
    "disabled": ["todo", "web-fetch"],
    "settings": { "compaction": { "threshold": 0.7 } }
  },
  "tools": { "disabled": ["bash"] }
}
```

| キー | 意味 |
| --- | --- |
| `plugins.enabled` | 既定で無効のプラグインを読み込む |
| `plugins.disabled` | プラグインを読み込まない。`enabled`にも書いてあれば、こちらが優先する |
| `plugins.settings.<名前>` | そのプラグインの設定 |
| `tools.disabled` | ツールを名前で外す（どのプラグインが登録したものでも） |

- `enabled`・`disabled`・`tools.disabled`は、グローバルとプロジェクトの両方を合わせる。プロジェクトの設定に書いた`plugins`（`enabled`・`disabled`・`settings`）は、プロジェクトを信頼するまで使わない。
- 既定で読み込むもの：`tool-repair`、`text-tool-calls`、`repetition-guard`、`thinking-budget`、`error-hints`、`loop-guard`、`write-guard`、`search-tools`、`syntax-check`、`action-nudge`、`agents-md`、`compaction`、`diagnostics`、`subagent`、`todo`、`progress-notes`、`web-fetch`、`web-search`、`permission-presets`、`agent-judge`、`browsr`、`openai-codex`、`usage`、`goal`、`background-sessions`、`auto-reconnect`、`checkpoints`、`gui`、`image-attach`、`image-slim`、`context-slim`、`ask-user`、`skills`、`mcp`。それぞれの内容は[README](../README.md)の同梱プラグインの表にある。
- 既定で無効のもの：`slim-profile`、`hashline-edit`、`git-context`、`project-checks`、`workflows`、`jev-guard`、`free-search`、`clean-retry`。このうち`git-context`、`project-checks`、`workflows`、`jev-guard`、`free-search`は、`ask-user`と同じく0.15までは別に入れるプラグインだった。名前も設定も当時のままなので、`enabled`に名前を足せば、それまでの`plugins.settings`がそのまま使える。
- 0.15まで`~/.sasacode/plugins`やプロジェクトの`.sasacode/plugins`に置いたプラグインは読み込まない。同梱版と同じ名前のプラグインで置き換える仕組みもなくなった。

以下は、0.15までは別に入れるプラグインだった6つ。`ask-user`は既定で読み込み、ほかの5つは既定で無効。

### ask-user

`ask_user`ツールを足す。モデルが推測で進めずに、選択肢つきの質問であなたに判断を仰ぐ。

- 1回で1〜4問を尋ねる。1問あたりの選択肢は2〜6個で、質問は順に選択パネルに出る。
- 選択肢が排他的でない質問では、モデルが`multiSelect`を指定できる。spaceで複数を選び、enterで決める。
- どの質問にも最後に「その他（自分で答える）」が付き、その場で答えを入力できる。
- Escで閉じたとき、空のまま入力を確定したとき、複数選択で何も選ばずに確定したときは、そこで打ち切る。モデルには、答えを推測しないよう伝える。実行中に中断すると、ツールはエラーで終わる。
- ヘッドレスでは何も尋ねない。モデルに、自分で決めて置いた前提を返信に書くよう返す。
- 質問するための承認は意味がないので、`ask_user`のallowルールを足す。askモードでも確認なしで出る。止めたいときは`permissions.deny`に`ask_user`と書く（ユーザーのdenyルールが優先する）。
- 既定で読み込む。ツールごと外すなら、`plugins.disabled`に`ask-user`と書く。

### git-context

`git_context`ツールを足す。レビューの初めに要るGitの状態・変更の統計・パッチを、1回の呼び出しで返す。読み取りだけで、Gitが要る。設定はない。

| 引数 | 既定 | 意味 |
| --- | --- | --- |
| `scope` | `working` | `working`：HEADと作業ツリーの差分（ステージ済みと未ステージ）。`staged`：HEADとindex。`branch`：merge-baseとHEAD |
| `base` | `@{upstream}` | `branch`の比較元（`main`、`origin/main`など）。リモート追跡ブランチは、ローカルにある版を使う |
| `patch` | `true` | `false`なら状態と統計だけを返す |
| `maxChars` | `16000` | 出力全体の文字数の上限。1000〜60000 |

- 作業ディレクトリの中だけを読む。Gitの外部diff・textconv・fsmonitorは使わず、変更もfetchもしない。権限は通常のreadとして判定する。
- 作業ツリーの状態はどの`scope`でも出すので、branchのパッチに入らない未コミットの変更もわかる。
- 未追跡のファイルは名前だけで、サブモジュールの差分は対象外。ステージで変えて作業ツリーで戻した箇所は`working`では相殺されるので、要るなら`staged`も調べる。
- 上限を超えた出力は、省いたことを明記する。Gitの各プロセスは15秒・8 MiBまでで、超える差分はエラーになる（`patch: false`か、ファイルを個別に読む）。upstreamがなければ`base`を指定する。

### project-checks

テスト・lint・型検査などを名前で実行し、PASS / FAIL、終了コード、実行時間、ログを返す。

- `project_checks`ツールで一覧を読み、`project_check`ツール（`{"name": "types"}`）で1つずつ実行する。`/checks`は同じ一覧を長文のパネルに出す（コマンドは実行しない）。
- 設定がなければ、作業ディレクトリの`package.json`から`test`・`lint`・`typecheck`・`check`・`build`のスクリプトを拾う。パッケージマネージャーはルートのlockfileで選び（bun / pnpm / yarn / npm、lockfileがなければnpm）、複数見つかったら設定を求める。monorepoの親ディレクトリは探さない。
- 設定の`checks`を書くと、自動の検出の代わりにそれを使う（空の`{}`なら検査なし）。JavaScript以外のプロジェクトにも使える。名前は英小文字で始まる40字以内の英小文字・数字・ハイフン。`timeout`は秒で、既定は120、範囲は1〜3600。設定は起動時に読むので、変えたら再起動する。

```json
{
  "plugins": {
    "enabled": ["project-checks"],
    "settings": {
      "project-checks": {
        "checks": {
          "test": { "command": "cargo test", "timeout": 600 },
          "lint": "ruff check ."
        }
      }
    }
  }
}
```

- `project_check`は実行のツールで、`bash`のルールを実際のコマンドに当てる（`permissionsAs: "bash"`に当たる）。たとえば`bash(cargo test*)`のdeny / askがそのまま効く。allowルールは足さない。検査のスクリプトがファイルを書き換えることもあるので、読み取りとは扱わない。
- 実行は`bash`ツールに任せるので、引数の検証・フック・権限の判定は通常のbashと同じ。`bash`を外していると実行できない。時間には承認を待った時間も入る。タイムアウト・中断・拒否・0以外の終了はFAIL。

#### 終える前の確認（verifyOnFinish）

`verifyOnFinish`に検査の名前を並べると、モデルが作業を終えようとしたときに、その検査をプラグインが自分で実行する。既定では無効。小型モデルで見た失敗への対策で、テストを流さずに、あるいは落ちたまま「直した」と報告する、「確認します」と書いて止まる、テストの方を書き換える、の3つが対象（[benchmarks.md](benchmarks.md)）。

```json
{
  "plugins": {
    "enabled": ["project-checks"],
    "settings": {
      "project-checks": {
        "checks": { "test": { "command": "cargo test", "timeout": 600 } },
        "verifyOnFinish": ["test"]
      }
    }
  }
}
```

- 実行するのは、モデルがツールを呼ばずにターンを終え、かつ、その依頼のあいだに次のどちらかがあったときだけ。
  - `write`か`edit`でファイルを変えた（サブエージェントが変えたものも含む）。
  - 検査が落ちた。`project_check`、検査のコマンドを実行した`bash`、この機能が流した検査のどれでもよい。ファイルを変えずに止まったときにも働く。
- ファイルを変えた後に、モデル自身が`project_check`で同じ検査を通していれば流し直さない。`bash`で流した検査は、落ちたときだけ数える（`cargo test | tail`は`tail`の終了コードで通るので、通ったことは当てにならない）。`bash`が検査を実行したとみなすのは、検査のコマンドが、bashのコマンドの頭か`;`・`&&`・`|`の後から始まるとき（`cd app && cargo test 2>&1`や`cargo test -p foo`は含む。`grep 'cargo test' docs`は含まない）。
- 書いた順に1つずつ実行し、最初に落ちたところで止める。通れば何も言わない。落ちれば、出力の末尾（40行・3,000バイトまで）をモデルに返して作業を続けさせる。返すのは1回の依頼で2回まで。3回目以降は、モデルの言うとおりに終える。
- 変えたファイルがテストだけのとき、またはテストを飛ばす印（`it.skip(`・`test.skip(`・`describe.skip(`と、同じ形の`.only(`、`xit(`、`xdescribe(`、`#[ignore]`、`@pytest.mark.skip`、`@unittest.skip`）を足したときは、検査が通っても短い注意を1回だけ返す。テストのファイルは、`test`・`tests`・`__tests__`・`spec`のディレクトリの下にあるか、`.test.`・`.spec.`・`_test.`・`_spec.`・`_tests.`を含む、または`test_`で始まる名前で見分ける。印は、差分で足された行から、語の頭にあるものだけを数える（`process.exit(`は`xit(`に、イテレーターの`.skip(1)`は印に数えない）。
- 権限の確認なしに実行する。フックからはツールを呼べないため。だから名前を書けるのは`checks`に書いた検査だけで、`package.json`から拾った検査や、ない名前を書くと読み込みのエラーになる。プロジェクトの設定に書いたものは、ほかのプラグインの設定と同じく、プロジェクトを信頼するまで使わない。
- 実行のしかたは`bash`ツールと同じ。sasacodeが`.env`から読んだキーを外した環境変数で動かし、検査の`timeout`で打ち切る。プロセスグループごと止め、長い出力は`~/.sasacode/tmp`に保存する。実行中はEscで止まらず、終わるかタイムアウトしてから中断になる。長い検査を選ぶなら、`timeout`を短めにする。

### workflows

繰り返し使う依頼を`/workflow`で呼び出す。

```text
/workflow
/workflow show review
/workflow review 認証まわりを重点的に
/workflow diagnose 決済のテストがタイムアウトする
```

- 組み込みは3つ。`review`は差分と周りのコードを読み、根拠と再現の条件を付けてレビューする（コードは変えない）。`diagnose`は失敗を再現し、観測と仮説を分けて修正案を出す（まだ適用しない）。`release-check`は公開の前に版・配布物・ライセンス・依存・検査を確かめる（公開や版の変更はしない）。文面は画面の言語（ja / en）に合わせる。
- `/workflow`か`/workflow list`で一覧、`/workflow show <名前> [追加の依頼]`で展開した結果を長文のパネルに出す（モデルは呼ばない）。`/workflow <名前> [追加の依頼]`は、通常のユーザーメッセージとして今のセッションに送る。
- git-contextやproject-checksがあれば使うよう頼む文面だが、なくても使える。プロンプトの集まりで、行動や権限を縛るものではない。
- 設定で足す・上書きする・外す（`false`）ことができる。プレースホルダーは`{{input}}`だけで、省けば追加の依頼を末尾に付ける。シェル、環境変数、ファイルの読み込みは展開しない。名前は英小文字で始まる40字以内の英小文字・数字・ハイフンで、`list`と`show`は使えない。説明は200字、プロンプトは20,000字まで。

```json
{
  "plugins": {
    "enabled": ["workflows"],
    "settings": {
      "workflows": {
        "workflows": {
          "release-check": false,
          "explain": {
            "description": "コードの処理経路を説明する",
            "prompt": "{{input}}の処理経路を調べ、入口から出口までファイルと関数名を添えて説明してください。コードは変更しないでください。"
          }
        }
      }
    }
  }
}
```

### jev-guard

ルールと権限モードの判定の後に、判断用のモデル[Jev](https://commandcode.ai/models/jev)（TypeSafe）の判定を重ねる。Jevは文章を返さず、型付きの質問に確率で答えるモデルで、1回あたり数百ミリ秒で済む。0.9.5までは同梱、0.9.6から0.15までは別に入れるプラグインで、0.20からはまた同梱になった（既定で無効）。

有効にする方法は2つある。

- `plugins.settings["jev-guard"].enabled: true`：読み込んで、いつも判定する（0.15までの設定もそのまま使える）。
- `plugins.enabled`に`jev-guard`：読み込むだけで、判定は止まっている。TUIで`/jev on`と打つと、そのセッションで有効になる（`/jev off`で無効。`/resume`した後も保たれる）。

```json
{ "plugins": { "settings": { "jev-guard": { "enabled": true } } } }
```

- Jevへの接続先は`backend`で選ぶ。指定がなければ、`CMD_API_KEY`か`TYPESAFE_API_KEY`のある方を使う（どちらもキーチェーンの`commandcode` / `typesafe`でもよい）。

  | `backend` | 接続先 | キー | 既定の`model` |
  | --- | --- | --- | --- |
  | `commandcode` | Command CodeのProvider API（GOAT以上のプラン） | `CMD_API_KEY` | `typesafe/jev` |
  | `typesafe` | TypeSafeのAPI | `TYPESAFE_API_KEY` | `jev-latest` |
  | `openrouter` | OpenRouterのDecisions API（alpha） | `OPENROUTER_API_KEY` | `~typesafe/jev-latest` |
  | `vercel` | Vercel AI Gateway | `AI_GATEWAY_API_KEY` | `typesafe-ai/jev` |
  | `chat` | sasacodeの任意のモデル（`model`に`"<provider>/<model>"`、省けば今のモデル） | そのプロバイダーのキー | — |

  OpenRouterとVercelは、チャット用にキーを持っているだけの人に判定を送らないよう、名前を指定したときだけ使う。`endpoint`と`apiKeyEnv`で接続先のURLとキーの変数を変えられる（プロキシなど）。実際のAPIで確かめたのは`commandcode`と`chat`だけで、`typesafe` / `openrouter` / `vercel`は公開されている仕様どおりの形で送る（テストは模擬サーバー）。
- `chat`はJevの代わりにチャットモデルへ同じ質問をし、JSONで答えさせる。確率が較正されていない代用品で、遅い。実際のコマンドで比べると、DeepSeek V4.1 Flashは止めるべき呼び出しをJevより多く通した（[ベンチマーク](benchmarks.md)）。ローカルモデルは1回に数十秒かかり、実用的でない。Jevに接続できない環境で、判定の仕組みだけを使いたいとき向け。
- 有効にすると、ツール呼び出しの内容（コマンド、変更されうるパスとそのgitの状態、ユーザーの直近の依頼）が接続先に送られる。APIキーやトークンらしき文字列は、送る前に伏せる（ベストエフォート）。
- 判定を変えるのは次の場合だけ。
  - 確認なしで実行されるはずだった呼び出し（allowルール、`auto`モード）：危険なら拒否、要確認なら確認に回す。
  - `agent`モードで確認に回るはずだった呼び出し：安全なら確認なしで実行する（モデルによる判定の代わり）。Jevが判断を保留したときは、これまでどおりモデルが判定する。
  - softDenyの呼び出し：安全なら、拒否せずにユーザーに確認する。
- ユーザーに確認するはずの呼び出しを、Jevが拒否に変えることはない（確認のダイアログにJevの判定を添える）。denyルールの呼び出しはJevに送らない。
- Jevに渡すのは、ハーネスが集めた事実とユーザー自身の依頼だけで、ツールの出力やファイルの中身は渡さない（Jevは状態に紛れ込んだ誘導文に影響されうるため）。
- 読み取りと、作業ディレクトリ内の編集は判定しない。Jevに届かないとき（キーがない、通信エラー、タイムアウト）は、Jevなしの判定のまま続ける。
- 閾値は`thresholds`（`allow` 0.8、ヘッドレスでの`headlessAllow` 0.9、`deny` 0.85、`confidence` 0.6、`risk` 0.6、`flag` 0.7、`clear` 0.3）で変えられる。ほかの設定は`timeoutMs`（既定は5秒、`chat`は60秒）と`skip`（判定しないツール。既定は`todo_write`と`task`）。
- `/jev`で状態と直近の判定を見る。キーがなければ`/jev on`でも有効にせず、理由を示す。判定はセッションにも記録する。
- Jevはコマンドの文字列から判断するので、`ssh host 'ps aux'`のような他のマシンでの読み取りは安全と見る。`make clean`のようにスクリプトの中身が見えない呼び出しは、中身が危険でも気づけない。他のマシンへの操作は`guard`プリセットが常に確認に回す。スクリプト経由の呼び出しも必ず確認したいなら、ルールで決める（例：`"permissions": { "ask": ["bash(npm run *)", "bash(make *)"] }`）。
- 実際のセッションログ5,135件での結果：安全な呼び出しの92%がautoで止められず、agentモードでは71%が確認なしで通る。要確認のものがagentで確認なしに通ったのは1%（ローカル）。詳しくは[ベンチマーク](benchmarks.md)。

### free-search

キー不要でローカルで動くWeb検索のMCPサーバー（複数の検索エンジン、ページの取得、文書の読み取り）。有効にすると、`uvx free-search-mcp`を`free-search`という名前のstdioのMCPサーバーとしてつなぐ。ツールは`mcp__free-search__<tool>`になり、ほかのMCPのツールと同じ権限確認を通る。[uv](https://docs.astral.sh/uv/)が入っていないと接続できない。プロジェクトの外の専用のフォルダで起動するので、`uvx`はプロジェクトの`uv.toml`や`pyproject.toml`を読まない。`/mcp`とステータス行には、`mcpServers`のサーバーと並んで出る（`mcpServers`に同じ名前があれば、そちらを使う）。`mcp`プラグインを無効にしても、free-searchだけで動く。設定はない。

```json
{ "plugins": { "enabled": ["free-search"] } }
```

### hashline-edit

`read` の各行の先頭を `<行番号>:<2桁ハッシュ>|` に変え、`hash_edit` ツールを足す。小型モデルが空白やインデントの一字一句コピーに失敗する問題を抑える。誤って `N:hh|` を付けたまま `write` を呼んだ場合は接頭辞を自動で外す。

2桁のハッシュでは同じ内容の行（`}`など）を区別できないため、`hash_edit`はハッシュの一致だけでなく、範囲内のすべての行がそのエージェントに見せた位置のままであることを確かめる。自分の`hash_edit`以外（`edit`・`write`・別のエージェント・利用者）でファイルが変わると、そのファイルのアンカーはすべて無効になり、もう一度`read`が必要になる。`slim-profile`と併用したときは、切り詰め後に残った行にだけアンカーを付ける。

判断がつかないときは編集せず、読み直しを求める。そのエージェントがそのファイルを読んでいない、または記録が消えた（多数のファイルやサブエージェントを扱った後、`/resume`の後など）ときや、どのエージェントの呼び出しか特定できないときがこれに当たる。2MBを超えるファイルにはアンカーを付けず、`hash_edit`も受け付けない（`edit`を使う）。同じファイルへの`hash_edit`は1つずつ実行し、書き込む直前にファイルが変わっていれば書かずに止める。並行するサブエージェントの編集が互いを消さないようにするため。

### clean-retry

編集エラー（`consecutive_edit_errors`、既定4回）やテスト失敗（`triggerFailures`、既定3回）が続いたとき、変更したファイルをタスク開始前に自動で戻し、失敗の要約だけを残した綺麗なコンテキストでやり直す。`maxRetries`（既定 1）、`triggerFailures`（既定 3）、`triggerEditErrors`（既定 4）、`maxFileKB`（既定 1024）を設定できる。

### slim-profile

小型ローカルモデル向けに、`subagent`・`todo`・`goal`・`web-fetch`・`web-search`・`browsr`・`background-sessions`・`progress-notes` と Skills（`dropSkills: false` で維持）を自動で外し、システムプロンプトへ簡潔な行動規則を足し、`read`（`readMaxChars`、既定 16000）・`bash`（`bashMaxChars`、既定 8000）の単発出力の上限を切り詰める。

### ローカルモデル向けの既定プラグイン（thinking-budget / action-nudge / context-slim）

`thinking-budget`・`action-nudge`・`context-slim`の3つは既定で有効だが、クラウドモデルでは挙動を変えないよう**ローカルモデルだけで動作する**。ローカルモデルとみなすのは、`model.server`が設定されている（`ollama` / `llamacpp` / `lmstudio`）、または`model.baseUrl`がこのマシンやローカルネットワークを指す（プライベートIP、`localhost`などドットのない名前、`.localhost` / `.local` / `.lan` / `.internal`で終わる名前）場合。ただし、ローカルOllama経由で呼び出すOllamaクラウドモデル（モデルIDの末尾が`-cloud`、または`:cloud`・`-cloud:`を含むもの）はクラウド扱いとして対象外になる。

- **thinking-budget**：ローカルモデルの思考ブロックが上限（`localMaxChars`、既定`12000`。モデルや用途に合わせて設定で引き上げられる）を超えると生成を止め、ツール呼び出しや最終回答へ進むよう促す。`maxChars`や`cloudMaxChars`（既定`0`）を明示したときはクラウドモデルにも効く（`maxStopsPerRun`、既定`2`）。
- **action-nudge**：ローカルモデルがツールを呼ばずにターンを終えたとき、応答の最終文・最終行だけを見て実行意図の宣言（「確認します」「Let me check...」など）であればすぐツールを呼ぶよう促す（`maxNudges`既定`2`、`maxChars`既定`1200`）。疑問文、完了報告、長文解説、「必要であれば」「必要なら」「ご希望なら」「よろしければ」「if you want」「if you'd like」「once you confirm」「let me know」などの条件付き提案には反応しない。
- **context-slim**：ローカルモデルで推定トークン数がコンテキスト長の閾値（`triggerRatio`、既定`0.55`）を超えたとき、直近の履歴（`keepRecent`、既定`6`）を残したまま古い大きな結果だけを`maskStep`（既定`4`）刻みで短い注記に畳み、ステップ境界のあいだはKVキャッシュを保つ（`minChars`、既定`1200`）。対象は`read`（`SKILL.md`の読み取りは除外）・`bash`・`find_files`・`search_text`（および`grep` / `glob` / `ls`）の結果だけで、`task`によるサブエージェント報告やその他のツール結果は畳まない。

```json
{
  "plugins": {
    "enabled": ["slim-profile", "hashline-edit", "clean-retry"],
    "settings": {
      "thinking-budget": { "localMaxChars": 12000 },
      "context-slim": { "triggerRatio": 0.55, "maskStep": 4 }
    }
  }
}
```

## API診断プラグイン

同梱の`diagnostics`は、読み込むが記録は止まっている。`/diagnostics on` / `off`でこの起動中だけ切り替え、`show`で直近100件を出し、`clear`で消す。設定は`plugins.settings.diagnostics: {enabled: false, persist: false}`。`persist: true`を明示したときだけ、セッションログに診断のメタデータを足す。

記録するのは、実際のリクエストのUTF-8のバイト数・system / toolsのバイト数・メッセージ / 推論 / 署名付きブロックの件数、同じモデル呼び出しの中でのHTTPの試行番号と本文が同じかどうか、HTTPのステータス、完了 / 中断、APIの使用量・キャッシュの数値、Anthropicの`input_transformations`の件数と既知の破棄理由の件数、ツールが実行されたか・エラーか・変更を報告したか。本文・URL・ヘッダー・APIキー・署名・暗号化された推論・任意のエラーメッセージは記録しない。未知の破棄理由は件数だけにする。数値はAPIが返したもので、欠けた値を0として数えない。

`before_request`の`fetch`は、前のハンドラが渡したtransportを受け取り、それを包んで返す。指定がなければ標準のHTTPクライアントを使う。リクエストごとのラッパーなので、並列のサブエージェントの試行は混ざらない。診断自身は再試行しない。本文が同じという記録だけで、429などの原因を断定しない。Codexの本文の調整の後も、このtransportに任せる。SSEは先読みせず元のバイト列を渡し、キャンセルを下流に伝える。

## テキストで書かれたツール呼び出し

同梱の`text-tool-calls`（既定で有効）は、`assistant_message`で応答を書き換え、本文に書かれた呼び出しを`tool_call`ブロックにする（停止理由は`tool_use`、idは`textcall`で始まる）。引数のJSONは書かれたままパースし、崩れていれば`raw_input`に入れて、`tool_call_raw`でtool-repairに直させる。名前のまわりまで崩れている（Python風の単一引用符など）ときは、tool-repairと同じ`repair_json`で全体を直してから読む。拾ったことは、その呼び出しの`tool_call_raw`の`note`でモデルに伝える。引数のJSONが直せずに実行されなかったときも、エラーと一緒に伝わる。前の文に残った呼び出し（後ろに文が続くので拾わなかったもの）があれば、実行しなかったその数も`note`に書く。コードブロックやインラインコードの中の例と、ほかの呼び出しの引数の中に書かれたものは数えない。元の本文はセッションに`text_tool_call`（`{text, calls, agent?}`、`calls`は呼び出しのid）として残る。

拾った呼び出しのうち種類が`read`でないもの（`asks_first`）は、autoモード以外で判定が権限モードの既定によるとき、`permission`フックで`ask`にする。editsモードでの作業ディレクトリ内の書き込みや編集も確認に回る。agentモードでは、後で動く`agent-judge`がこの呼び出しを判定しない（`text-tool-calls`が`ask`を返しても判定の出どころは`mode`のままなので、判定させると`allow`に緩められてしまう）。例を書いただけの応答で、ファイルが黙って書き換わらないようにするため。autoモード、ユーザーやプラグインのallowルール、`tool_call`の判定は変えない。確認できないヘッドレス（`sasacode -p`、既定はeditsモード）では、その呼び出しは実行されない。

拾うのは、次をすべて満たす応答だけ。どれかが外れれば、応答はそのまま残す。

- メインのエージェントのモデルと同じプロバイダーの応答で、その`baseUrl`のホストが、このマシンかローカルネットワークのもの（`private_address`が認めるIPアドレス、`localhost`などドットのない名前、`.localhost` / `.local` / `.lan` / `.internal`で終わる名前）。クラウドのAPIのモデルはネイティブに呼び出せるので、本文の呼び出しは例とみなす。設定`always: true`なら、どのモデルの応答も対象にする
- 自然に終わった（停止理由が`stop`で、プラグインが止めていない）。ネイティブの`tool_call`も、署名付きのthinkingもない（書き換えると、署名付きの推論は次のリクエストで再生できない）
- 最後のtextブロックが呼び出しで終わり、最後の呼び出しの後ろと呼び出しの間には空白しかない。前の文は残す。後ろに説明が続く例や、コードブロック・インラインコードの中の例は、閉じる記号が後ろに来るので拾わない。前の文の` ``` `が奇数個（コードブロックが閉じていない）なら、呼び出しはその中にあるとみなして拾わない
- `json`のコードフェンスを含むときは、前の文が300字以内で、ほかのコードブロックを含まない（普通の回答も、長い説明の最後にJSONの例を置くことがあるため）。中身は`name`と`arguments`（または`parameters`）の2つのキーだけのオブジェクトで、`description`などを持つツール定義は呼び出しとみなさない
- ツール名がすべて、既知のツールか、`resolve_tool_name`（tool-repairと同じ規則：接頭辞の除去、MCPの接頭辞を省いた名前、ほかのハーネスでの名前）で1つに決まるもの。typoは数えない（`head`や`hash`のような普通の語が`read`や`bash`になってしまうため）

## 同梱プラグインを書く（開発者向け）

同梱プラグインは`crates/sasacode-bundled/src`のRustのモジュールで、`sasacode-plugin-api`の`PluginApi`だけを通して登録する。外部のプラグインはないので、このAPIは公開しない内部のAPIで、版ごとの互換も約束しない。

1. モジュールに`pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>>`を書く。
2. `crates/sasacode-bundled/src/lib.rs`の`PLUGINS`に加える。既定で読み込むなら`on("名前", モジュール::plugin)`、既定で無効なら`off(…)`。並びが読み込みの順で、フックもこの順に呼ばれる。
3. 名前は`plugins.enabled` / `plugins.disabled` / `plugins.settings.<名前>`で使われる。READMEの同梱プラグインの表（日本語と英語）にも足す。

例：`git-context`（`crates/sasacode-bundled/src/git_context.rs`。説明文は略した）。

```rust
pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        api.register_tool(
            ToolDef::new(
                "git_context",
                "Read a bounded Git review context: …",
                json!({
                    "type": "object", "properties": {
                        "scope": { "type": "string", "enum": ["working", "staged", "branch"] },
                        "base": { "type": "string", "minLength": 1, "maxLength": 256 },
                        "patch": { "type": "boolean" },
                        "maxChars": { "type": "integer", "minimum": 1000, "maximum": 60_000 },
                    }, "additionalProperties": false,
                }),
                ToolKind::Read,
                |args, ctx| async move { collect_context(&args, &ctx).await },
            )
            .concurrent(true)
            .paths(|_args, cwd| vec![cwd.to_string_lossy().into_owned()])
            .into_ref(),
        );
        Ok(())
    })
}
```

```rust
// crates/sasacode-bundled/src/lib.rs
pub const PLUGINS: &[Bundled] = &[
    on("tool-repair", tool_repair::plugin),
    // …
    off("git-context", git_context::plugin),
    // …
];
```

`PluginApi`（`crates/sasacode-plugin-api/src/api.rs`）でできること：

| メソッド | できること |
| --- | --- |
| `name()` / `cwd()` / `version()` | このプラグインの名前 / 作業ディレクトリ / APIの版（記録のために残した`1.15.0`） |
| `settings()` / `define_settings(schema, defaults)` | `plugins.settings.<名前>`を読む / JSON Schemaで検証して既定値とまとめる（不正ならエラー） |
| `register_tool(ToolRef)` / `unregister_tool(name)` | ツールを足す（同じ名前のツールは置き換わる） / このプラグインのツールを外す |
| `register_command(Command)` | スラッシュコマンドを足す（`Command::new(名前, 説明, 本体).hint(…).complete(…)`） |
| `register_provider(name, config, implementation)` | プロバイダーを足す。`implementation`を渡せば、新しいAPI形式も足せる |
| `on::<hook::X, _, _>(f)` / `on_sync::<hook::X, _>(f)` | フックを登録する（`on_sync`は`stream_delta`のような同期のフック用） |
| `ready(future)` | 起動時の非同期の処理を知らせる。ヘッドレスはこれを待ってから最初のリクエストを送り、TUIは待たない |
| `add_permission_rules(PermissionRules { allow, ask, deny, soft_deny })` | 権限ルールを足す |
| `ui()` | `interactive`、`lang`、`notify`、`confirm`、`select`、`select_many`、`input`、`show_text`、`set_status`、`register_tool_renderer`、`on_paste` |
| `session()` | `id`、`append` / `entries`（独自の状態の保存と読み戻し）、`messages` / `replace_messages`、`inject` / `inject_text` |
| `agent()` | `model`、`tools`、`run`（サブエージェント）、`complete`（ツールなしで1回だけモデルを呼ぶ） |
| `log(message)` | `SASACODE_DEBUG`があるときだけstderrに出す |

- ツールは`ToolDef::new(名前, 説明, 引数のJSON Schema, ToolKind, 本体)`で作り、`.concurrent`・`.always_load`・`.summary`・`.match_target`・`.paths`・`.permissions_as`を足して`.into_ref()`で渡す（`Tool`トレイトを直接実装してもよい）。`ToolKind`は`Read` / `Edit` / `Exec` / `Other`で、既定のeditsモードで確認なしに動くのは作業ディレクトリ内のRead / Editだけ。本体が`Err`を返すと、エラーの結果としてモデルに返り、ループは止まらない。
- 本体の`ToolContext`には`cwd`、`signal`（中断）、`on_update`、`call_tool`（ほかのツールを、検証・フック・権限の判定を通して呼ぶ）、`read_rule`（そのツールがあるパスを読むことに、denyやaskのルールが当たるか。ディレクトリを歩いて見つけたファイルのように、権限の判定を通っていないパスに使う）がある。
- フックは`crates/sasacode-plugin-api/src/hooks.rs`の`hook`にある：`SessionStart`、`SessionEnd`、`UserPrompt`、`SystemPrompt`、`BeforeRequest`、`StreamDelta`、`AssistantMessage`、`ToolCallRaw`、`ToolCall`、`Permission`、`ToolResult`、`TurnEnd`、`Usage`、`AgentEnd`、`ContextLimit`。ハンドラは登録の順に呼ばれ、`Ok(None)`なら変えず、`Ok(Some(結果))`で変え、`Err`はプラグインのエラーとして表示して次のハンドラに進む。第2引数の`HookContext`の`agent`は、メインのエージェントなら`None`、サブエージェントなら実行ごとに違う値になる。
- コマンドを実行するプラグインは、`child_env`か`apply_child_env`で環境変数を渡す（`~/.sasacode/.env`から読んだキーを子プロセスに渡さないため）。長い出力は`save_output`で`~/.sasacode/tmp`に保存できる。

1回の応答では、フックが次の順に呼ばれる。

```
before_request → stream_delta（生成中、差分ごと） → assistant_message
  → ツール呼び出しごとに: tool_call_raw → 検索・検証 → tool_call → ルールとモードの判定 → permission → （agentモードのモデル判定） → 実行 → tool_result
  → turn_end
```

## これまでの経緯

- 0.15まではTypeScriptのプラグインを`~/.sasacode/plugins`やプロジェクトの`.sasacode/plugins`に置いて読み込み、`sasacode plugin search|install|update|remove|list|publish`で探す・入れる・公開することができた。型はnpmの`@sasacode/plugin-api`で配っていた。
- 1.xの公開プラグインAPI（1.4.0で互換を約束し、最後の版は1.15.0）は、0.20のRustへの書き換えで終わった。外部のプラグインを使う人がいなかったため、プラグインのローダー、`sasacode plugin`、npmの`@sasacode/plugin-api`をなくし、外からの拡張はMCPとSkillsだけにした（`rust/DECISIONS.md`のADR-002）。
- 同じAPIは、同梱プラグインのための内部のAPIとして残っている。組み込みのツールやコマンドも、これまでどおりこのAPIで登録する。`PLUGIN_API_VERSION`は記録のために`1.15.0`のまま置いている。
- 公式プラグイン（ask-user、git-context、project-checks、workflows）とjev-guard、free-searchは、既定で無効の同梱プラグインになった（ADR-011）。
- 0.20.1から、ask-userは既定で読み込む。
