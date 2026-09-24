# 実行時依存パッケージの記録

非機能要件「コアの実行時依存パッケージを最小限にし、追加時は理由を記録する」に基づく記録。

| パッケージ | 使う場所 | 理由 | 検討した代替 |
| --- | --- | --- | --- |
| `@anthropic-ai/sdk` | `@sasacode/ai` | Messages API のストリーミング、型付きエラー、429 / 5xx / overloaded の指数バックオフ再試行（FR-L07）、`ant auth login` プロファイルからの認証をまとめて提供する。SSE パーサーと再試行を自前で書くよりコードが小さく、API の変更にも追従しやすい | fetch と自前の SSE パーサー：thinking の署名、`eager_input_streaming`、エラー分類を自前で追う必要がある |
| `openai` | `@sasacode/ai` | Chat Completions と Responses の両方を1つの SDK で扱え、`baseURL` を変えるだけで OpenRouter / vLLM / Ollama / Command Code に対応できる（FR-P02・P03）。再試行も内蔵 | 自前実装：Responses のイベント種別が多く、保守の負担が大きい |
| `@earendil-works/pi-tui` | `@sasacode/tui` | 決定事項 8.2。差分描画、同期出力、IME 対応エディタ、貼り付け処理、Markdown 表示がそろっていて、依存は `marked` と `get-east-asian-width` の2つだけ | OpenTUI（ネイティブバイナリに依存）、Ink（React が入る） |

## pi-tui の Bun 上での検証（M1 冒頭）

`@earendil-works/pi-tui@0.87.1` を Bun 1.4.2 で次の点について検証した。

- 日本語（全角）入力が編集バッファに正しく入り、表示幅も正しく計算される
- ブラケットペースト 15 行が `[paste #1 +15 lines]` マーカーに畳まれ、送信時には展開される
- 全角 Markdown を 5 文字ずつストリーミング描画しても、全画面の再描画は 1 回だけで、どの行も端末幅を超えない
- 擬似端末（100×40）で実モデルを使った一連の操作（ストリーミング、承認ダイアログ、中断、キュー、`/resume`）

未検証：実際の IME 変換中の候補ウィンドウの位置は、自動テストでは確認できない。実端末で手動確認が必要。
