# デスクトップGUI（Rust）

sasacodeのGUIは、sasacode本体を子プロセスとして動かすRust製のネイティブアプリとして作る。ElectronとTauri（WebViewを使う方式全般）は使わない。本体をRustへ書き換える大改修の先駆けとして、本体とGUIが共有する型を先に小さなクレートにしておく。最初の版の画面は[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)（MIT）を参考にした（コードは流用していない）。今の画面は、先にHTMLのモックで決めた構成を、sasacodeが実際に出す情報に合わせて作り直したもの。

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

画面を作るうえで効いてくる、プロトコルの性質：

- 承認は1件ずつ、順に届く。1つの応答に呼び出しが複数あっても、まとめては聞かれず、それぞれ実行の直前に聞かれる。だから承認カードは1枚で足りる。bashやeditのように並行して走れないツールは、その呼び出しが終わってから次が聞かれる。readのように並行できるツールは、許可されるとすぐ走り出すので、走っている最中に次が聞かれることがある（`packages/agent/src/tool-exec.ts`）。
- 拒否された呼び出し、規則やプラグインに止められた呼び出し、中断で走らなかった呼び出しには`tool_start` / `tool_end`が出ない。あるのは結果のメッセージだけで、本文の出だし（`The user denied this tool call.`など）で見分ける。
- セッションログにはツール結果の`details`が残らない。読み込んだ会話では、差分は呼び出しの引数から作り（行番号と前後の行はない）、終了コード、所要時間、承認の記録は出せない。
- 実行中に送ったメッセージは、次のターンで新しいユーザーメッセージとしてモデルに渡る。中断のあとに渡るときは、先頭に`[The user interrupted the previous response.]`というテキストの部品が付く（ユーザーの言葉ではないので、画面には出さない）。
- ハーネスがユーザーの代わりに送る注意（`[Reminder from sasacode, not from the user] `で始まる）と、サブエージェントの終了報告も、ユーザーのメッセージとして届く。吹き出しにはせず、注記として出す。

CLIのソースが正。`crates/sasacode-protocol/tests/fixtures`に実際の実行の出力とそのセッションログを置き、`cargo test`で読めるかを確かめる。テストはCLIのソース（`crates/sasacode-agent/src/events.rs`、`session.rs`、`crates/sasacode-cli/src/control.rs`、`headless.rs`）も読み、このクレートの知らないイベントの種類が増えたら落ちる。出力やログの形を変えたら`cargo build -p sasacode-cli`のあと`SASACODE_E2E_BIN=target/debug/sasacode cargo run -p sasacode-testing --example protocol-fixtures`でフィクスチャを作り直す（`crates/sasacode-testing/examples/protocol-fixtures.rs`）。

## 使い方

```bash
cargo run -p sasacode-gui -- <フォルダ>                     # PATHの sasacode を使う
SASACODE_BIN=/path/to/sasacode cargo run -p sasacode-gui    # 実行ファイルを指定（--sasacode でも同じ）
cargo install --locked --path crates/sasacode-gui           # sasacode-gui をPATHに入れる
```

`sasacode-gui`がPATHにあれば、sasacodeからも開ける。

