# マイルストーン進捗

M0〜M4 のすべてで完了条件を満たした。各段階の検証方法と結果を以下にまとめる。実モデルでの検証には主に Command Code の `deepseek/deepseek-v4-flash` と、ローカルの `ollama/gemma4:e4b` を使った。

## M0 骨格 — 完了

完了条件「`sasacode -p` で小さなバグ修正が完了できる」。題材は、`mean()` の off-by-one で `bun test` が落ちる小さなプロジェクト。

| モデル（API 形式） | 指示 | 結果 |
| --- | --- | --- |
| commandcode/deepseek-v4-flash（Chat Completions） | 「bun test が失敗しています。原因を調べて直してください。」 | 自分で調査・修正・再テストして成功 |
| commandcode-responses/deepseek-v4-flash（Responses） | 同上 | 成功 |
| ollama/gemma4:e4b（Chat Completions） | 手順を具体的に指示 | 成功（曖昧な指示だとツールを使わずに聞き返してきた。モデル性能の問題） |
| ollama/gemma4:e4b（Anthropic Messages 互換） | ツール呼び出しの往復 | 成功（thinking を含む） |

## M1 日常利用 — 完了

完了条件「作者がこのハーネス自身の開発に使える」。このリポジトリの `.sasacode/config.json` で Command Code のモデルを既定にし、リポジトリ内でソースを読んで説明させる操作と、`.env` を deny ルールでブロックする動作を確認した。

| 要件 | 確認方法 |
| --- | --- |
| FR-U01〜U04（エディタ、ストリーミング表示、コアコマンド、ヘッドレス） | 擬似端末（100×40）での実モデル操作と、テスト |
| FR-S01（JSONL 追記保存、クラッシュ後の復元） | テスト（書きかけの行と、結果の揃っていないツール呼び出しを切り捨てる）、`-c` での再開 |
| FR-C01・C02（2階層設定、環境変数 / .env / キーチェーン） | テスト、実機 |
| FR-A01〜A04（4つの権限モード、ルール、承認 UI、切替） | テストと実機（agent モードで `ls` は自動許可、`rm -rf ~/…` は確認に回る） |
| FR-L01〜L07（ループ） | テスト（並行実行、中断、キュー、コンテキスト上限、エラー、再試行は SDK の `maxRetries`） |

## M2 プラグインAPI — 完了

完了条件「コアが特権 API なしで動く」。

- 組み込み4ツールは `builtin-tools` プラグインとして、`PluginHost` が発行する公開 `PluginAPI` の `registerTool` で登録している。TUI のコアコマンド（`/model` `/resume` `/clear` `/help` `/permission` `/fork`）も、`core-commands` プラグインとして `registerCommand` で登録している。
- 公開 API は 5.2 の表をすべて実装した：`registerTool`、`registerCommand`、`on`、`registerProvider`、`ui.*`、`session.*`、`agent.*`。加えて `permissions.addRules` と `settings`。
- フックは 5.3 の表をすべて実装した。ハンドラは登録順に直列で実行し、例外は `plugin_error` として表示してループは続ける。
- P3：同名で登録すれば組み込みツールも置き換わる。`tools.disabled` と `plugins.disabled` で外せる（テストあり）。
- プラグインのパッケージ（5.1）：`plugin.json` または `package.json` の `sasacode` フィールドに書く。npm と git URL からのインストール、ビルド不要の TS 読み込み、`apiVersion` による semver 互換チェック、`@sasacode/plugin-api` の import 解決に対応。
- FR-S02（分岐）は `/fork`、FR-S03（プラグインの状態の保存）は `session.append` / `entries`。

## M3 エコシステム — 完了

完了条件「既存の MCP サーバーと SKILL.md がそのまま動く」。

| 項目 | 確認方法 |
| --- | --- |
| MCP stdio | 公式 SDK で書いたテスト用サーバー：ツールの一覧取得と呼び出し、エラー、大きい出力の切り詰め（ファイルに退避）、prompts のコマンド化。単一バイナリからも実モデル経由で呼び出せた |
| MCP Streamable HTTP | 同じサーバーを HTTP で公開し、`${VAR}` で展開した Authorization ヘッダーで接続 |
| 起動できないサーバー | 通知を出すだけで、他は止まらない |
| Skills | 標準の frontmatter（折り返しや引用符を含む）、グローバルとプロジェクトの優先順、システムプロンプトには一覧だけ載せる段階的開示、`/skill:<name>`、SKILL.md の読み取り許可 |
| ツール遅延ロード | テスト：31 個で遅延に切り替わり、検索で読み込むと以後の定義に含まれる。閾値の設定、`never` / `always`。実モデル：40 個のツールから、`tool_search` で `svc_pagerduty` を選んで呼び出した |

