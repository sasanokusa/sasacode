# 実行時依存パッケージの記録

非機能要件「コアの実行時依存パッケージを最小限にし、追加時は理由を記録する」に基づく記録。

| パッケージ | 使う場所 | 理由 | 検討した代替 |
| --- | --- | --- | --- |
| `@anthropic-ai/sdk` | `@sasacode/ai` | Messages APIのストリーミング、型付きエラー、429 / 5xx / overloadedの指数バックオフ再試行（FR-L07）、`ant auth login`プロファイルからの認証をまとめて提供する。SSEパーサーと再試行を自前で書くよりコードが小さく、APIの変更にも追従しやすい | fetchと自前のSSEパーサー：thinkingの署名、`eager_input_streaming`、エラー分類を自前で追う必要がある |
| `openai` | `@sasacode/ai` | Chat CompletionsとResponsesの両方を1つのSDKで扱え、`baseURL`を変えるだけでOpenRouter / vLLM / Ollama / Command Codeに対応できる（FR-P02・P03）。再試行も内蔵 | 自前実装：Responsesのイベント種別が多く、保守の負担が大きい |
| `@modelcontextprotocol/sdk` | `@sasacode/mcp` | 公式のMCPクライアント。プロトコルのバージョン交渉、stdioとStreamable HTTPのトランスポート、セッションID、進捗通知によるタイムアウト延長を仕様どおりに扱える。単一バイナリに含まれるのはimportしているクライアント部分だけ（サーバー用のexpressなどは入らない） | 自前のJSON-RPC実装：HTTP側のセッション管理やSSEの再接続まで追うと、保守の負担が大きい |
| `@earendil-works/pi-tui` | `@sasacode/tui` | 決定事項8.2。差分描画、同期出力、IME対応エディタ、貼り付け処理、Markdown表示がそろっていて、依存は`marked`と`get-east-asian-width`の2つだけ | OpenTUI（ネイティブバイナリに依存）、Ink（Reactが入る） |

pi-tuiには、`patches/@earendil-works%2Fpi-tui@0.87.1.patch`を、ルートの`package.json`の`patchedDependencies`で0.87.1に当てている。pi-tuiはEast Asian Ambiguous幅の文字（①、○ など）を常に1セルとして数えるので、これを2セルで表示する端末（CJK向けの設定）で行の幅がずれる。パッチは`setAmbiguousWidth(1 | 2)`を追加して`get-east-asian-width`に`ambiguousAsWide`を渡すようにし、`@sasacode/tui`が端末に合わせて呼ぶ（`SASACODE_AMBIGUOUS_WIDTH=1|2`で固定できる）。pi-tuiの版を上げるときは、パッチを作り直す。

## pi-tui の Bun 上での検証（M1 冒頭）

`@earendil-works/pi-tui@0.87.1`をBun 1.4.2で次の点について検証した。

- 日本語（全角）入力が編集バッファに正しく入り、表示幅も正しく計算される
- ブラケットペースト15行が`[paste #1 +15 lines]`マーカーに畳まれ、送信時には展開される
- 全角Markdownを5文字ずつストリーミング描画しても、全画面の再描画は1回だけで、どの行も端末幅を超えない
- 擬似端末（100×40）で実モデルを使った一連の操作（ストリーミング、承認ダイアログ、中断、キュー、`/resume`）

未検証なのは、実際のIME変換中の候補ウィンドウの位置。自動テストでは確認できないので、実端末で手動確認が必要。
