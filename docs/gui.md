# デスクトップGUI（Rust）

sasacodeのGUIは、sasacode本体を子プロセスとして動かすRust製のネイティブアプリとして作る。ElectronとTauri（WebViewを使う方式全般）は使わない。本体をRustへ書き換える大改修の先駆けとして、本体とGUIが共有する型を先に小さなクレートにしておく。画面の構成は[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)（MIT）を参考にしたが、コードは流用していない。

段階ごとの完了条件と状態は [milestones.md](milestones.md#guig0g4進行中) にある。

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
- 左の一覧はそのフォルダの会話（TUIで作ったものも含む）。選ぶと読み込み、次の入力はその会話の続きになる。
- 上の欄で、モデル（`provider/model`、空なら既定）、権限モード、推論の深さを次の実行に渡す。
- 画面の言語は `SASACODE_LANG`、なければ `LANG` で決まる（`ja` / それ以外は英語）。明暗はOSの設定に従う。
- Linuxでは実行時に `libxkbcommon-x11` が要る（多くのデスクトップ環境には入っている）。

## 未決事項

- 本体とGUIを別々のバイナリで配るか、`sasacode gui install` で後から入れる形（`web-search install` と同じ）にするか。
- 入力ごとの再起動で体感が許せるか。遅ければ、`--control stdio` に「`agent_end` の後も終了せず次の入力を待つ」モードを足す。
- TUIとGUIが同じセッションを同時に開いたときの扱い。
- 実モデルでの確認、実際のIME（変換中の文字と候補の位置）の確認、macOSとWindowsでの確認。