- `sasacode gui [フォルダ]`：そのフォルダ（省略すると今のフォルダ）で窓を開く。
- TUIの`/gui`（同梱プラグイン`gui`）：今のフォルダで窓を開き、進行中の会話を読み込んだ状態にする（`sasacode-gui --session <id>`を使う。idは`sasacode -r`と同じ読み方で、そのフォルダの会話のid、idの先頭、ログのパスのどれか）。まだ何も送っていない会話にはログがないので、新しい会話で開く。
- 窓はsasacodeとは別のプロセスとして残り、sasacodeを終えても閉じない。
- 窓が動かすsasacodeは、単一バイナリのsasacodeから開いたときはそのバイナリ（`--sasacode`で渡す）。ソースから動かしているときと、`SASACODE_BIN`があるときは渡さず、窓が自分で探す（`SASACODE_BIN`、なければPATHの`sasacode`）。
- `sasacode-gui`の場所は`~/.sasacode/config.json`の`plugins.settings.gui.command`でも指定できる。プロジェクトの設定に書いた場所は、プロジェクトを信頼するまで使わない（`sasacode gui`は信頼の確認をしないので、常に使わない）。
- 起動のあと0.6秒待ち、そのあいだにエラーで終わったときはその旨を表示する（古い`sasacode-gui`が`--session`を知らない場合など）。確実ではない：初めて動かすプログラムは起動そのものに0.6秒より長くかかることがあり、そのときは見逃す。それより後の失敗も、sasacodeの側からは見えない。
- `/gui`で開いたあとは、TUIと窓のどちらか一方で続ける。TUIは窓が足した発言を読み込まない。窓も、一覧から会話を選び直すまで、TUIがその後に足した発言を表示しない。両方から続けると、ログには両方の発言が書かれた順に並び、次に再開したときにどちらの画面にもなかった会話になる。

窓は3つに分かれる。左がそのフォルダの会話の一覧、中央が会話と入力欄、右がインスペクタ。幅が1180未満の窓では左右のどちらかだけを出す。

- 入力欄は下にある。Enterで送信、Shift+Enterで改行。実行中に送ると、次のターンでモデルに届く（届くまでは、その旨を添えて会話の末尾に出る）。
- 入力欄の下の段で、モデル（`provider/model`、空なら既定）、権限モード、推論の深さを次の実行に渡す。選んだものは下端のバーにも出る。
- 承認が要る呼び出しは、入力欄の上のカードに全文が出る（コマンド、編集の差分、それ以外は引数のJSON）。答え方は3つ：許可、この実行中は同じものを許可、拒否。入力欄に何か書いて送ると、拒否して、書いた内容を理由としてモデルに伝える。
- 終わったやりとりは、返答だけを残して手順（思考、ツール呼び出し、承認の記録）を畳む。「手順を表示」で開く。返答がないまま止まったやりとり（中断、エラー）は開いたままにする。
- インスペクタのタブは4つ：変更（writeとeditの差分）、計画（todoの最新の状態）、コマンド（bashの出力と終了コード）、使用量（入力・出力・キャッシュのトークン数、リクエスト数、直近の入力の大きさ、料金）。
- 左の一覧はそのフォルダの会話（TUIで作ったものも含む）。選ぶと読み込み、次の入力はその会話の続きになる。
- 画面の言語は `SASACODE_LANG`、なければ `LANG` で決まる（`ja` / それ以外は英語）。明暗はOSの設定に従う。
- Linuxでは実行時に `libxkbcommon-x11` が要る（多くのデスクトップ環境には入っている）。

| キー（macOS以外は`⌘`を`Ctrl`に読み替える） | 動き |
| --- | --- |
| `Enter` | 送信。承認待ちのあいだは、書いた内容を理由にして拒否する（空なら何もしない） |
| `Shift+Enter` | 改行 |
| `⌘Enter` | 承認待ちで入力欄が空なら許可。実行中に入力があれば、中断して今すぐ送る |
| `Esc` | 中断 |
| `⌘B` / `⌘I` | 会話の一覧 / インスペクタを出し入れする |
| `⌘N` | 新しい会話 |

修飾キーなしの文字やEnterだけで承認されることはない。入力欄に打つつもりのキーが、承認の答えになってはいけないから。

## コードの場所と確かめ方

| ファイル（`crates/sasacode-gui/src/`） | 中身 |
| --- | --- |
| `app.rs` | 窓の状態と、メッセージごとの更新。ショートカットもここ |
| `runner.rs` | 子プロセスの起動と、stdout / stdinの読み書き |
| `transcript.rs` | イベント（またはセッションログ）から、画面に出す会話を組み立てる |
| `view/` | 描画。`sidebar.rs`（一覧）、`timeline.rs`（会話）、`dock.rs`（承認カードと入力欄）、`inspector.rs`（右のパネル）、`mod.rs`（枠、フォント、共通の部品） |
| `theme.rs` / `icons.rs` / `i18n.rs` / `diff.rs` | 色と部品の見た目、SVGのアイコン、日本語と英語の文言、差分の計算 |
| `drive.rs` / `snapshots.rs` | テスト。下を参照 |

