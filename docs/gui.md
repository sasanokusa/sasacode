# デスクトップGUI（Rust）

sasacodeのGUIは、sasacode本体を子プロセスとして動かすRust製のネイティブアプリとして作る。ElectronとTauri（WebViewを使う方式全般）は使わない。本体をRustへ書き換える大改修の先駆けとして、本体とGUIが共有する型を先に小さなクレートにしておく。

段階ごとの完了条件と状態は [milestones.md](milestones.md#guig0g4進行中) にある。

## 画面の考え方

ほかのハーネスのGUIを小さく写したものにはしない。頼んだ人が読む順に並べ、いま必要なものだけを前に出す。

- 会話は「依頼」と「その答え」の組で並べる。間の作業（思考、ツールの呼び出し、承認）は、実行中はそのまま流し、答えが出たら「作業の記録 6件」の1行に畳む。押せば開く。
- 承認は入力欄のすぐ上に出す。コマンドはそのまま、編集と書き込みは消える行と入る行で見せる。Ctrl+Enter（macOSは⌘Enter）でも許可できる。
- 実行中は入力欄の下に、いま何をしているかと経過時間を出す。エージェントの作業リスト（`todo_write`）は、進み具合といまの項目を入力欄の上に1行で出す。
- 実行中に送った指示は、モデルに届くまで入力欄の上で待たせ、届いたら会話に入れる。届かないまま実行が終わったら入力欄に戻す。
- 終わった依頼の下に、かかった時間と変更したファイルを出す。答えとコードブロックはコピーできる。
- モデル、権限、推論の深さは入力欄の中に置く。次の送信に効く設定なので、送信ボタンの並びにある。使ったモデル名は覚えておき、次からは一覧から選べる。
- 文言は [yomiyasu](https://github.com/nanaism/yomiyasu) の決まりに従う。誰が何をするかを述語まで書き、飾りの記号やダッシュを使わない。

色は `style.rs`、文言は `i18n.rs`、状態は `app.rs`、見た目は `view.rs`、会話のデータは `transcript.rs` にある。

## 構成

| 部品 | 場所 | 役割 |
| --- | --- | --- |
| `sasacode-gui` | `crates/sasacode-gui` | 窓。入力ごとに `sasacode -p <入力> --output jsonl --control stdio`（続きなら `-r <id>`）を作業フォルダで起動し、stdoutのイベントを画面に出し、承認・追加の指示・中断をstdinに書く |
| `sasacode-protocol` | `crates/sasacode-protocol` | イベント、制御メッセージ、セッションログの型と、セッションの一覧。将来のRust版コアも同じ型を使う想定 |
| sasacode本体 | `packages/` | これまでどおり。エージェントのループ、ツール、権限確認、セッションの保存はすべてこちらに残るので、TUIとGUIで振る舞いがずれない |

中継のサーバーは置かない。Rust版のコアができたら、GUIは子プロセスをやめてコアのクレートを直接使う形に移れる。そのときも画面側は `sasacode-protocol` の型だけを見ているので、書き換えは接続部分（`runner.rs`）に収まる。

## プロトコル

GUIが使うのは、すでにあるヘッドレスの出力と制御だけ（[README](../README.md#ヘッドレスの制御)）。

| 画面 | 使うもの |
| --- | --- |
| 応答のストリーミング | `message_start` / `message_update` / `message_end` |
| ツールのカード | `tool_start` / `tool_update` / `tool_end` |
| 承認 | `approval_request` → stdinの `approval`（allow / always / deny、拒否の理由） |
| 実行中の追加の指示・中断 | stdinの `message` / `abort` |
| トークン数・料金 | `message_end` の `usage` と `usage` イベント |
| 会話の続き | `session` 行のIDを覚え、次の入力を `-r <id>` で起動 |
| 会話の一覧と読み込み | `$SASACODE_HOME/sessions/<フォルダ名>-<ハッシュ>/*.jsonl` を直接読む |

TS側が正。`crates/sasacode-protocol/tests/fixtures` に実際の実行の出力とそのセッションログを置き、`cargo test` で読めるかを確かめる。テストはTSのソース（`events.ts`、`control.ts`、`session.ts`）も読み、Rust側の知らないイベントの種類が増えたら落ちる。出力やログの形を変えたら `bun crates/sasacode-protocol/tests/fixtures/generate.ts` でフィクスチャを作り直す。

## 使い方

```bash
cargo run -p sasacode-gui -- <フォルダ>                     # PATHの sasacode を使う
SASACODE_BIN=/path/to/sasacode cargo run -p sasacode-gui    # 実行ファイルを指定（--sasacode でも同じ）
```

- Enterで送信、Shift+Enterで改行。実行中に送ると、次のターンでモデルに届く。
- Ctrl+Enter（macOSは⌘Enter）は、承認待ちなら許可、そうでなければ送信。Ctrl+N で新しい会話、Ctrl+. で中断。
- 左の一覧はそのフォルダの会話（TUIで作ったものも含む）。選ぶと読み込み、次の入力はその会話の続きになる。6件以上あると検索欄が出る。
- 入力欄の中で、モデル（`provider/model`）、権限モード、推論の深さを選ぶ。モデルは、この窓で使ったものと、このフォルダの会話が使ったものから選べる。一覧にないものは「モデル名を入力する」で入れる。
- 選んだモデルと推論の深さ、使ったモデル名は `$SASACODE_HOME/gui.json` に覚える。権限は `edits` と `ask` だけ覚え、`agent` と `auto` は窓を開くたびに選び直す。
- 画面の言語は `SASACODE_LANG`、なければ `LANG` で決まる（`ja` / それ以外は英語）。明暗はOSの設定に従う（`SASACODE_THEME=light|dark` で固定できる）。
- Linuxでは実行時に `libxkbcommon-x11` が要る（多くのデスクトップ環境には入っている）。

## 未決事項

- 本体とGUIを別々のバイナリで配るか、`sasacode gui install` で後から入れる形（`web-search install` と同じ）にするか。
- 入力ごとの再起動で体感が許せるか。遅ければ、`--control stdio` に「`agent_end` の後も終了せず次の入力を待つ」モードを足す。
- TUIとGUIが同じセッションを同時に開いたときの扱い。
- 実際のIME（変換中の文字と候補の位置）の確認、Windowsでの確認。
- モデルの一覧をプロバイダーから取る方法。いまは使ったことのあるモデルだけを出す。CLIに一覧を出力するコマンドができたら、それを読む。
- シェルの関数や `.zshrc` だけで設定したAPIキーは、Finderなどから開いたGUIの子プロセスに渡らない。`$SASACODE_HOME/.env` に書けば渡る。
