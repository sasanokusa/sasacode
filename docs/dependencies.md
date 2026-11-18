# 実行時依存の記録

非機能要件「コアの実行時依存パッケージを最小限にし、追加時は理由を記録する」に基づく記録。0.20からは、配布する`sasacode`コマンド（`crates/sasacode-cli`と、それが使う`sasacode-*`のクレート）のCargoの依存を書く。テストだけで使うもの（`[dev-dependencies]`の`rmcp`、`axum`、`tempfile`など）は入れない。

| クレート | 使う場所 | 理由 | 検討した代替 |
| --- | --- | --- | --- |
| `tokio` | すべて | 非同期のランタイム（マルチスレッド）。子プロセス、タイマー、ファイル、標準入出力（`rust/DECISIONS.md`のADR-010） | — |
| `tokio-util` | ai、plugin-api、agent、tools、mcp、bundled、cli | `CancellationToken`を、TS版の`AbortSignal`の代わりに使う（ADR-010） | — |
| `futures` | ai、plugin-api、agent、mcp、bundled、tui、cli | `BoxFuture`（プラグインとフックの型）とストリームの組み合わせ | — |
| `async-trait` | ai、plugin-api、agent、tools、mcp、bundled | `Tool`や`Provider`のように、オブジェクトとして持つ非同期のトレイト（ADR-010） | — |
| `anyhow` | すべて | メッセージになるだけのエラー（ADR-010） | — |
| `thiserror` | ai | プロバイダーと通信のエラーを種類で分ける（再試行の判断と表示に使う） | — |
| `serde` | すべて | 設定、セッションのJSONL、APIの本文を型に読み書きする | — |
| `serde_json` | すべて | JSONの読み書き。`preserve_order`でキーの順をTS版の`JSON.stringify`と同じにし、`float_roundtrip`で保存した数値を読み戻したときに一致させる（ADR-003） | — |
| `indexmap` | agent、mcp、skills、host、bundled、tui、cli | 入れた順を保つ表（`mcpServers`の順、Skillsの一覧、セッションの索引など） | — |
| `reqwest` | ai、mcp、bundled、cli | モデルのAPI、MCPのStreamable HTTP、`web_fetch`、更新の取得。SSEはこの上に自前のパーサーで読む（ADR-010） | 各社のSDK：使わない。TS版でSDKが暗黙にしていた再試行・タイムアウト・ヘッダーは`rust/research/llm-sdks.md`で洗い出し、自前で同じにした |
| `rustls` | ai | TLS。Rustだけで持ち、OSのOpenSSLに依存しない（暗号は`ring`） | — |
| `hyper` | ai | 接続が切れたエラー（`hyper::Error`）を、ほかの本文のエラーと見分けるためだけ。reqwestがすでに使っている | — |
| `bytes` | ai、bundled | ストリームで受け取る本文 | — |
| `http` | bundled | `diagnostics`が観測した応答を、元のバイト列のまま組み立て直す | — |
| `url` | ai、bundled、tui、cli | URLの組み立てと解釈 | — |
| `percent-encoding` | bundled | URLのエンコードとデコード（RustXNGのリリースの名前など） | — |
| `regex` | ai、plugin-api、agent、bundled、tui、cli | 正規表現 | — |
| `fancy-regex` | plugin-api、bundled | TS版の正規表現が使っていた先読み・後読み・後方参照（`regex`にはない。`rust/PORTING.md`）。引数の検証の`pattern`と`text-tool-calls`など | — |
| `globset` | agent、bundled | 権限ルールのパス（`edit(src/**)`など）の照合と、jev-guardでのglobの展開（TS版の`Bun.Glob`の代わり） | — |
| `sha2` | ai、agent、bundled、cli | セッションのフォルダ名のハッシュ（TS版と同じ名前にする）、信頼の指紋、ダウンロードしたファイルの検証など | — |
| `uuid` | plugin-api、agent、bundled | セッションとツール呼び出しのID | — |
| `chrono` | ai、agent、bundled、tui | 日時（tuiは`/resume`の日付） | — |
| `base64` | tools、bundled、tui | 画像の読み込み、チェックポイント、ChatGPTプランの認証、端末へのコピー | — |
| `encoding_rs` | tools、bundled | UTF-8でないファイル、コマンドの出力、ページの文字コード | — |
| `unicode-normalization` | agent、bundled | 権限ルールの照合（NFC / NFD）、`web_fetch`と`web_search` | — |
| `dirs` | tui | ホームディレクトリ | — |
| `which` | mcp（Windows）、bundled、cli | 実行ファイルを探す（browsr、gui、RustXNG、WindowsのMCPサーバー、`gui install`のcargo） | — |
| `nix` | plugin-api、tools、mcp、cli（Unix） | プロセスグループへのシグナル、`setsid`、ファイルの操作を、`unsafe`を自分で書かずに行う（例外は`setsid`の1か所。ADR-012） | `libc`を直接使う：`unsafe`が要る |
| `libc` | tools、tui（tuiはUnix） | エラー番号（`ENOENT`など）からTS版と同じ文面を作る。TUIでは端末の応答を待って読む | — |
| `lol_html` | bundled | `web_fetch`で、ページから本文とリンクを取り出す（ストリーミングで読むHTMLの書き換え） | — |
| `flate2` | bundled | `image-slim`：PNGの透明度を判定するため、IDATを展開する | — |
| `image` | bundled | `image-slim`：縮小とJPEG / PNGへの書き出し（TS版の`Bun.Image`の代わり）。形式はPNG / JPEG / GIF / WebPだけを入れる | — |
| `rand` | bundled | ChatGPTプランのログイン（PKCE）の乱数 | — |
| `ratatui` | tui | 全画面のTUI（ADR-010）。セルの差分とcrosstermのバックエンドに使い、行の組み立てはpi-tuiと同じ形を自前で持つ（`rust/research/tui.md`） | pi-tui（0.15まで。下の経緯） |
| `crossterm` | tui、bundled | 端末の入出力（キー、貼り付け、画面の切り替え）。bundledは`/usage`で端末の幅を読む | — |
| `pulldown-cmark` | tui | 返答のMarkdownの表示 | — |
| `unicode-segmentation` | tui | 入力欄を書記素の単位で編集する | — |
| `unicode-width` | bundled | 表示幅（`/usage`のグラフ）。ratatuiも同じ版の幅を使う | — |