```bash
cargo test -p sasacode-gui                          # drive.rs：表示せずに窓をクリック・キー入力で動かし、子プロセスへ書く内容を確かめる
cargo test -p sasacode-gui -- --ignored snapshots   # 主な状態をtarget/snapshots/*.pngに描き出す（目で見る用。比較はしない）
```

フォントは名前で指定している（macOSは本文がHiragino Sans、等幅がMenlo。Windowsは等幅がConsolas）。icedの`Font::MONOSPACE`はmacOSでは使えない。等幅フォントにない文字を、cosmic-text 0.15がほかの等幅フォントから探し、漢字ではシステムの「GB18030 Bitmap」に当たる。このフォントは字の幅が読めず、漢字が消えて行が崩れる。Linuxは今のところicedの既定のまま（未確認）。返答のMarkdownにも本文のフォントを渡している（渡さないとsans-serifという種類の指定になり、太字の漢字とかなが別々のフォントから取られる）。コードブロックは自前で等幅に描いている（icedはハイライトを使わないとき、コードブロックの行を本文のフォントで描く）。

## 未決事項

- 本体とGUIを別々のバイナリで配るか、`sasacode gui install` で後から入れる形（`web-search install` と同じ）にするか。`sasacode gui [フォルダ]`を窓を開くコマンドにしたので、`gui install`にすると`install`という名前のフォルダを開けなくなる（別の名前にするか、`./install`と書いてもらう）。
- 入力ごとの再起動で体感が許せるか。遅ければ、`--control stdio` に「`agent_end` の後も終了せず次の入力を待つ」モードを足す。
- TUIとGUIが同じセッションを同時に開いたときの扱い。`/gui`は進行中の会話を窓で開くので、この状態になりやすい。今は防がず、開いたときに「続きはどちらか一方で進める」と表示するだけ。
- `sasacode gui`と`/gui`からの起動で確かめたこと（macOS、2026-11-16、偽のモデルサーバー）：上の`cargo install`で入れた`sasacode-gui`が、PATHから見つかって`sasacode gui <フォルダ>`で起動し、sasacodeの終了後も残る。その窓から送信すると返答が表示される。その会話をTUIで再開して`/gui`を打つと`--session <id>`つきで起動し、TUIの終了後も残る。その窓は会話を読み込んだ状態で開き、続きを送ると同じ会話のログに追記される。確かめていないこと：WindowsとLinuxで、sasacodeの終了後に窓が残るか（起動のしかたはバックグラウンド実行と同じ`detached`）。
- 実モデルでの確認、実際のIME（変換中の文字と候補の位置）の確認、WindowsとLinuxでの見た目の確認。macOSでは、偽のモデルサーバーを相手に実際の窓で、送信、承認の3つの答え、理由つきの拒否、実行中の追加送信、中断、会話の読み込みと続き、エラーの表示まで通した（2026-11-16）。
- アプリのバンドルにして`open`で起動したとき、実行の子プロセスが何も出力しないまま止まった（シェルから直接起動すると動く）。原因は確かめていない。候補は、新しいアプリとして扱われたことによる、macOSの書類フォルダへのアクセス許可の確認待ち（このときsasacodeのソースは`~/Documents`の下にあった）。配布（G4）の前に確かめる。
- 実際の窓で確かめていないもの：窓の幅を変えたときの配置（描画のテストでは確認）、明るい配色（同）、モデルの欄からの`-m`、「続けてください」のボタン、返答の中のリンク、会話の検索。
- モックにあって作っていないもの：コマンドパレット、`@`でのファイル名の補完、モデルを一覧から選ぶメニュー、パネルの幅の変更。
