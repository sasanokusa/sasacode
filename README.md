# sasacode

[English](README.en.md) · 日本語 · [ドキュメント](https://sasanokusa.com/sasacode/docs/)

同梱のプラグインを設定で組み替えられる、ターミナル向けの小さなコーディングエージェント（Rust）。

コアに入っているのは、エージェントループ、プロバイダー、4つの組み込みツール（read / write / edit / bash）、TUI、セッション、権限確認だけ。それ以外の振る舞いは、すべて同梱のプラグインがプラグインAPIを通して足す。組み込みツールも同じAPIで登録している。どのプラグインも設定で外せる。外から機能を足すには、MCPサーバーとAgent Skills（`SKILL.md`）を使う。小型モデルでも実用になるよう、形式の崩れたツール呼び出しの修復と、同じ文の繰り返しの抑止も同梱している。

0.20でTypeScript / BunからRustに書き換えた。設定ファイル、キー、セッション、CLIのフラグはそのまま使える。外部のプラグインを読み込む仕組みはなくなった（[拡張のしかた](docs/plugins.md)）。

図や設定の一覧を含む詳しい説明は [sasanokusa.com/sasacode/docs](https://sasanokusa.com/sasacode/docs/) にある。要件は [docs/requirements.md](docs/requirements.md)。マイルストーンM0〜M4はすべて完了した（検証の記録は [docs/milestones.md](docs/milestones.md)）。

## クイックスタート

```bash
curl -fsSL https://sasanokusa.com/sasacode/install.sh | sh   # ~/.local/bin/sasacode に入る
sasacode --version                                          # 初回起動で ~/.sasacode/.env ができる
$EDITOR ~/.sasacode/.env                                    # 使うプロバイダーのキーを書く
sasacode
```

`~/.sasacode/.env` には、対応プロバイダーのキーが空欄で並んでいる。使うものだけ埋めればよく、空欄は未設定として扱う。モデルを指定しなければ、キーが書かれている最初のプロバイダーの既定モデルを使う。

## インストール

```bash
curl -fsSL https://sasanokusa.com/sasacode/install.sh | sh
```

WindowsではPowerShellから入れる（`%LOCALAPPDATA%\Programs\sasacode\sasacode.exe` に入り、PATHに追加される）。

```powershell
irm https://sasanokusa.com/sasacode/install.ps1 | iex
```

- Windowsでは [Git for Windows](https://git-scm.com/download/win) が必要。`bash` ツールはそのGit Bashでコマンドを実行する（`winget install --id Git.Git -e`）。標準以外の場所に入れた場合は `SASACODE_GIT_BASH_PATH` に `bash.exe` のパスを指定する。キーは `%USERPROFILE%\.sasacode\.env` に書く（OSのキーチェーンからの読み出しはmacOS / Linuxのみ）。
- npmからも `npm install -g sasacode` で入れられる。npmのパッケージは起動用の小さなスクリプトで、初回の実行時に同じ版のバイナリをGitHub Releasesから取得してSHA256を検証し、`~/.sasacode/npm-bin` に置く。更新は `npm install -g sasacode@latest`。
- macOS（arm64 / x64）、Linux（x64 / arm64、glibc / musl）、Windows（x64 / arm64）の単一バイナリ。x64版はどれもAVX2のないCPUでも動く（0.15までのbaseline版の名前でも、同じものを置いている）。
- インストーラーもバイナリも、[GitHub Releases](https://github.com/sasanokusa/sasacode/releases) の最新版から取得してSHA256を検証する。sasanokusa.comのURLは、最新リリースの `install.sh` / `install.ps1` へのリダイレクト。
- SHA256が防ぐのは壊れたダウンロードまで。ファイルがこのリポジトリのGitHub Actionsでビルドされたものかは、v0.9.8以降のリリースに付けた署名つきの来歴で確かめられる（`gh attestation verify sasacode-darwin-arm64.tar.gz --repo sasanokusa/sasacode`。リリースから落としたファイルに対して。`install.sh` も同じ）。
- `SASACODE_VERSION=v0.20.1` で版を固定でき、`SASACODE_INSTALL_DIR` で置き場所を変えられる（どちらのインストーラーでも）。
- 更新は`sasacode update`（インストーラーと同じ検証つき。`--check`で確認だけ）。新しい版が出ていると起動時に一度だけ静かに知らせる（`"updateCheck": false`で止める）。ソースから入れた場合は、`git pull`のあと下の`cargo install`をもう一度実行する。
- Web検索（`web_search`）に使うRustXNGは `sasacode web-search install`（TUIでは `/web-search install`）で入る。公式リリースをチェックサムで確かめて `~/.sasacode/bin` に置き、再起動しなくても使える。もう一度実行すると最新版に更新する。
- デスクトップGUI（開発中）は配布物に入っておらず、`sasacode gui install`でこのマシンでビルドして入れる（[Rust](https://rustup.rs)が要る。ないときは入れ方を案内して止まる）。`~/.sasacode/gui/bin`に入り、`sasacode update`では更新されない。sasacodeを更新したら、もう一度`sasacode gui install`を実行する（[docs/gui.md](docs/gui.md)）。

ソースから使う場合（[Rust](https://www.rust-lang.org/)1.88以上）は次のとおり。

```bash
git clone https://github.com/sasanokusa/sasacode && cd sasacode
cargo install --locked --path crates/sasacode-cli   # ~/.cargo/bin/sasacode に入る
```

## 使い方

```bash
sasacode                            # 対話モード（TUI）
sasacode -p "テストを直して"           # ヘッドレス：答えを stdout に、進捗を stderr に
sasacode -p "…" --output jsonl       # 全イベントを JSONL で
sasacode -p "…" --output jsonl --control stdio   # 親プロセスが stdin で承認に答え、メッセージを送り、中断できる（[ヘッドレスの制御](#ヘッドレスの制御)）
echo "$LOG" | sasacode -p "原因は？"   # パイプした入力は -p の後ろに付く（5 秒何も来なければ読まない。--stdin で終わりまで待つ）
sasacode -c                         # このディレクトリの直近のセッションを再開
sasacode -r <id> -p "続き"            # セッション ID を指定して続ける（tmux がなくても会話を続けられる）
```

| オプション | 内容 |
| --- | --- |
| `-p, --print <prompt>` | ヘッドレスで実行する（パイプした入力はプロンプトの後ろに付く） |
| `--stdin` | `-p`と併用。パイプした入力を、時間がかかっても終わりまで待つ |
| `--output <text\|jsonl>` | ヘッドレスの出力の形式（既定`text`） |
| `--control stdio` | `-p --output jsonl`と併用。親プロセスがstdinで承認に答え、メッセージを送り、実行を中断できる（[ヘッドレスの制御](#ヘッドレスの制御)） |
| `-m, --model <provider/model>` | モデル（例`anthropic/claude-opus-5`、`openai/gpt-5.5`、`ollama/gemma4:e4b`）。プロバイダーのモデル一覧にない名前なら、近い名前を示してエラーで終わる（一覧を取れないときは確かめない） |
| `--permission <edits\|ask\|agent\|auto>` | 権限モード（既定 `edits`） |
| `--thinking <off\|low\|medium\|high\|xhigh\|max>` | 推論の深さ |
| `--max-turns <n>` | 1回の実行のターン数の上限（既定 200、0 で無制限） |
| `-c, --continue` / `-r, --resume <id>` / `--no-session` | 再開 / IDを指定して再開 / 保存しない |
| `--append-system-prompt-file <path>` | ファイルの中身（UTF-8）を、設定の `instructions` の後ろにシステムプロンプトとして足す。読めないファイルはエラーで終わり、空のファイルは何も足さない。TUIでも使える |
| `--trust-project` | プロジェクトを確認なしで信頼する（ヘッドレス向け。[プロジェクトの信頼](#プロジェクトの信頼)） |
| `--allow <rule>` | 権限ルールに合う呼び出しを、この実行のあいだ「常に許可」と答えたように許す。繰り返し指定できる。形は `approval_request` の `rules` と同じ（例 `bash(npm test)`）。入力ごとにプロセスを起動するGUIが「常に許可」を次の入力へ引き継ぐのに使う |
| `--slash` | `-p` と併用。`/<コマンド> 引数` で始まるプロンプトを、モデルに送らずにそのコマンド（同梱プラグインやSkillsのもの）として実行する（`-r <id>` で続ける会話の `/goal …` など）。知らないコマンドならエラーで終わる。コマンドの表示は `--control stdio` では `plugin_notify` などの行に、それ以外ではstderrに出る |
| `sasacode endpoint add <name> <url> [--key-env VAR] [--project]` | 自前のサーバーを足す（形式は自動判別。`--project`ならプロジェクトの設定に書く） |
| `sasacode endpoint remove <name> [--project]` / `sasacode endpoint list` | 足したサーバーを外す / 一覧 |
| `sasacode list models\|commands\|skills [--json]` | このフォルダでの実行が使えるもの。`models` はTUIの `/model` と同じ一覧（`[{"id":"provider/model","contextWindow"?,"maxContext"?,"server"?,"current"?}]`）、`skills` は `[{"name","description","path"}]`、`commands` はTUIのコマンドとSkillsのコマンド（`[{"name","description","argumentHint"?,"owner","headless"}]`）。同梱プラグインのコマンドは、読み込むとプラグインの起動時の処理が走るのでここには出さず、`--control stdio` の実行が `commands` 行で知らせる |
| `sasacode plugin list --json` | 同梱プラグインを `[{"name","source":"bundled","enabled"}]` で。`enabled` は設定（`plugins.enabled` と `plugins.disabled`）で決まる。0.20の `sasacode plugin` で残るのはこれだけで、GUIが使う |
| `sasacode trust [--yes] [--json]` | このフォルダを信頼しているかと、信頼すると有効になるもの（`{"cwd","trusted","needed","complete","items","plugins","mcpServers","endpoints","allow","mode"}`）。`--yes` で、起動時の確認に「y」と答えたのと同じく信頼を保存する。0.20はプロジェクトのプラグインを読まないので、`plugins` は空、`complete` は常に `true` |
| `sasacode login [status\|logout]` | ChatGPTプランでログイン（`openai-codex/…` のモデル用） |
| `sasacode update [--check] [tag]` | 最新のリリースを取得して入れ替える（チェックサムを検証。`--check`なら確認だけ） |
| `sasacode web-search install [tag]` | `web_search`が使うRustXNGを入れる（チェックサムを検証） |
| `sasacode gui [folder]` | デスクトップGUIをフォルダで開く（`sasacode gui install`で入れる。[docs/gui.md](docs/gui.md)） |
| `sasacode gui install [--path <dir>]` | デスクトップGUIを、このマシンでRustを使ってビルドして入れる（この版のタグから。`--path`ではチェックアウトから） |
| `-h, --help` / `-v, --version` | 使い方 / 版を出す |

`sasacode plugin`は0.20でなくなった。実行すると、プラグインはsasacodeに同梱になったこと（既定で無効のものは`plugins.enabled`で有効にする）と、ツールはMCPサーバーかSkillsで足すこと（[拡張のしかた](docs/plugins.md)）を示して、エラーで終わる。

### ヘッドレスの制御

ほかのプログラムが `sasacode -p "…" --output jsonl` を子プロセスとして動かすときは、`--control stdio` を付けると、確認が必要な呼び出しにstdinで答えたり、実行中のモデルにメッセージを送ったり、実行を中断したりできる。`-p` と `--output jsonl` の両方が必要で、欠けているとエラーで終わる。stdinは制御に使うので、パイプした入力はプロンプトに付かない。

stdoutには、これまでのJSONLのイベントに混ざって、次の行が1件ずつ出る。

```jsonc
{"type":"approval_request","id":"approval-1","callId":"call_1","tool":"bash","args":{"command":"touch x"},"reason":"…","cwd":"/path/to/project","rules":["bash(touch x)"]}
{"type":"approval_cancelled","id":"approval-1"}
{"type":"control_error","error":"not valid JSON"}
{"type":"ui_request","id":"ui-1","kind":"confirm","title":"…","message":"…"}   // kind: "confirm" | "select" | "input" | "select_many"
{"type":"ui_request","id":"ui-2","kind":"select","title":"…","options":[{"value":"a","label":"A","description":"…"}]}
{"type":"ui_request","id":"ui-3","kind":"input","title":"…","message":"…","placeholder":"…","initial":"…"}
{"type":"ui_request","id":"ui-4","kind":"select_many","title":"…","options":[…],"selected":["a"]}
{"type":"ui_cancelled","id":"ui-1"}
{"type":"plugin_notify","level":"info","text":"…"}                 // "info" | "warning" | "error"
{"type":"plugin_text","title":"…","text":"…","format":"text"}      // "text" | "markdown"
{"type":"plugin_status","key":"goal:goal","text":"…"}             // textがnullなら消える
{"type":"commands","commands":[{"name":"goal","description":"…","argumentHint":"…","owner":"goal"}]}
{"type":"judged","callId":"call_1","tool":"bash","verdict":"allow","reason":"agent judged safe: …"}   // "allow" | "ask"
```

stdinは1行1件のJSON（JSON Lines）で読む。

```jsonc
{"type":"approval","id":"approval-1","decision":"allow"}           // "allow" | "always" | "deny"
{"type":"approval","id":"approval-1","decision":"deny","feedback":"理由"}
{"type":"approval","id":"approval-1","decision":"allow","args":{"command":"touch y"}}   // 引数を直して許可
{"type":"message","text":"テストも見て"}
{"type":"abort"}
{"type":"abort","callId":"call_1"}                                // その呼び出しだけを止める
{"type":"ui_response","id":"ui-1","value":true}                   // confirmならtrue/false、selectなら選んだvalue、inputなら文字列、select_manyならvalueの配列、nullで閉じる
```

- `approval_request` は、確認が必要な呼び出しごとに1行出る。呼び出しは答えが届くまで実行しない。`id` は答えに付ける。`callId` は会話の中のtool_callのid、`tool` と `args` は呼び出しの中身、`reason` は確認が必要な理由。
- `approval` の `decision` は、`allow` が今回だけ許可、`always` が許可に加えて同じ呼び出しを許すルールの追加（TUIの「常に許可」と同じで、プロセスの終了まで有効）、`deny` が拒否。`feedback` は `deny` のときモデルに渡る。
- `approval_request` の `rules` は、`always` で足される許可ルール。次のプロセスに `--allow` で1つずつ渡すと、「常に許可」が実行をまたいで続く。
- `allow` と `always` には `args` を付けられる。呼び出しはその引数で動く（同じツール）。モデルの引数と同じように確かめ、合わなければ、またはdenyルールに当たれば、拒否としてモデルに理由を返す。モデルには、引数が直されたことと実際の引数を結果と一緒に伝える。`always` のルールは直した引数のものになる。
- `abort` に `callId` を付けると、その実行中のツール呼び出し（サブエージェント、長いコマンドなど）だけを止める。その結果はエラーになり、実行は続く。実行中でないidは `control_error`。止まるのは、中断の合図に従うツール（bash、サブエージェント、MCPなど）。
- 同梱プラグインの問いやお知らせ（`api.ui`）は親プロセスに届く。確認、選択、入力、複数選択の問いは `ui_request` を出し、同じ `id` の `ui_response` を待つ。`null` で閉じると、ヘッドレスと同じ答え（確認なら「いいえ」、ほかは答えなし）になる。実行の中断（`abort` かCtrl+C）で、答えの届いていない問いには `ui_cancelled` を出して閉じた扱いにし、stdinが閉じたら以降の問いもすべて閉じた扱いにする。お知らせは `plugin_notify`、テキストの表示は `plugin_text`、ステータス行は `plugin_status`（キーは `プラグイン名:キー`）になる。プラグインからは対話できる実行に見える（stdinが閉じるまで）。
- `commands` は、プラグインを読み込んだ後に1回と、変わるたびに出る。その実行で使えるスラッシュコマンドの一覧で、どれも `--slash` で動かせる。
- `judged` は、`agent` モードの判定（同梱プラグイン `agent-judge`）が呼び出しを決めたときに出る。`allow` はそのまま実行、`ask` は危ないか判定に失敗したので、続けて `approval_request` が出る。
- `message` は、TUIで実行中に打った入力と同じで、モデルの次のターンに届く。実行中の作業に指示を足したり、方向を変えたりするのに使う。`text` は空でない文字列。
- `abort` はCtrl+Cと同じで、実行を中断する（`agent_end` の `cause` は `aborted`、終了コードは1）。
- 実行が中断されたとき、答えが届いていない要求には `approval_cancelled` を出す。そのidへの答えは `control_error` になる。
- 読めない行（JSONでない、不明な `type`、不明な `id`、不正な値）は `control_error` を出して読み続ける。
- stdinが閉じられたら、答えを待っている要求と、以降の要求をすべて拒否する。`feedback` は `the controlling process closed stdin`。実行は中断しない。

### TUI

全画面で動く。会話は上でスクロールし、入力欄とフッターは下に固定される。終了すると端末は起動前の状態に戻り、会話の代わりに要約（使ったトークン数・リクエスト数・料金・モデル・再開コマンド）だけが残る。従来どおり会話を端末に残したいときは、設定で `"tui": { "altScreen": false }` にする。

| キー | 動作 |
| --- | --- |
| enter / shift+enter・alt+enter | 送信 / 改行 |
| ↑↓ | 入力履歴 |
| tab | 補完（スラッシュコマンド、`/model` のモデル名、ファイルパス） |
| esc | 生成・ツール実行を中断（中断したことは次の入力でモデルに伝わる）。予約したメッセージは送らずに、入力中の下書きのあとへ戻す（付けた画像も `[Image #1]` のまま戻り、送り直すと一緒に送られる。`/fork` で戻したメッセージも同じ） |
| 実行中にenter | メッセージを予約し、次のターンで届ける |
| 貼り付け | 40行・4,000文字まではそのまま表示する。それより長いと `[paste #1 +60 lines]` のようにまとめて表示し、送信するときは全文を送る。このマーカーと画像の `[Image #1]` は、Backspace・Deleteでまとめて消える |
| ctrl+o | ツール出力とthinkingの折りたたみを切り替え |
| shift+tab | 権限モードを順に切り替え |
| pageup / pagedown・ホイール | 会話をスクロール（上に戻っている間は右端にスクロールバーと「最新へ」を表示） |
| ctrl+↑ / ctrl+↓ | 前 / 次の自分の入力へ移動 |
| ctrl+home / ctrl+end | 会話の先頭 / 最新へ |
| ctrl+shift+f | 会話の中を検索（enterで次、shift+enterで前、escで閉じる） |
| ドラッグ | 選択してクリップボードにコピー（端末の標準の選択はoption / altを押しながら） |
| `/exit`・ctrl+c ×2・ctrl+d | 終了（使用量と `sasacode -r <id>` の要約を表示） |

起動時に、①のような幅の曖昧な文字（East Asian Ambiguous）を端末が何マスで表示するかを問い合わせて、表示の計算を合わせる。端末が答えない場合は1マスとして扱う。`SASACODE_AMBIGUOUS_WIDTH=1` か `2` で明示できる。

| コマンド | 内容 |
| --- | --- |
| `/model` | プロバイダーから取得したモデル一覧から選ぶ。文字を打つと絞り込める。`/model <provider/model>` で直接指定、`/model --refresh` で取り直す |
| `/resume` `/clear` `/fork` `/session` | 過去のセッション / 新しいセッション / 過去のメッセージから分岐 / IDと保存先 |
| `/effort` | 推論の深さ（`off` / `low` / `medium` / `high` / `xhigh` / `max`）を切り替える。引数なしなら一覧から選ぶ。現在の値はフッターに出る。OpenAI形式のサーバーには `reasoning_effort` として送り、`off` は `none` として送る（vLLMなどは送らないと推論する）。llama.cppでは、b10121より前のllama-serverが `reasoning_effort` を読まないので、`off` のときに `chat_template_kwargs` の `enable_thinking: false` も送る。サーバーが受け付けない値なら近い値に替えて送り直し、以後はその値を使う（例：Command Codeは `none` を受け付けないので `off` は `low` になる） |
| `/language` | 画面の言語を切り替える（`ja` / `en`。引数なしなら一覧から選ぶ）。その場で切り替わり、`~/.sasacode/config.json` の `lang` にも保存する。切り替え前に表示された文はそのまま |
| `/copy` | 直前の応答をクリップボードにコピー（pbcopy / wl-copy / xclip、なければ端末経由） |
| `/permission` `/help` `/exit`（`/quit`） | 権限モード / ヘルプ / 終了 |
| `/usage` `/goal` `/bg` `/reconnect` `/compact` `/mcp` `/skills` `/skill:<name>` `/presets` `/browsr` `/web-search` `/undo` `/gui` | 同梱プラグインのコマンド（既定で無効のプラグインを有効にすれば`/jev` `/checks` `/workflow`も） |

フッターには、モデル、推論の深さ、権限モード、コンテキストの使用量（`ctx 77.6k/256k (30%)`）、セッションIDと、プラグインのステータス（MCPの接続数、TODOの進捗など）を出す。30 秒以上かかった実行が終わると、端末のベルを鳴らす（別の作業をしていても気づけるように。`"tui": { "bell": false }` で止める）。

## プロバイダーとモデル

モデルは `<provider>/<model>` で指定する。

| provider | API形式 | キー |
| --- | --- | --- |
| `anthropic` | Anthropic Messages | `ANTHROPIC_API_KEY`（空なら環境変数`ANTHROPIC_AUTH_TOKEN`をBearerトークンとして、それもなければ`ant auth login`のプロファイルを使う） |
| `openai` / `openai-chat` | OpenAI Responses / Chat Completions | `OPENAI_API_KEY` |
| `openrouter` | Chat Completions | `OPENROUTER_API_KEY` |
| `commandcode` / `commandcode-responses` / `commandcode-anthropic` | Command CodeのChat Completions / Responses / Messages | `CMD_API_KEY` |
| `ollama` | Chat Completions（localhost:11434） | 不要 |
| `openai-codex` | ChatGPTプランのCodexバックエンド（Responses） | 不要（`sasacode login` でChatGPTアカウントにログイン） |

**ChatGPTプランで使う**：`sasacode login` を実行するとブラウザが開き、ChatGPTアカウントでログインすると `openai-codex/…` のモデル（`/model` に一覧が出る）がAPIキーなしで使える。利用分はプランのCodexの枠から引かれる。認証情報は `~/.sasacode/codex-auth.json`（本人だけが読める権限）に保存し、期限が近づけば自動で更新する。SSH先などでブラウザがこのマシンに戻れないときは、ログイン後にブラウザが表示したURLを貼り付ける。`sasacode login status` で状態を確認、`sasacode login logout` で削除。プランの残り（5時間・週の枠）は `/usage plan` で見られる。APIキーが1つもなくログインだけしている場合は、`openai-codex/gpt-5.5` が既定のモデルになる。これはOpenAIのCodex CLIと同じ仕組み（公開クライアントとPKCE）を使う非公式の対応で、OpenAIの都合で使えなくなることがある。

### 自前のサーバー（llama.cpp、vLLM、LM Studio、プロキシなど）

```bash
sasacode endpoint add llama http://192.168.1.20:8080            # 形式を判別して ~/.sasacode/config.json に保存
sasacode endpoint add mygw https://llm.example.com/v1 --key-env MYGW_KEY
sasacode endpoint list
sasacode -m llama/<モデル名>
```

`GET /v1/models`の応答から、OpenAI形式（Chat Completionsで呼ぶ）かAnthropic形式（Messagesで呼ぶ）かを自動で判別し、URLも各形式に合わせて整える。llama.cpp、Ollama、LM Studio（0.3.6以降）は、それぞれ`/props`、`/api/show`、`/api/v0/models`から実際のコンテキスト長も取る（LM Studioを見分けなかった以前のバージョンで追加したサーバーも、モデル一覧を取るときに見分ける）。追加したサーバーのモデルは、`/model`の一覧に自動で出る。設定ファイルに`"providers": { "llama": { "baseUrl": "http://…" } }`とURLだけ書いた場合も、起動時に判別する（結果は`~/.sasacode/endpoints.json`にキャッシュする）。

プロジェクトの `.sasacode/config.json` に書いたエンドポイントは、そのプロジェクトを信頼するまで使わない（[プロジェクトの信頼](#プロジェクトの信頼)）。キーを使わないサーバーでも、会話とコードが送られるため。

**モデル一覧の自動取得**：TUIの起動時に、キーのあるプロバイダー（とOllama）のModels API（`GET /v1/models`）をバックグラウンドで呼び、`/model` に一覧とコンテキスト長を出す。取得したコンテキスト長は、フッターの使用率や圧縮のしきい値にも使う（設定の `modelOverrides` があればそちらが優先）。Ollamaは独自の `/api/show` から読み、Modelfileの `num_ctx` があればそれを実際の長さとして使い、なければモデルの最大長を参考として表示する（メモリに載っているモデルは `/api/ps` の実際の長さを使う）。LM Studioも同じで、ロード済みのモデルはロードしたときの長さ（`/api/v0/models` の `loaded_context_length`）を使い、未ロードのモデルは最大長を参考として表示する（LM Studioは最大長ではなく自分の既定の長さでロードするため）。長さがまだ分からないOllamaとLM Studioのモデルは、応答の後にサーバーに聞き直し（分からなければ次の応答の後にも聞き、3回まで）、実際にロードされた長さに切り替える。それまでは既定の128Kで計画する（最大長がそれより短ければ最大長で）。`num_ctx` のないOllamaのモデルを選ぶと、Ollamaが既定の長さを超えた入力を気づかないうちに切り詰めることを一度だけ警告する。ヘッドレス（`-p`）でも、ローカルのサーバー（Ollama、LM Studio、llama.cpp）や追加したOpenAI形式のサーバーなら、実行の前に現在のモデルの長さを聞き、同じ警告を標準エラーに出す。OpenAIのModels APIはコンテキスト長を返さないので、組み込みのモデル表の値で補う。Command Codeのように1つのキーで複数のAPI形式を提供するサービスでは、モデルごとに対応する形式のプロバイダーへ自動で振り分ける（例：Claude系は `commandcode-anthropic/…`）。

**APIキー**は次の順で探す：シェルの環境変数 → `~/.sasacode/.env` → OSキーチェーン（macOS: `security add-generic-password -s sasacode -a <provider> -w`、Linux: `secret-tool store --label sasacode service sasacode account <provider>`）。どこでも空欄は無視する。作業ディレクトリの`.env`は読まない（リポジトリが`ANTHROPIC_BASE_URL`などで接続先を差し替えられないようにするため）。セッションのログには、使用中のキーを`[REDACTED]`に置き換えてから書く。`~/.sasacode/.env`にだけ書いたキーは、エージェントが実行するコマンド（bash、バックグラウンドジョブ）には渡さない。`~/.sasacode`（セッションのログ、入力履歴、長い出力の保存先`tmp/`）は本人しか読めない権限（0700）にする。

## 設定

`~/.sasacode/config.json`（全体）と`<project>/.sasacode/config.json`（プロジェクト）をマージし、プロジェクトが優先する。ただし、プロジェクト設定のうち信頼が必要な部分は、信頼するまで使わない（下記）。権限ルールと`disabled`（ツール・プラグイン）、`plugins.enabled`の配列は両方を合わせ、全体設定のdeny / askや無効化をプロジェクト側から消すことはできない。型の合わない値や未知のキーは、警告を出して無視する。

```json
{
  "model": "commandcode/deepseek/deepseek-v4-flash",
  "models": ["commandcode/deepseek/deepseek-v4-pro", "ollama/gemma4:e4b"],
  "lang": "ja",
  "thinking": "high",
  "providers": { "myproxy": { "api": "openai-chat", "baseUrl": "https://…/v1", "apiKeyEnv": "MY_KEY" } },
  "modelOverrides": { "myproxy/some-model": { "contextWindow": 200000, "images": false } },
  "permissions": {
    "mode": "edits",
    "allow": ["bash(git status*)", "bash(bun test*)"],
    "ask": ["bash(git push*)"],
    "deny": ["read(**/.env)", "edit(**/.env)", "write(**/.env)", "bash(rm -rf *)"]
  },
  "instructions": "システムプロンプトに追記するテキスト",
  "mcpServers": { "github": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"] } },
  "plugins": { "enabled": ["git-context"], "disabled": ["web-fetch"], "settings": { "permission-presets": { "presets": ["guard", "tests"] } } },
  "tools": { "disabled": [] },
  "toolSearch": { "mode": "auto", "percent": 10, "count": 30 },
  "tui": { "altScreen": true, "bell": true },
  "updateCheck": true,
  "trustedProjects": ["/path/to/project"]
}
```

`models` は `/model` の一覧の先頭に出す。`providers` でOpenAI互換やAnthropic互換のエンドポイントを足せる。`lang` は画面の言語（TUI・CLI・同梱プラグインの表示を同じ言語にする。`"ja"` / `"en"`。`/language` でも切り替えられる）。環境変数 `SASACODE_LANG` があればそちらが優先し、どちらもなければシステムのロケールで決まる。モデルに渡す文（ツールの説明や結果）は訳さない。`updateCheck` は起動時の新版の知らせ（既定true）。

**料金**：フッターや終了時の要約に出る料金は、組み込みのモデル表の価格（100万トークンあたりの米ドル）からの概算で、請求額そのものではない。OpenRouterはリクエストごとの実際の料金を返すので、それがあるときは実額を使う。価格は `modelOverrides` でモデルごとに設定・修正できる。

```json
  "modelOverrides": { "myproxy/some-model": { "price": { "input": 1.25, "output": 10 } } }
```

`maxTurns`（既定 200、0 で無制限）は、1回の依頼でモデルに送るリクエスト数の上限。達すると、対話では続けるかを尋ね、ヘッドレスでは止まる。これとは別に、同梱の `loop-guard` が、ツール呼び出しがどれも実行されない（拒否や引数の不備が続く）ターンが5回続いたら実行を止める（`plugins.settings["loop-guard"].noProgressTurns` で変更、0 で無効）。

### 権限

| モード | 動作 |
| --- | --- |
| `edits`（既定） | 作業ディレクトリ内のread / write / editは自動で許可し、bashやディレクトリ外への操作は確認する。`.git`と`.sasacode`の中への書き込みは確認する（gitのフックや設定は次のgitの実行でコードを動かせ、sasacodeの設定や権限ルールは次の起動で効くため）。名前が`HEAD`のファイルと、`HEAD`、`objects/`、`refs/`がそろったフォルダの`config`への書き込みも確認する（そのフォルダがgitのリポジトリとして扱われうるため） |
| `ask` | 読み取りを含む、すべてのツール実行を確認する |
| `agent` | 作業ディレクトリ内のreadは自動で許可し、それ以外は現在のモデルに危険度を判定させる。安全なら許可、そうでなければ確認する。判定には、設定の `judgeModel`（`"provider/model"`）で別のモデルを使える。速くて安いモデルにすると、確認の待ち時間と費用が減る。判定するモデルは呼び出しの引数も読むため、モデルを誘導する文が判定も誘導しうる。利便のための機能で、プロンプトインジェクションへの防御ではない（それにはdeny / askルールと `guard` プリセットを使う） |
| `auto` | すべて許可する（denyとaskのルールは効く）。TUIのフッターでは赤字で表示する |

- ルールは `ツール名` か `ツール名(パターン)`。bashはコマンド文字列を `*` のワイルドカードで、read / write / editはパスをglobで照合する。`*` そのものは `\*` と書く。パスのルールは書いたツールにだけ効くので、`.env` を守るならread / edit / writeを並べ、サブディレクトリも含めるには `**/` を付ける（`edit(.env)` は作業ディレクトリ直下の `.env` のeditだけを止める）。コマンドのルールはコマンドの文字列に合わせるだけなので、オプションの順を変えたり絶対パスで呼んだりすれば当たらない。間違いを防ぐ柵であって、完全な防御ではない。
- 確認の「常に許可」で足すルールは、見たものより広くならない。コマンドはそのまま（中の `*` はワイルドカードにしない）、ファイルを触るツールはそのパスだけを許可する。ルールはsasacodeを終了するまで残る（`/clear` をまたぐ）。
- 判定の順番はdeny → softDeny → ask → allow → モード → `permission`フック（同梱プラグイン）。`softDeny`はdenyと同じく拒否するが、`jev-guard`などのプラグインが「ユーザーに確認」まで下げられる（確認なしに実行されることはない）。広すぎて正当な操作も巻き込むパターン向け。allowルールで通るのは、`&&` `;` `|`でつないだコマンドの**すべて**が許可されている場合だけ。`$(…)`、バッククォート、`>`を含むコマンドは、allowルールでは通らない。
- 同梱の `permission-presets` が、既定で `guard`（sudo、`rm -rf /`、ディスク操作、`| sh` などを常に拒否。ホーム配下の `rm -rf ~…` とforce pushはsoftDeny。`ssh`、`scp`、`sftp`、リモートへの `rsync` は `auto` モードでも常に確認）を有効にしている。
- `read-only-shell`プリセット（既定では無効）は、`ls`、`cat`、`rg`、`git status`、`git diff`、`git log`、`git show`などの読み取りだけのコマンドを確認なしで許可する。許可するのはコマンドそのものか、コマンドの後に空白と引数が続く形だけで、`git difftool`のような別のサブコマンドは確認に回る。`cat`、`head`、`tail`、`rg`、`grep`などは任意のパスを読めるので、作業ディレクトリの外のファイルも確認なしで読める（パスは判定しない）。作業ディレクトリの外を読ませたくないときは、このプリセットを有効にしないか、`bash(cat *)`などのaskルールを足す（askはallowより先に効く）。`tests`プリセット（既定では無効）も同じ形で、`cargo test`や`pytest`などのコマンドそのものか、その後に空白と引数が続く形だけを許可する。`yarn test:unit`のように名前が続くものは確認に回る。
- ヘッドレスでは確認できる人がいないので、確認が必要な呼び出しは実行せず、その理由をモデルに返す。`--control stdio` を付けると、親プロセスがstdinとstdoutで答える（[ヘッドレスの制御](#ヘッドレスの制御)）。
- パスは、シンボリックリンクをたどった先（実パス）で判定する。作業ディレクトリ内のリンクが外を指していれば、リンク先がまだ存在しなくても外への操作として扱う。プロジェクト内のリンクで、ルールの対象をすり替えることもできない。リンクの循環などで行き先が決まらないパスは、モードによらず確認する。
- 大文字と小文字を区別しないファイルシステム（macOSとWindowsの既定）では、パスを大文字小文字をそろえて照合する。deny / askルールは、`ſ`と`s`のように畳み込むと同じになる文字も同じとみなすが、ファイルシステム自身の畳み込みとは完全には一致しない（`ß`と`ss`など）。allowルールは、小文字にそろえるだけで照合する。
- 権限は、ツールを呼ぶ前の判定であって隔離（サンドボックス）ではない。判定から実行までの間にリンクを差し替えるような操作や、許可したbashコマンドの中身までは防げない。信頼できないコードを扱うときは、コンテナなどOS側で隔離すること。
- 中断（esc）した後は、承認済みでもまだ始まっていないツールは実行しない。
- 確認の画面でescを押すと、その呼び出しを拒否して実行全体を中断する。「代わりの指示を伝える」の入力中のescは、選択肢に戻る。確認するコマンドが画面に収まらないときは、shift+↑↓で全文をスクロールできる（選択肢はいつも表示したまま）。

### Jevによる実行前判断（同梱・既定で無効の`jev-guard`）

ルールと権限モードの判定の後に、判断専用モデル[Jev](https://commandcode.ai/models/jev)（TypeSafe）の判定を重ねるプラグイン。既定で無効でキーも要るので、0.9.6から0.15までは別に入れるプラグインだった。0.20から再び同梱し、既定では読み込まない。いつも使うなら、設定に次のとおり書く（これまでの設定がそのまま使える）。

```json
{ "plugins": { "settings": { "jev-guard": { "enabled": true } } } }
```

`"plugins": { "enabled": ["jev-guard"] }`と書いた場合は、読み込むだけで判定は止まったままになる。TUIで`/jev on`と打つと、そのセッションで有効になる。接続先、判定の変え方、閾値などは[docs/plugins.md](docs/plugins.md#jev-guard)にある。

### プロジェクトの信頼

クローンしたリポジトリは、そのままでは「制限を強める」ことしかできない。次のものは、コードを実行するか、データの送り先を変えうるので、プロジェクトを信頼するまで使わない。

- `.sasacode/config.json`の`providers`（エンドポイント）、`modelOverrides`、`plugins`（有効化・無効化と設定）、`mcpServers`、`allow`ルール、全体設定より緩い権限モード、全体設定より大きい`maxTurns`（0は上限なし）と`maxRetries`、そのプロジェクトにしかないプロバイダーを指す`model`・`models`・`judgeModel`

これらがあるプロジェクトで起動すると、最初に一覧を出して信頼するかを尋ねる（ヘッドレスでは`--trust-project`）。答えは`~/.sasacode/trust.json`に保存し、上の設定が変わったら再び尋ねる。全体設定の`trustedProjects`に入れたプロジェクトは、尋ねずに信頼する。モデルの選択、`thinking`、`instructions`、deny / askルールやツール無効化の追加、より厳しい権限モードや小さい上限などは、信頼しなくても使う。0.15までは`.sasacode/plugins`のプラグインも信頼の対象だったが、0.20からはそのフォルダを読み込まないので、対象から外した（プロジェクトのプラグインがなく、0.15で信頼したプロジェクトは、そのまま信頼される）。

## プラグイン・MCP・Skills

### 同梱プラグイン（`plugins.disabled` で外せる）

どれも`plugins.disabled`に名前を書けば読み込まない。設定は`plugins.settings.<名前>`に書く。0.15まで`~/.sasacode/plugins`やプロジェクトの`.sasacode/plugins`に置いたプラグインは、0.20からは読み込まない（同じ名前の同梱版の代わりに読み込む仕組みもなくなった）。

| 名前 | 内容 |
| --- | --- |
| `tool-repair` | 検証の前に、形式の崩れたツール呼び出しを直す。直す対象は、JSONの崩れ（末尾カンマ、閉じ括弧、クォートのないキー、`True`/`None`、コードフェンス、二重エンコード）、キー名（`file` → `path`）、型（`"20"` → `20`）、ツール名のtypo、MCPの接頭辞を省いた名前（`get_file_contents` → `mcp__github__get_file_contents`）と、ほかのハーネスでの組み込みツールの名前（`read_file`、`str_replace_editor`、`run_command`など。`str_replace_editor`の`command`が`view` / `create`なら`read` / `write`に回す）。候補が1つに絞れるときだけ直す。直したことはモデルに短く伝え、履歴には直した後の形を残し、元の出力はセッションに記録する |
| `text-tool-calls` | ツール呼び出しを本文に書いてしまう応答（llama.cppなどで、チャットテンプレートがtool callに対応していないとき）から呼び出しを拾い、実行する。対象は、このマシンかローカルネットワークで動くモデル（`baseUrl`がプライベートアドレス、`localhost`などドットのない名前、`.localhost` / `.local` / `.lan` / `.internal`で終わる名前）だけ。クラウドのAPIのモデルはネイティブに呼び出せるので、本文の呼び出しは例とみなす（`plugins.settings.text-tool-calls`の`always: true`で、どのモデルからも拾う）。拾う形は`<tool_call>{"name": …, "arguments": …}</tool_call>`（Qwen / Hermes）、`{"name": …, "arguments": …}`（または`"parameters"`）だけを入れた`json`のコードフェンス、`<function=名前>{…}</function>`（Llama 3.x）。対象は、ネイティブの呼び出しがなく、途中で切れていない応答の、本文の末尾にある呼び出しだけ。後ろに文が続く例やコードブロック（閉じていないものも）の中の例は拾わず、`json`のコードフェンスは前の文が300字以内のときだけ拾う。ツール名は既知のものか、tool-repairと同じ規則（typoは除く）で1つに決まるものだけで、ほかは応答に触れない。署名付きのthinkingを含む応答にも触れない。崩れたJSONはtool-repairが直す。読み取り以外の呼び出しは、例かもしれないので、autoモード以外では先に確認する（editsモードなら確認なしで動く書き込みや編集も、agentモードでagent-judgeが判定するものも）。確認できないヘッドレス（`sasacode -p`）ではそれらを実行しないので、autoモードかallowルールを使う。拾ったことはモデルに伝え（前の文に残って実行しなかった呼び出しがあれば、コードの中の例を除いたその数も）、元の本文はセッションに記録する。既定で有効（拾わなければ、この形の応答はツールを使わない最終回答として扱われ、実行が終わるため） |
| `repetition-guard` | 生成中に同じ文を繰り返し始めたら止め、1回分だけ残して、続きを促す（「なるほど。」のような短い繰り返しも、長く続けば止める） |
| `thinking-budget` | ローカルモデルだけ（`server`設定済み、または`baseUrl`がローカル／プライベートアドレスのもの。ID末尾が`-cloud`や`:cloud`/`-cloud:`を含むOllamaクラウドモデルは除外。`maxChars`や`cloudMaxChars`明示時はクラウドでも有効）の思考ブロックが上限（`localMaxChars`、既定12,000文字。設定で引き上げ可）を超えたら生成を止め、手元の情報でツール呼び出しや回答へ進むよう促す |
| `error-hints` | `read` / `edit` のファイル未検出時に近い実在パス（`Did you mean:`）、`Unknown tool` が Skill 名のときは `SKILL.md` のパス（または類似ツール名）、テスト失敗時はインポートやスタックトレースからテスト対象の実装ファイル候補を追記する |
| `loop-guard` | 同じツール呼び出し（またはA→B→A→Bのような3手までの周期）が同じ結果で続いたら、3回目に注意を添え、5回目からはその周期の呼び出しをすべて止める（ファイルを変更するか、次の依頼で解除）。同じ応答の中で、間に状態を変えうる呼び出しがない重複は1回だけ実行する。同じエラーの繰り返し（引数の不備などで実行されなかった呼び出しを含む）も知らせる。ツールが1件も実行されないターンが5回続いたら、実行を止める |
| `write-guard` | `write`の内容が`read`の出力の写しなら、行番号を外して書き込み、外したことをモデルに伝える（どの行も空白で始まる行番号を持ち、1つずつ増えているときだけ。行の抜けや追加で数が飛んでいれば書き込まない）。行番号の数字とタブだけを消して、桁揃えの空白（1〜5個）が全行の先頭に残った形でファイルを書き戻したときも、その空白を外して伝える。20行以上ある既存のファイルを半分未満の行数で上書きしようとしたら、書き込まずに`edit`を使うよう返す。同じ内容の`write`がもう一度来れば、そのまま書き込む。`lineNumbers: false` / `shrink: false`で外し、`minLines`で行数を変える |
| `search-tools` | `.gitignore` と `.git` / `node_modules` / `target` などを除いて件数上限つき（既定 50 件、最大 200 件）で探す `find_files`・`search_text` ツールを足す |
| `syntax-check` | `edit` / `write` / `hash_edit` 成功直後に構文を軽く確かめ（JSON、Rust、JS/TSは `bun` / `node`、Pythonは `python3`）、その編集で壊れたときだけ該当行の抜粋付きで `[syntax-check]` の注意をその場で返す。外部の検査はプロジェクト外の専用ディレクトリで動き、プロジェクト内のコードは実行しない |
| `action-nudge` | ローカルモデルだけ（判定は`thinking-budget`と同じ）が、最終文・最終行で「確認します」「Let me check...」のようにやるつもりだけ書いてツールを呼ばずにターンを終えたとき、すぐツールを呼ぶよう促す（1回の依頼につき既定2回まで。質問、「必要であれば」「if you want」などの条件付き提案、完了報告、長文解説には反応しない） |
| `agents-md` | `~/.sasacode/AGENTS.md` と、リポジトリのルートから作業ディレクトリまでの `AGENTS.md` を読む（CLAUDE.mdは読まない） |
| `compaction` | コンテキストが 80% に達するか上限に来たら、古い履歴を要約する。`/compact` で手動実行。要約を頼むリクエストはコンテキスト長に収まる大きさにするので、小さいコンテキスト長のローカルモデルでも動く（要約はその15%まで、履歴が入りきらなければ古い部分を省く）。自動の圧縮では、直近の履歴（コンテキスト長の15%、最大4万トークン）を要約せずにそのまま残す。1つの指示のあとツール呼び出しが長く続いていても、直近の手順は残る。`/compact` は全部を要約する。画面の会話はどちらでもそのまま残る（再開したときも、要約する前の会話から表示する） |
| `subagent` | `task` ツール。別の履歴を持つサブエージェントに作業を任せ、報告だけを受け取る（1ターンに複数あれば並行して動く）。`background: true` なら待たずに作業を続け、終わると報告がメッセージで届く（メインをEscで止めると一緒に止まる）。`task_status`・`task_send`・`task_stop` で進み具合を見る・指示を足す・止める。途中で失敗したときは理由と最後の発言・ツール呼び出しを返す。ユーザーは `/task`（一覧・詳細、`/task send <id> <内容>`、`/task stop <id>`）で同じことができる。サブエージェントは `task` 系・`todo_write`・`set_goal` を使えない（呼び出し元のTODOとゴールを書き換えないため） |
| `todo` | `todo_write` ツール。作業計画をセッションに残し、進捗をフッターに出す。未完了の項目を残したままツールを10回呼んでも更新しなければ、ツールの結果のあとに、計画の中身と更新を促す一言を足す（モデルへのメッセージとして履歴に残り、画面には薄い1行で出る。`remindAfter` で回数を変え、0で止める） |
| `progress-notes` | 作業中の進捗の一言を促す。システムプロンプトで、最初のツール呼び出しの前に何をするかを一文で、その後も見つけたこと・終えたこと・方針の変更を短く書くよう頼む。説明なしのツール呼び出しが5回続いたら、ツールの結果のあとに一言足して促す（`todo` と同じく履歴に残り、画面には薄い1行で出る。`remindAfter` で回数を変え、0で促すのだけ止める）。サブエージェントには付けない |
| `web-fetch` | `web_fetch` ツール。URLを取ってきてテキストにする。メニュー・目次・言語一覧・広告まわり・インラインのCSSやJSを除き、ページが本文の場所（`main`、`article`、Wikipediaの本文など）を示していればそこを読む（その中の折りたたまれた部分も読む）。サイトのナビゲーションのリンクは、本文のあとに1行（最大25件）で残す。ページ内のリンクは絶対URLの番号付き一覧にして返し、推測でURLを組み立てずに辿れるようにする。404のときは、サイトの `llms.txt` / `sitemap.xml` があればそれと検索を案内する。リダイレクトは一つずつたどり、リダイレクト先が内部アドレス（ループバック、プライベート、リンクローカルなど）なら止める（最初のURLは承認で見たものなので、手元の開発サーバーでも読む。最初のURLと同じオリジンへのリダイレクトも通す）。リダイレクトされたときは、結果に取得元のURLを書く |
| `web-search` | `web_search` ツール。[RustXNG](https://github.com/sahenjp/RustXNG)（キー不要のSearXNG互換メタ検索）で検索する。`include_text` では、検索語に合う上位3件（`textPages` で変更）のページを `web_fetch` と同じ処理で読み、本文を添える（読むのは公開アドレスのページだけで、名前解決で確かめたアドレスにそのまま接続する。リダイレクトも一つずつ同じ確認をする。社内の検索サーバーなどで内部のページも読むなら `privatePages: true`）。`rustxng`（PATHか、`sasacode web-search install` で入れたもの）があれば、最初の検索で `127.0.0.1:47888` に起動し、`RUSTXNG_IDLE_TIMEOUT`（既定600秒）のあいだ使われなければ自分で終了する（常駐しない。同時に動くセッションは同じものを使う）。`plugins.settings.web-search` の `url` で、常駐させているRustXNGやSearXNGを使う。ほかに `command`、`port`、`idleTimeout`、`results`、`textPages`、`privatePages`。browsrの検索がつながっているあいだは、検索手段が重ならないよう出さない |
| `browsr` | [browsr-4-agent](https://github.com/sasanokusa/browsr-4-agent) によるWeb検索（`search`）と本文の閲覧（`open`）。`browsr-agent` がPATHにあれば自動で起動する |
| `permission-presets` | 権限ルールのプリセット（`guard`、`read-only-shell`、`tests`） |
| `agent-judge` | `agent` モードの判定役。モードが確認に回す呼び出しを、モデル（`judgeModel`、なければ現在のモデル）に判定させる。ルールは緩めない。無効にすると `agent` モードは確認になる |
| `openai-codex` | ChatGPTプランのモデル（`openai-codex/…`）。`sasacode login` の認証情報を使う |
| `usage` | `/usage` コマンド。この起動の使用量、今日・直近7日・30日・全期間の合計、ChatGPTプランの残り（5時間・週の枠と、リセットまでの時間）を出す。`/usage graph` は日ごとの使用量をGitHubのContributions Graphのように、`/usage history [日数]` は日別の記録を、`/usage models` はモデル別の合計を、`/usage plan` はプランの残りだけを出す。記録は `~/.sasacode/sessions` のセッションログから読む（サブエージェント、圧縮の要約、agent モードの判定、やり直しで捨てた応答の分も含む。それより前の版のログには残っていない） |
| `goal` | セッションのゴール（最終目的）。`/goal <目的>` で設定し、`done` / `pause` / `resume` / `clear` で状態を変える。「これをゴールにして」と頼めば、モデルが `set_goal` ツールで会話から要約して設定する。作業中のゴールはシステムプロンプトとフッターに出る |
| `background-sessions` | `bg_start` ツール。ビルドやテストなど長いコマンドをバックグラウンドで動かし、終わったら終了コードと出力の末尾を会話に届ける（待機中なら自動で続きを始める）。bashと同じ権限ルールで判定する（`permissionsAs`）。sasacodeを終了してもジョブは続き、次の起動で結果を知らせる。`/bg` で一覧・詳細・停止。WindowsではGit Bashで動かす |
| `auto-reconnect` | 実行がエラーで終わったとき、使用中のエンドポイントに届かなければ、復旧を待って作業を自動で再開する（既定は最大10分）。サーバーが一時的なエラー（429・5xx）を返し続けたときは、間を空けて最大4回再試行する。`/reconnect` で手動でも同じことをする |
| `checkpoints` | write / editが変えたファイルの直前の内容をセッションに退避する。`/undo` で直近の1件から1つずつ戻せる（`/undo list` で一覧、`/undo <path>` で対象を絞る。bashの変更は対象外） |
| `gui` | `/gui` コマンド。デスクトップGUI（`sasacode-gui`、開発中）をこのフォルダで開き、進行中の会話を読み込んだ状態にする。`sasacode-gui` は、`plugins.settings.gui.command`、`sasacode gui install`で入れたもの、PATHの順に探す。開いたあとは、TUIとGUIのどちらか一方で続ける（互いの発言は読み込まない） |
| `image-attach` | 画像対応モデルにメッセージと画像を送る。スクリーンショットは ⌘V（届かないターミナルでは ctrl+v）、ファイルはターミナルへドラッグ&ドロップすると、その場で読み込んで入力欄に `[Image #1]` と出る。続けてメッセージを書いて送れば一緒に届く。ほかに `@パス`、`/image パス…`、`/image` だけならクリップボード（macOS、Windows、Linuxは `wl-paste` か `xclip`）。形式は中身で判定（PNG / JPEG / GIF / WebP）、1枚 5 MB・1メッセージ 8 枚まで。`plugins.settings.image-attach` の `dropPaths: false` で `@` なしのパスを無視 |
| `image-slim` | 履歴の画像を、毎回のリクエストで小さく送る。プロバイダーがどのみち縮める大きさ（約105万画素、`maxPixels`）を超える画像は縦横比を保って縮め、透明度のないPNGで100 KB以上のものはJPEG（品質90）にする。ツールが返した画像はその場で縮めてセッションに残し、座標を元の大きさに戻す倍率をモデルに伝える。古い画像は送らず短い注記に置き換え、新しい4〜11枚だけを送る（`keepImages`、`dropStep`。8枚ずつまとめて省くので、その間はプロンプトの先頭が変わらずキャッシュが効く）。最後のメッセージの画像は必ず送り、画像の合計が8 MiBを超えればさらに省く。`/image-slim` で直近のリクエストの枚数を出す。`models` で効かせるモデルを絞る |
| `context-slim` | ローカルモデルだけ（判定は`thinking-budget`と同じ）で、推定トークン数がコンテキスト長の閾値（既定55%）を超えると、直近の履歴を残したまま古い大きなツール出力（`read`〔`SKILL.md`を除く〕・`bash`・`find_files`・`search_text`・`grep`/`glob`/`ls`。`task`のサブエージェント報告やその他のツール結果は対象外）だけを`maskStep`（既定4件）刻みで短い注記に畳み、ステップ境界の間はプロンプト先頭を変えずKVキャッシュを保つ |
| `ask-user` | `ask_user`ツール。モデルが推測で進めずに、選択肢つきの質問（1回に1〜4問）であなたに判断を仰ぐ。ヘッドレスでは尋ねず、モデルに自分で決めて前提を書くよう返す。0.15までは別に入れるプラグインだった（[詳しく](docs/plugins.md#ask-user)） |
| `diagnostics` | APIの診断（既定で無効）。`/diagnostics on` で、リクエストの成否・停止理由・トークン数などのメタデータだけを記録し、`show` で直近100件を見る。本文や秘密値は記録しない |
| `skills` / `mcp` | Agent SkillsとMCPのアダプタ |

次の8つも同梱しているが、既定では読み込まない（`git-context`、`project-checks`、`workflows`、`jev-guard`、`free-search`は、0.15までは別に入れるプラグインだった）。`plugins.enabled`に名前を書くと読み込む。`plugins.disabled`にも書いてあれば、そちらが優先する。プロジェクトの設定に書いた`plugins.enabled`は、プロジェクトを信頼するまで使わない。

```json
{ "plugins": { "enabled": ["git-context", "project-checks"] } }
```

| 名前 | 内容 |
| --- | --- |
| `hashline-edit` | `read` の各行に `<行番号>:<2桁ハッシュ>|` を付け、空白の完全一致なしで編集できる `hash_edit` ツールを足す。`write` に混入した `N:hh|` 接頭辞も外す（[詳しく](docs/plugins.md#hashline-edit)） |
| `git-context` | `git_context`ツール。レビューに要るGitの状態・変更の統計・パッチを、1回の呼び出しで返す。読み取りだけで、Gitが要る（[詳しく](docs/plugins.md#git-context)） |
| `project-checks` | `project_checks`・`project_check`ツールと`/checks`。テスト・lint・型検査などを名前で実行し、PASS / FAIL、終了コード、時間、ログを返す。bashと同じ権限ルールで判定する。`verifyOnFinish`に名前を書いた検査は、モデルが作業を終えようとしたときにも確認なしで実行し、落ちていれば出力の末尾を返して続けさせる（[詳しく](docs/plugins.md#project-checks)） |
| `workflows` | `/workflow`。レビュー・失敗の調査・公開前の確認など、繰り返し使う依頼を呼び出す。設定で足す・上書きする・外すことができる（[詳しく](docs/plugins.md#workflows)） |
| `jev-guard` | 実行前のツール呼び出しをJevに判定させる。有効にする方法は上の「Jevによる実行前判断」にある（[詳しく](docs/plugins.md#jev-guard)） |
| `free-search` | MCPサーバー`uvx free-search-mcp`を`free-search`という名前でつなぐ（キー不要でローカルで動くWeb検索。複数の検索エンジン、ページの取得、文書の読み取り）。ツールは`mcp__free-search__<tool>`になる。[uv](https://docs.astral.sh/uv/)が要る（[詳しく](docs/plugins.md#free-search)） |
| `clean-retry` | 編集エラーやテスト失敗が連続したとき、変更したファイルをタスク開始前に巻き戻し、失敗要約だけを残した綺麗な履歴でやり直す（[詳しく](docs/plugins.md#clean-retry)） |
| `slim-profile` | 小型ローカルモデル向けに補助的なプラグイン群（`subagent`、`todo`、`goal`、Web系、Skills など）を自動で省き、指示の簡潔化と `read` / `bash` 単発出力の上限カットを行う（[詳しく](docs/plugins.md#slim-profile)） |

### MCP

```json
{
  "mcpServers": {
    "github": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" } },
    "remote": { "url": "https://example.com/mcp", "headers": { "Authorization": "Bearer ${REMOTE_TOKEN}" } }
  }
}
```

グローバルの`~/.sasacode/config.json`に書いたstdioのサーバーは、`cwd`がなければプロジェクトの外にある専用のフォルダ（`~/.sasacode/tmp/mcp/<サーバー名>`）で起動する。`npx`や`uvx`はカレントの`node_modules`や`pyproject.toml`を先に見るので、開いたプロジェクトの中身が動かないようにするためである。プロジェクトのフォルダで動かしたいサーバーには、`"cwd": "${SASACODE_PROJECT}"`と書く。プロジェクトの`.sasacode/config.json`に起動のしかた（`command`か`url`）まで書いたサーバー（信頼したあと）は、これまでどおりプロジェクトのフォルダで起動する。グローバルのサーバーに`disabled`などだけを足した場合は、グローバルのサーバーのままである。同梱の`free-search`も、プロジェクトの外で起動する。

stdioとStreamable HTTPに対応する。ツールは`mcp__<server>__<tool>`という名前になり、通常のツールと同じ権限確認とフックを通る。promptsはスラッシュコマンドになる。サーバーはバックグラウンドで接続するので、起動を待たせない。`${VAR}`は環境変数から展開する。落ちたサーバーは自動で再接続する（間隔を空けて数回まで。`/mcp reconnect [name]`で手動に）。サーバーのツールの一覧が変わったら、外れたツールはモデルに渡さなくなる。プロジェクトの`.sasacode/config.json`に書いたサーバーは、プロジェクトを信頼するまで起動しない（[プロジェクトの信頼](#プロジェクトの信頼)）。サーバーごとの`cwd`、`disabled`、`alwaysLoad`、`timeout`、`toolNames`は[docs/plugins.md](docs/plugins.md)にある。

### Skills

`~/.sasacode/skills/<name>/SKILL.md`と`.sasacode/skills/<name>/SKILL.md`から、標準のAgent Skills形式（frontmatterの`name`と`description`）を読む。同じ名前なら、プロジェクトのものが優先する。システムプロンプトには名前・説明・パスだけを載せ、本文はモデルが必要なときにreadツールで読む。`/skill:<name> <依頼>`で明示的に使わせることもでき、`/skills`で一覧を出す。0.15まで読んでいたプラグインの`skills`ディレクトリは、0.20からはない。

組み込みのSkill `tool-authoring`も同梱している（`~/.sasacode/builtin-skills`に書き出す）。

### ツールの遅延ロード

MCPと同梱プラグインのツールの定義が、合計でコンテキストの10%以上か30個以上になると、定義を全部は送らない。名前と説明の一覧を持つ`tool_search`ツールだけを渡し、モデルが必要なものを検索して読み込む。組み込みの4ツールと`alwaysLoad`付きのツールは常に送る。

### MCPとSkillsで拡張する

0.15までの`~/.sasacode/plugins`に置くTypeScriptのプラグインと、`sasacode plugin`による検索・インストール・公開は、0.20でなくなった。npmの`@sasacode/plugin-api`も0.20からは公開しない。1.xのプラグインAPI（最後の版は1.15.0）は0.20で終わり、同梱プラグインのためのRustの内部API（`crates/sasacode-plugin-api`）として残っている。自分のツールやコマンドを足すには、次のどちらかを使う。

- **コードを動かすもの**：MCPサーバーにして`mcpServers`に書く。stdioのサーバーなら言語は問わない。サーバーのツールはモデルに渡り、promptsはスラッシュコマンドになる。
- **手順や知識**：Skillにして`~/.sasacode/skills/<name>/SKILL.md`か、プロジェクトの`.sasacode/skills/<name>/SKILL.md`に置く。

設定のキー、`SKILL.md`の形式、同梱プラグインの設定、同梱プラグインの書き方（開発者向け）は[docs/plugins.md](docs/plugins.md)にある。

## セッション

`~/.sasacode/sessions/<作業ディレクトリ>-<ハッシュ>/<日時>_<id>.jsonl` に、メッセージとツールの結果を1件ずつ追記して保存する。途中で強制終了しても再開できる。結果が保存される前に止まった呼び出しは「実行されたか不明」としてモデルに伝え、状態を確かめてからやり直させる。書きかけの最終行は `<ファイル>.torn` に退避する。セッションIDは、TUIのフッター、`/session`、終了時の表示、ヘッドレスの出力（テキストならstderrの最後、JSONLなら先頭の `{"type":"session"}`。この行の`contextWindow`はモデルのコンテキスト長で、分からなければ`null`）に出す。`SASACODE_HOME` で `~/.sasacode` の場所を変えられる。

## 開発

```bash
cargo test --workspace --locked   # LLMなしのテスト（応答の再生、偽のモデルサーバー、テスト用のMCPサーバーを使う）
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo xtask checks                # リポジトリの検査（コアの行数、READMEとCLIの対応など）
cargo xtask improve               # 改善ループの信号（検査の合否、コアの行数、リポジトリの規模）
cargo xtask build                 # 配布用のバイナリ
```

開発に要るのはRustのツールチェーン（cargo）だけで、0.20からBunは使わない。

**リリースの手順**：`v*`のタグをpushすると、GitHub ActionsがバイナリをビルドしてGitHub Releaseを作る。版はルートの`Cargo.toml`（`[workspace.package]`の`version`）で、`sasacode --version`もこの版を出す。タグとこの版が合っているかを確かめ、macOS版はmacOSのランナーでビルドし、すべてのバイナリを動く環境で起動してから（x64のmacOSはRosetta、muslはAlpineのコンテナ）、署名つきの来歴を付けて公開する。リリースのファイル名は0.15から変えていない。sasanokusa.comからのインストールも、それだけで新しい版になる。案内ページを変えたときと、新しい版を出した後に`scripts/publish-site.sh <ホスト>`（または環境変数`SASACODE_SITE_HOST`）を実行する。ページの版の表記は、実行時に埋める。リリースが公開されたら`cd npm/sasacode && npm publish`でnpmの`sasacode`も同じ版にする（`npm/sasacode/package.json`の版はルートの`Cargo.toml`と合わせておく。合っていないとリリースのワークフローが止まる）。

| クレート | 役割 |
| --- | --- |
| `sasacode-ai` | プロバイダーに依存しないメッセージ型、Anthropic / OpenAI Chat / OpenAI Responsesのアダプタ、料金の計算 |
| `sasacode-agent` | ループ、フック、ツール実行（修復・検証・権限）、ツールの遅延ロード、セッション、プラグインのホスト |
| `sasacode-plugin-api` | 同梱プラグインが使うAPI（ツール、コマンド、プロバイダー、フック、UI、セッション、エージェント）。0.20からは内部のAPIで、公開しない |
| `sasacode-tools` | read / write / edit / bash（プラグインAPIで登録） |
| `sasacode-tui` | ratatuiとcrosstermで作った全画面の対話UI |
| `sasacode-cli` | `sasacode`コマンド：引数、設定、キー、モデル一覧、信頼確認、同梱プラグインの読み込み、ヘッドレス実行 |
| `sasacode-mcp` / `sasacode-skills` / `sasacode-bundled` | MCPのクライアント（stdioとStreamable HTTP）、Agent Skills、同梱プラグイン |
| `sasacode-host` | TUI・CLI・同梱プラグインが共有するもの：画面の言語（ja / enの訳）、組み込みのプロバイダーと既知のモデル（価格・コンテキスト長）、セッションの索引 |
| `sasacode-protocol` / `sasacode-gui` | JSONLのイベント・`--control stdio`のメッセージ・セッションログの型（GUIが読む）、デスクトップGUI |
| `sasacode-testing` | テスト用：応答を再生するプロバイダーと、偽のモデルサーバー |
| `xtask` | リポジトリの作業：ビルド、検査、改善ループの計測、配布物の梱包 |

デスクトップGUI（Rust、開発中）は `crates/` にある。`cargo run -p sasacode-gui -- <フォルダ>` で開き、入力ごとに `sasacode -p … --output jsonl --control stdio` をそのフォルダで動かす（`SASACODE_BIN` か `--sasacode` で実行ファイルを指定できる）。`sasacode gui install --path .` で入れておけば、`sasacode gui [フォルダ]` と、TUIの `/gui`（進行中の会話を読み込んだ状態で開く）でも開ける。`cargo test` で、CLIの出力とセッションログを読めるかも確かめる。設計と段階は [docs/gui.md](docs/gui.md)、ほかのエージェントのGUIとの比べは [docs/gui-parity.md](docs/gui-parity.md)。

コア（`sasacode-ai`・`sasacode-agent`・`sasacode-plugin-api`・`sasacode-tools`）の行数の上限は12,500行で、1行は120桁まで（rustfmtの`max_width = 120`。ほとんどが1つの文字列リテラルの行は除く）。どちらかを超えると`cargo xtask checks`が落ちる。いまの行数は`cargo xtask improve`が出す。

リポジトリのドキュメント：[要件](docs/requirements.md)、[マイルストーンの検証](docs/milestones.md)、[実行時依存の記録](docs/dependencies.md)、[拡張のしかた（MCP・Skills・同梱プラグイン）](docs/plugins.md)、[API互換性の確認表](docs/api-compatibility.md)、[ベンチマーク](docs/benchmarks.md)、[改善の回し方](docs/improvement.md)。改善は1周ずつ小さく積む。手順と観点、これまでの記録は[docs/improvement.md](docs/improvement.md)にある。