Anthropic ネイティブの tool search（Should）は未対応で、全プロバイダーでハーネス側の `tool_search` を使う。

## M4 同梱プラグイン — 完了

完了条件「単一バイナリで配布できる」。

- `bun run build` で `dist/sasacode`（約 65MB）ができる。`node_modules` のない場所で次の点を確認した：ヘッドレス実行、`~/.sasacode/plugins` の TS プラグイン（`@sasacode/plugin-api` の import を含む）、MCP stdio サーバー、パイプ入力、TUI の起動。
- `.github/workflows/release.yml`：`v*` タグを push すると、7 種類のバイナリ（darwin-arm64 / darwin-x64 / linux-x64 / linux-x64-baseline / linux-arm64 / linux-x64-musl / linux-arm64-musl）をビルドして GitHub Release に添付する。macOS 版は macOS のランナーでビルドし、起動を確認する。
- npm 向け：各パッケージに公開用のメタデータを入れ、`bun pm pack` での梱包を確認した。まだ公開はしていない。
- 同梱プラグイン：`agents-md`、`compaction`、`subagent`、`todo`、`web-fetch`、`permission-presets`。どれもテストがあり、TUI で実モデルを使って確認した（TODO の表示とステータス行、サブエージェント、`/compact` の後も内容を覚えていること）。

### M4 以降の追加

- **配布**：`curl -fsSL https://sasanokusa.com/sasacode/install.sh | sh` の1コマンドでインストールできる。インストーラーは OS・CPU・libc・AVX2 の有無を見て 7 種類のバイナリから選び、SHA256 を検証して配置し、起動できるかも確かめる。配布ファイルは GitHub Release と sasanokusa.com（Cloudflare Tunnel → Apache）の両方に置いている。
- **browsr プラグイン**：browsr-4-agent の manifest v1 を読んで互換性を確かめ、`browsr-agent serve` を MCP サーバーとして起動する。`search` / `open` を元の名前のまま公開する。実モデルで、検索 → ページを開く → 出典付きで回答、という流れを確認した。
- **組み込み Skill `tool-authoring`**：実モデルで `/skill:tool-authoring` から `now` ツールのプラグインを作らせ、テストを通し、作ったツールを sasacode から呼び出せることを確認した。
- **プラグイン API 1.1.0**：`api.ready()` を追加した（既存を壊さない変更）。ヘッドレス実行では、MCP サーバーなどの起動処理を待ってから最初のリクエストを送る。

## 非機能要件の実測（M4 時点）

| 項目 | 目標 | 実測 |
| --- | --- | --- |
| 起動から入力可能まで（プラグイン 0 個） | 300 ms 以内 | 単一バイナリで 86 ms、ソースから `bun` で約 100 ms |
| 同上（プラグイン＋MCP サーバーあり） | 入力をブロックしない | 95 ms（読み込みは起動後にバックグラウンドで行う） |
| コアの行数（ai + agent + plugin-api + tools） | 約 3,000 行 | 2,691 行 |
| テスト | LLM なしで E2E | 56 テスト（9 ファイル）。replayProvider と、テスト用の MCP サーバー、偽の browsr-agent を使う |

## 未検証・既知の制限

- **実際の Anthropic API**：Command Code の現在のプランでは Claude 系のモデルが 403 になる。Anthropic アダプタは Ollama の Messages 互換エンドポイントでしか確認していない。prompt caching、adaptive thinking、署名付き thinking の再送は、本物の API での確認が必要。
- **IME の候補ウィンドウの位置**：実端末での手動確認が必要。
- **OAuth が必要なリモート MCP サーバー**：未対応。ヘッダーでトークンを渡す方式だけ使える。
- **MCP の resources**（Could）：一覧取得と参照は未実装。ツール結果に含まれる resource は扱える。
- **クロスコンパイル**：ローカルではこのマシン向けのビルドだけを確認した。他ターゲットのビルドは release ワークフローで行う。
- **権限の承認フロー**：1ターンに複数のツール呼び出しがあるときは、全部の承認を先に取ってから実行する。