MCPのクライアントは、公式のRust SDK（`rmcp`）を使わずに書いた（`crates/sasacode-mcp`）。必要なのは小さなクライアントで、`rmcp`ではプロトコルの版、clientInfo、環境変数、stderr、タイムアウトなどの既定をほとんど上書きすることになるため（`rust/research/mcp.md`）。`rmcp`はテスト用のサーバーとしてだけ使う。

コマンドラインの引数は、clapを使わず、Nodeの`parseArgs`を移した自前の解析（`crates/sasacode-cli/src/args.rs`）で読む。フラグとエラーの文面をTS版と同じにするため（ADR-010、ADR-003）。

## Rustのクレート（GUI）

デスクトップGUI（`crates/sasacode-gui`、`crates/sasacode-protocol`）が使うもの。`sasacode`コマンドには入らない（先頭の列は検査の対象外にするため、名前をコードの書式にしない）。

| クレート | 使う場所 | 理由 | 検討した代替 |
| --- | --- | --- | --- |
| serde / serde_json | sasacode-protocol、sasacode-gui | JSONLのイベントとセッションログを型に読み書きする。`type`で分けたTSの共用体を、そのまま`#[serde(tag = "type")]`で表せる | 手書きのパーサー：イベントの数が多く、TS側の変更に追いにくい |
| sha2 | sasacode-protocol | セッションのフォルダ名に入る作業ディレクトリのSHA-256（TS側の`sessionDir`と同じ名前にする） | なし（ハッシュが一致しないとセッションを見つけられない） |
| iced | sasacode-gui | ElectronやTauriのWebViewを使わず、GPU（なければCPU）で描くGUI。Elm型の更新で子プロセスのイベントを扱いやすく、Markdown表示、複数行の入力欄、IME対応（0.14〜）がそろっている。MIT。機能は`markdown`（返答の表示）、`tokio`、`advanced`（インラインコードの背景の指定）、`svg`（アイコン。絵文字や記号の文字はOSごとに形と幅が変わるので、線画をSVGで埋め込んで描く。resvgなどが入る） | egui（毎フレーム描き直す方式で、長い会話ログに工夫が要る）、gpui（Zed本体向けで単体利用の文書が少ない）、Slint（GPLv3か独自ライセンス）、Dioxus（中身がTauriと同じWebView）。アイコンは、アイコンフォントの同梱（フォントファイルを抱えることになる）と比べてSVGにした |
| iced_test | sasacode-gui（テストだけ） | 画面を表示せずに窓を描き、クリックとキー入力を送る。承認カードのボタンや入力欄の動きをテストし（`drive.rs`）、主な状態を画像に書き出して目で確かめる（`snapshots.rs`）。icedと同じ作者・同じ版。MIT | Xvfbで実際の窓を操作する：Linuxでしか動かず、座標頼みになる |
| tokio | sasacode-gui | 子プロセスの起動とstdin / stdoutの非同期読み書き。icedの`tokio`機能と同じランタイムを使う | async-std / smol：icedがtokioに対応しているので、ランタイムを2つ持たない |

