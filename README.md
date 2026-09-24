# sasa-code-harness

TypeScript (Bun) で作るターミナル向けコーディングエージェントのハーネス。コアは「エージェントループ・最小ツール・TUI」だけで、それ以外はプラグインで足す。要件は [docs/requirements.md](docs/requirements.md)。

現在の到達点は **M4**。要件定義書のマイルストーン M0〜M4（骨格、日常利用、プラグインAPI、MCP・Skills・ツール遅延ロード、同梱プラグインと単一バイナリ配布）まで実装済み。

## インストール

```bash
curl -fsSL https://sasanokusa.com/sasacode/install.sh | sh
```

macOS（arm64 / x64）と Linux（x64 / arm64、glibc / musl、AVX2 のない CPU 向けの baseline 版）の単一バイナリを `~/.local/bin/sasacode` に置く。Bun は不要。`SASACODE_VERSION=v0.4.0` で版を固定でき、`SASACODE_INSTALL_DIR` で置き場所を変えられる。同じファイルは [GitHub Releases](https://github.com/sasanokusa/sasacode/releases) にもある。

**ソースから**（[Bun](https://bun.sh) 1.4 以上が必要）：

```bash
git clone https://github.com/sasanokusa/sasacode && cd sasacode
bun install
ln -s "$PWD/packages/cli/src/main.ts" ~/.local/bin/sasacode   # PATH の通ったディレクトリへ
```

npm 向けのパッケージ情報（`@sasacode/cli` ほか）は用意してあるが、npm にはまだ公開していない。

## 使い方

```bash
sasacode                        # 対話モード（TUI）
sasacode -p "テストを直して"       # ヘッドレス（結果を stdout へ）
sasacode -p "..." --output jsonl  # 全イベントを JSONL で
sasacode -c                     # このディレクトリの直近セッションを再開
```

主なオプション: `-m <provider/model>`、`--permission <edits|ask|agent|auto>`、`--thinking <off|low|medium|high|xhigh|max>`、`--max-turns <n>`、`-r <session id>`、`--no-session`。`sasacode --help` で一覧。

### TUI の操作

| キー / コマンド | 動作 |
| --- | --- |
| enter / shift+enter・alt+enter | 送信 / 改行 |
| ↑↓ | 入力履歴（`~/.sasacode/history.jsonl` に保存） |
| esc | 生成・ツール実行を中断（中断した事実は次の入力でモデルに伝わる） |
| 実行中に enter | 追加メッセージをキューし、次のターンで届ける |
| ctrl+o | ツール出力・thinking の折りたたみを切替 |
| shift+tab | 権限モードを順に切替 |
| ctrl+c ×2 / ctrl+d | 終了 |
| `/model` `/resume` `/clear` `/help` `/permission` `/fork` | コアのスラッシュコマンド |
| `/compact` `/mcp` `/skills` `/skill:<name>` `/presets` | 同梱プラグインのコマンド |

## プロバイダーとモデル

モデルは `<provider>/<model>` で指定する。組み込みプロバイダー:

| provider | API | APIキーの環境変数 |
| --- | --- | --- |
| `anthropic` | Anthropic Messages | `ANTHROPIC_API_KEY`（未設定なら SDK が `ant auth login` のプロファイルを使う） |
| `openai` | OpenAI Responses | `OPENAI_API_KEY` |
| `openai-chat` | OpenAI Chat Completions | `OPENAI_API_KEY` |
| `openrouter` | Chat Completions | `OPENROUTER_API_KEY` |
| `ollama` | Chat Completions（localhost:11434） | 不要 |
| `commandcode` / `commandcode-responses` / `commandcode-anthropic` | Command Code の Chat Completions / Responses / Messages | `CMD_API_KEY` |

キーは環境変数、なければ OS キーチェーン（macOS: `security add-generic-password -s sasacode -a <provider> -w`、Linux: `secret-tool store --label sasacode service sasacode account <provider>`）から読む。`.env` にも書ける：作業ディレクトリの `.env`（Bun が自動で読む）と `~/.sasacode/.env`（どのディレクトリから起動しても読む）の2か所。

## 設定

`~/.sasacode/config.json`（グローバル）と `<project>/.sasacode/config.json`（プロジェクト）をマージし、プロジェクトが優先。権限ルールの配列は両方を連結する。

```json
{
  "model": "commandcode/deepseek/deepseek-v4-flash",
  "models": ["commandcode/deepseek/deepseek-v4-pro", "ollama/gemma4:e4b"],
  "thinking": "high",
  "providers": { "myproxy": { "api": "openai-chat", "baseUrl": "https://…/v1", "apiKeyEnv": "MY_KEY" } },
  "modelOverrides": { "myproxy/some-model": { "contextWindow": 200000 } },
  "permissions": {
    "mode": "edits",
    "allow": ["bash(git status*)", "bash(bun test*)"],
    "ask": ["bash(git push*)"],
    "deny": ["bash(rm -rf *)", "edit(.env)"]
  },
  "instructions": "システムプロンプトに追記するテキスト",
  "maxTurns": 0,
  "trustedProjects": ["/path/to/project"],
  "mcpServers": { "name": { "command": "…" } },
  "plugins": { "disabled": ["web-fetch"], "settings": { "permission-presets": { "presets": ["guard", "tests"] } } },
  "tools": { "disabled": [] },
  "toolSearch": { "mode": "auto", "percent": 10, "count": 30 }
}
```

- `models` は `/model` の選択肢。
- ルールは `ツール名` または `ツール名(パターン)`。bash はコマンド文字列に `*` ワイルドカード、read/write/edit はパス glob で照合する。
- 判定の優先順位は deny → ask → allow → モード。allow ルールで許可されるのは、`&&` `;` `|` などで連結されたコマンドの**すべての断片**がルールに一致した場合だけ。`$(...)`、バッククォート、`>` を含むコマンドは allow ルールでは許可されない。
- クローンしたリポジトリが勝手に承認を外せないよう、プロジェクト設定の `mode: auto|agent` と `allow` は、グローバル設定の `trustedProjects` に入っているプロジェクトでだけ有効になる。

### 権限モード

| モード | 動作 |
| --- | --- |
| `edits`（既定） | 作業ディレクトリ内の read / write / edit は自動許可、bash やディレクトリ外への操作は確認 |
| `ask` | 読み取りを含むすべてのツール実行で確認 |
| `agent` | 作業ディレクトリ内の read は自動許可、それ以外は現在のモデルに危険度を判定させ、安全なら許可、そうでなければ確認 |
| `auto` | すべて許可（deny ルールだけは有効） |

ヘッドレスでは確認できる人がいないので、「確認」になった呼び出しは実行せず、その理由を tool_result としてモデルに返す。

## プラグイン・MCP・Skills

コアに入っているのは、ループ、プロバイダー、4つの組み込みツール、TUI、セッション、権限だけ。それ以外はすべてプラグインで、組み込みツールも同じ公開 API で登録している。書き方は [docs/plugins.md](docs/plugins.md) を参照。

### 同梱プラグイン（`plugins.disabled` で外せる）

| 名前 | 内容 |
| --- | --- |
| `agents-md` | `~/.sasacode/AGENTS.md` と、リポジトリのルートから作業ディレクトリまでの各 `AGENTS.md` をシステムプロンプトに加える |
| `compaction` | コンテキストが 80%（`threshold`）に達するか上限に達したら、古い履歴を要約して置き換える。`/compact` で手動実行 |
| `subagent` | `task` ツール。独立した履歴のサブエージェントに作業を任せ、最終報告だけを受け取る。1ターンに複数あれば並行実行する |
| `todo` | `todo_write` ツール。作業計画をセッションに保存し、ステータス行に進捗を出す |
| `web-fetch` | `web_fetch` ツール。URL を取得してテキストにする（ネットワークを使うので確認対象） |
| `browsr` | [browsr-4-agent](https://github.com/sasanokusa/browsr-4-agent) による Web 検索（`search`）と本文の閲覧（`open`）。`browsr-agent` が PATH にあれば、MCP サーバーとして自動で起動する（`uv tool install browsr-4-agent && browsr-agent setup`）。`plugins.settings.browsr` の `command` / `mode`（`standard` / `single`）/ `config` で変えられる |
| `permission-presets` | 権限ルールのプリセット。既定で `guard`（sudo、`rm -rf ~`、force push、`\| sh` などを常に拒否）。ほかに `read-only-shell`、`tests` |
| `skills` | Agent Skills（`SKILL.md`）のアダプタ |
| `mcp` | MCP のアダプタ |

### MCP

```json
{
  "mcpServers": {
    "github": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" } },
    "remote": { "url": "https://example.com/mcp", "headers": { "Authorization": "Bearer ${REMOTE_TOKEN}" } }
  }
}
```

stdio と Streamable HTTP に対応。ツールは `mcp__<server>__<tool>` という名前になり、通常のツールと同じ権限判定とフックを通る。prompts はスラッシュコマンドになる。サーバーはバックグラウンドで接続するので、起動を待たせない。

### Skills

`~/.sasacode/skills/<name>/SKILL.md`、`.sasacode/skills/<name>/SKILL.md`、プラグインの `skills` ディレクトリを探す。対応するのは標準の Agent Skills 形式（frontmatter に `name` と `description`）。システムプロンプトには名前・説明・パスだけを載せ、本文はモデルが必要になったときに read ツールで読む。`/skill:<name>` で明示的に読み込ませることもできる。

組み込みの Skill として `tool-authoring`（sasacode のツール・コマンド・プラグインの作り方のリファレンス）を同梱している。`/skill:tool-authoring 天気を返すツールを作って` のように使う。同名の Skill を `~/.sasacode/skills` に置けば、そちらが優先される。

### ツールの遅延ロード

MCP とプラグインのツールの定義が、合計でコンテキストの 10% 以上になるか、30 個以上になると、定義を送るのをやめる。代わりに名前と説明の一覧を持つ `tool_search` ツールだけを渡し、モデルは必要なツールを検索して読み込む。組み込み4ツールと `alwaysLoad` 付きのツールは常に送る。閾値は `toolSearch: { mode: "auto" | "always" | "never", percent: 10, count: 30 }` で変えられる。

## セッション

`~/.sasacode/sessions/<cwd>/<日時>_<id>.jsonl` にメッセージを1件ずつ追記保存する。`/fork` を使うと、過去の任意のメッセージの直前から新しいセッションに分岐できる。書きかけの行や、結果が揃っていないツール呼び出しは再開時に切り捨て、最後に完了したターンから再開する。使用中の API キーは書き込み前に `[REDACTED]` に置換する。`SASACODE_HOME` で `~/.sasacode` の場所を変えられる。

## 開発

```bash
bun test          # LLM なしの E2E を含む全テスト（replayProvider で応答を再生）
bun run typecheck
bun run build     # dist/sasacode（このマシン向け）。--all で全ターゲット
```

リリースの手順：`v*` タグを push すると GitHub Actions がバイナリをビルドして GitHub Release を作る（macOS 版は macOS のランナーでビルドし、起動を確認する）。その後 `scripts/publish-site.sh <version>` で sasanokusa.com/sasacode に同じファイルを置き、`latest` を切り替える。

| パッケージ | 責務 |
| --- | --- |
| `@sasacode/ai` | 正規化メッセージ型、Anthropic / OpenAI Chat / OpenAI Responses アダプタ、録画・再生プロバイダー |
| `@sasacode/agent` | ループ、イベントバス、権限判定、セッション JSONL、システムプロンプト |
| `@sasacode/plugin-api` | プラグインが依存する唯一の公開 API（semver 管理、現在 1.0.0） |
| `@sasacode/tools` | read / write / edit / bash（plugin-api 経由で登録） |
| `@sasacode/tui` | pi-tui の上に作った対話 UI |
| `@sasacode/cli` | 引数解析、設定マージ、APIキー解決、プラグインの検出・読み込み・信頼確認、ヘッドレス実行 |
| `@sasacode/mcp` | MCP アダプタ（プラグイン） |
| `@sasacode/skills` | Skills アダプタ（プラグイン） |
| `@sasacode/bundled` | 同梱プラグイン |

実行時の依存パッケージとその理由は [docs/dependencies.md](docs/dependencies.md)、各マイルストーンでの検証結果と実測値は [docs/milestones.md](docs/milestones.md)、プラグインの書き方は [docs/plugins.md](docs/plugins.md) にまとめている。
