# sasa-code-harness

TypeScript (Bun) で作るターミナル向けコーディングエージェントのハーネス。コアは「エージェントループ・最小ツール・TUI」だけで、それ以外はプラグインで足す。要件は [docs/requirements.md](docs/requirements.md)。

現在の到達点は **M1（日常利用）**。TUI、セッション保存・再開、2階層設定、権限確認まで動く。プラグインAPI（フック等）は M2。

## インストール

[Bun](https://bun.sh) 1.4 以上が必要。

```bash
git clone https://github.com/sasanokusa/sasacode && cd sasacode
bun install
ln -s "$PWD/packages/cli/src/main.ts" ~/.local/bin/sasacode   # PATH の通ったディレクトリへ
```

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
| `/model` `/resume` `/clear` `/help` `/permission` | コアのスラッシュコマンド |

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
  "trustedProjects": ["/path/to/project"]
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

## セッション

`~/.sasacode/sessions/<cwd>/<日時>_<id>.jsonl` にメッセージを1件ずつ追記保存する。書きかけの行や、結果が揃っていないツール呼び出しは再開時に切り捨て、最後に完了したターンから再開する。使用中の API キーは書き込み前に `[REDACTED]` に置換する。`SASACODE_HOME` で `~/.sasacode` の場所を変えられる。

## 開発

```bash
bun test          # LLM なしの E2E を含む全テスト（replayProvider で応答を再生）
bun run typecheck
```

| パッケージ | 責務 |
| --- | --- |
| `@sasacode/ai` | 正規化メッセージ型、Anthropic / OpenAI Chat / OpenAI Responses アダプタ、録画・再生プロバイダー |
| `@sasacode/agent` | ループ、イベントバス、権限判定、セッション JSONL、システムプロンプト |
| `@sasacode/plugin-api` | プラグインが依存する型（M1 時点では registerTool / registerCommand） |
| `@sasacode/tools` | read / write / edit / bash（plugin-api 経由で登録） |
| `@sasacode/tui` | pi-tui の上に作った対話 UI |
| `@sasacode/cli` | 引数解析、設定マージ、APIキー解決、ヘッドレス実行 |

実行時の依存パッケージとその理由は [docs/dependencies.md](docs/dependencies.md)、M1 時点の実測値と未検証の項目は [docs/milestones.md](docs/milestones.md) にまとめている。
