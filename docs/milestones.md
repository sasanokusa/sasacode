# マイルストーン進捗

## M0 骨格 — 完了

完了条件「`sasacode -p` で小さなバグ修正が完了できる」を満たした。題材は、`mean()` の off-by-one で `bun test` が落ちる小さなプロジェクト。

| モデル（API 形式） | 指示 | 結果 |
| --- | --- | --- |
| commandcode/deepseek-v4-flash（Chat Completions） | 「bun test が失敗しています。原因を調べて直してください。」 | 自分で調査・修正・再テストして成功 |
| commandcode-responses/deepseek-v4-flash（Responses） | 同上 | 成功 |
| ollama/gemma4:e4b（Chat Completions） | 手順を具体的に指示 | 成功（曖昧な指示だとツールを使わずに聞き返してきた。モデル性能の問題） |
| ollama/gemma4:e4b（Anthropic Messages 互換） | ツール呼び出しの往復 | 成功（thinking を含む） |

## M1 日常利用 — 完了

完了条件「作者がこのハーネス自身の開発に使える」：このリポジトリの `.sasacode/config.json` で Command Code のモデルを既定にし、リポジトリ内でソースを読んで説明させる操作と、`.env` を deny ルールでブロックする動作を確認した。

| 要件 | 状態 | 確認方法 |
| --- | --- | --- |
| FR-U01 エディタ（複数行・履歴・貼り付け） | ✅ | pi-tui の Editor。履歴は `~/.sasacode/history.jsonl` に保存 |
| FR-U02 ストリーミング、ツールの折りたたみ表示 | ✅ | 擬似端末での操作。ctrl+o で展開、edit の差分は色付き |
| FR-U03 `/model` `/resume` `/clear` `/help` `/permission` | ✅ | 擬似端末での操作 |
| FR-U04 ヘッドレス（text / JSONL） | ✅ | 実モデル＋テスト |
| FR-S01 JSONL 追記保存、クラッシュ後の復元 | ✅ | テスト（書きかけの行と未完了のツール呼び出しを切り捨てる）、`-c` での再開 |
| FR-C01 2階層設定 | ✅ | テスト |
| FR-C02 環境変数 / キーチェーン | ✅ / ⚠️ | 環境変数は実機で確認。キーチェーンは実装済みだが、実際のキーチェーン項目での動作は未確認 |
| FR-A01〜A04 権限（4モード、ルール、承認UI、/permission・フラグ） | ✅ | テスト＋実機（agent モードで `ls` は自動許可、`rm -rf ~/…` は確認に回る） |
| FR-L01〜L07 ループ | ✅ | テスト（並行実行、中断、キュー、コンテキスト上限、エラー）。L07 の再試行は各 SDK の `maxRetries`（既定 8） |

## 非機能要件の実測（M1 完了時点）

| 項目 | 目標 | 実測 |
| --- | --- | --- |
| 起動から入力可能まで（プラグイン 0 個） | 300 ms 以内 | 約 100 ms（擬似端末で最初の完全な画面が出るまで、5 回の中央値） |
| コアの行数（ai + agent + plugin-api + tools） | 約 3,000 行 | 2,017 行（TUI と CLI を含めると 3,148 行） |
| テスト | LLM なしで E2E | 24 テスト。replayProvider で応答を再生 |

## 未検証・既知の制限

- **実際の Anthropic API**：Command Code の現在のプランでは Claude 系のモデルが 403（MODEL_NOT_IN_PLAN）になる。Anthropic アダプタは Ollama の Messages 互換エンドポイントでしか確認していない。prompt caching、adaptive thinking、署名付き thinking の再送は、本物の API での確認が必要。
- **IME の候補ウィンドウの位置**：実端末での手動確認が必要。
- **画像入力**：read ツールが画像を返すところと、各アダプタでの変換は実装済み。実モデルでの確認はしていない。
- **権限の承認フロー**：1ターンに複数のツール呼び出しがあるときは、全部の承認を先に取ってから実行する。