## 0.15までの記録（TypeScript版）

0.15までは、次のnpmパッケージを実行時に使っていた。0.20でどれも使わなくなった。

| パッケージ | 使っていた場所 | 理由 | 0.20での置き換え |
| --- | --- | --- | --- |
| @anthropic-ai/sdk | @sasacode/ai | Messages APIのストリーミング、型付きエラー、429 / 5xx / overloadedの指数バックオフ再試行（FR-L07）、`ant auth login`プロファイルからの認証 | `reqwest`と自前のSSEパーサー、再試行。`ant auth login`のプロファイルは`sasacode-cli`が読む |
| openai | @sasacode/ai | Chat CompletionsとResponsesの両方を1つのSDKで扱え、`baseURL`を変えるだけでOpenRouter / vLLM / Ollama / Command Codeに対応できる（FR-P02・P03） | 同上 |
| @modelcontextprotocol/sdk | @sasacode/mcp | 公式のMCPクライアント。プロトコルのバージョン交渉、stdioとStreamable HTTP、セッションID、進捗通知によるタイムアウト延長 | 自前のクライアント（`crates/sasacode-mcp`） |
| @earendil-works/pi-tui | @sasacode/tui | 決定事項8.2。差分描画、同期出力、IME対応エディタ、貼り付け処理、Markdown表示がそろっていて、依存は`marked`と`get-east-asian-width`の2つだけ | `ratatui`と`crossterm` |

pi-tuiには、`patches/@earendil-works%2Fpi-tui@0.87.1.patch`を当てていた。pi-tuiはEast Asian Ambiguous幅の文字（①、○など）を常に1セルとして数えるので、これを2セルで表示する端末（CJK向けの設定）で行の幅がずれる。パッチは`setAmbiguousWidth(1 | 2)`を足して`get-east-asian-width`に`ambiguousAsWide`を渡し、`@sasacode/tui`が端末に合わせて呼んでいた（`SASACODE_AMBIGUOUS_WIDTH=1|2`で固定できる）。ほかに、貼り付けマーカーの展開、`Editor.moveToEnd()`、会話内検索の案内文の差し替え、補完一覧の列の幅を直していた。Rust版では、Ambiguous幅の扱いを`crates/sasacode-tui/src/ambiguous.rs`に持っている（`SASACODE_AMBIGUOUS_WIDTH`も同じ）。

### pi-tuiのBun上での検証（M1冒頭）

`@earendil-works/pi-tui@0.87.1`をBun 1.4.4で次の点について検証した。

- 日本語（全角）入力が編集バッファに正しく入り、表示幅も正しく計算される
- ブラケットペースト15行が`[paste #1 +15 lines]`マーカーに畳まれ、送信時には展開される
- 全角Markdownを5文字ずつストリーミング描画しても、全画面の再描画は1回だけで、どの行も端末幅を超えない
- 擬似端末（100×40）で実モデルを使った一連の操作（ストリーミング、承認ダイアログ、中断、キュー、`/resume`）

未検証なのは、実際のIME変換中の候補ウィンドウの位置。自動テストでは確認できないので、実端末で手動確認が必要。
