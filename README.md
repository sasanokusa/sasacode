# sasacode

[English](README.en.md) · 日本語 · [ドキュメント](https://sasanokusa.com/sasacode/docs/)

プラグインで組み替えられる、ターミナル向けの小さなコーディングエージェント（TypeScript / Bun）。

コアに入っているのは、エージェントループ、プロバイダー、4つの組み込みツール（read / write / edit / bash）、TUI、セッション、権限確認だけ。それ以外の振る舞いは、すべて公開のプラグインAPIで足す。組み込みツールも同じAPIで登録している。MCPサーバーとAgent Skills（`SKILL.md`）はそのまま使える。小型モデルでも実用になるよう、形式の崩れたツール呼び出しの修復と、同じ文の繰り返しの抑止も同梱している。

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
- macOS（arm64 / x64）、Linux（x64 / arm64、glibc / musl、AVX2のないCPU向けのbaseline版）、Windows（x64 / arm64、x64のbaseline版）の単一バイナリ。Bunは不要。
- インストーラーもバイナリも、[GitHub Releases](https://github.com/sasanokusa/sasacode/releases) の最新版から取得してSHA256を検証する。sasanokusa.comのURLは、最新リリースの `install.sh` / `install.ps1` へのリダイレクト。
- SHA256が防ぐのは壊れたダウンロードまで。ファイルがこのリポジトリのGitHub Actionsでビルドされたものかは、v0.9.8以降のリリースに付けた署名つきの来歴で確かめられる（`gh attestation verify sasacode-darwin-arm64.tar.gz --repo sasanokusa/sasacode`。リリースから落としたファイルに対して。`install.sh` も同じ）。
- `SASACODE_VERSION=v0.10.1` で版を固定でき、`SASACODE_INSTALL_DIR` で置き場所を変えられる（どちらのインストーラーでも）。
- 更新は `sasacode update`（インストーラーと同じ検証つき。`--check` で確認だけ）。新しい版が出ていると起動時に一度だけ静かに知らせる（`"updateCheck": false` で止める）。ソースから動かしている場合は `git pull` と `bun install`。

ソースから使う場合（[Bun](https://bun.sh) 1.4 以上）は次のとおり。

```bash
git clone https://github.com/sasanokusa/sasacode && cd sasacode
bun install
ln -s "$PWD/packages/cli/src/main.ts" ~/.local/bin/sasacode
```

## 使い方

```bash
sasacode                            # 対話モード（TUI）
sasacode -p "テストを直して"           # ヘッドレス：答えを stdout に、進捗を stderr に
sasacode -p "…" --output jsonl       # 全イベントを JSONL で
sasacode -p "…" --output jsonl --control stdio   # 親プロセスが stdin で承認に答え、中断できる（[ヘッドレスの制御](#ヘッドレスの制御)）
echo "$LOG" | sasacode -p "原因は？"   # パイプした入力は -p の後ろに付く（5 秒何も来なければ読まない。--stdin で終わりまで待つ）
sasacode -c                         # このディレクトリの直近のセッションを再開
sasacode -r <id> -p "続き"            # セッション ID を指定して続ける（tmux がなくても会話を続けられる）
```

| オプション | 内容 |
| --- | --- |
| `-m <provider/model>` | モデル（例 `anthropic/claude-opus-5`、`openai/gpt-5.5`、`ollama/gemma4:e4b`） |
| `--permission <edits\|ask\|agent\|auto>` | 権限モード（既定 `edits`） |
| `--thinking <off\|low\|medium\|high\|xhigh\|max>` | 推論の深さ |
| `--max-turns <n>` | 1回の実行のターン数の上限（既定 200、0 で無制限） |
| `--control stdio` | `-p --output jsonl` と併用。親プロセスがstdinで承認に答え、実行を中断できる（[ヘッドレスの制御](#ヘッドレスの制御)） |
| `--append-system-prompt-file <path>` | ファイルの中身（UTF-8）を、設定の `instructions` の後ろにシステムプロンプトとして足す。読めないファイルはエラーで終わり、空のファイルは何も足さない。TUIでも使える |
| `-c` / `-r <id>` / `--no-session` | 再開 / IDを指定して再開 / 保存しない |
| `--trust-project` | プロジェクトを確認なしで信頼する（ヘッドレス向け。[プロジェクトの信頼](#プロジェクトの信頼)） |
| `sasacode plugin search\|install\|update\|remove\|list\|publish` | プラグインを探す・入れる（npmかgit URL）・更新する・npmに公開する（[共有](#プラグインを共有する)） |
| `sasacode endpoint add\|remove\|list` | 自前のサーバーの追加（形式は自動判別） |
| `sasacode login [status\|logout]` | ChatGPTプランでログイン（`openai-codex/…` のモデル用） |

### ヘッドレスの制御

ほかのプログラムが `sasacode -p "…" --output jsonl` を子プロセスとして動かすときは、`--control stdio` を付けると、確認が必要な呼び出しにstdinで答えたり、実行を中断したりできる。`-p` と `--output jsonl` の両方が必要で、欠けているとエラーで終わる。stdinは制御に使うので、パイプした入力はプロンプトに付かない。

stdoutには、これまでのJSONLのイベントに混ざって、次の行が1件ずつ出る。

```jsonc
{"type":"approval_request","id":"approval-1","callId":"call_1","tool":"bash","args":{"command":"touch x"},"reason":"…","cwd":"/path/to/project"}
{"type":"approval_cancelled","id":"approval-1"}
{"type":"control_error","error":"not valid JSON"}
```

stdinは1行1件のJSON（JSON Lines）で読む。

```jsonc
{"type":"approval","id":"approval-1","decision":"allow"}           // "allow" | "always" | "deny"
{"type":"approval","id":"approval-1","decision":"deny","feedback":"理由"}
{"type":"abort"}
```

- `approval_request` は、確認が必要な呼び出しごとに1行出る。呼び出しは答えが届くまで実行しない。`id` は答えに付ける。`callId` は会話の中のtool_callのid、`tool` と `args` は呼び出しの中身、`reason` は確認が必要な理由。
- `approval` の `decision` は、`allow` が今回だけ許可、`always` が許可に加えて同じ呼び出しを許すルールの追加（TUIの「常に許可」と同じで、プロセスの終了まで有効）、`deny` が拒否。`feedback` は `deny` のときモデルに渡る。
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
| esc | 生成・ツール実行を中断（中断したことは次の入力でモデルに伝わる） |
| 実行中にenter | メッセージを予約し、次のターンで届ける |
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
| `/effort` | 推論の深さ（`off` / `low` / `medium` / `high` / `xhigh` / `max`）を切り替える。引数なしなら一覧から選ぶ。現在の値はフッターに出る。OpenAI形式のサーバーには `reasoning_effort` として送り、`off` は `none` として送る（vLLMなどは送らないと推論する）。サーバーが受け付けない値なら近い値に替えて送り直し、以後はその値を使う（例：Command Codeは `none` を受け付けないので `off` は `low` になる） |
| `/language` | 画面の言語を切り替える（`ja` / `en`。引数なしなら一覧から選ぶ）。その場で切り替わり、`~/.sasacode/config.json` の `lang` にも保存する。切り替え前に表示された文はそのまま |
| `/copy` | 直前の応答をクリップボードにコピー（pbcopy / wl-copy / xclip、なければ端末経由） |
| `/permission` `/help` `/exit`（`/quit`） | 権限モード / ヘルプ / 終了 |
| `/usage` `/goal` `/bg` `/reconnect` `/compact` `/mcp` `/skills` `/skill:<name>` `/presets` `/browsr` `/undo` | 同梱プラグインのコマンド |

フッターには、モデル、推論の深さ、権限モード、コンテキストの使用量（`ctx 77.6k/256k (30%)`）、セッションIDと、プラグインのステータス（MCPの接続数、TODOの進捗など）を出す。30 秒以上かかった実行が終わると、端末のベルを鳴らす（別の作業をしていても気づけるように。`"tui": { "bell": false }` で止める）。

## プロバイダーとモデル

モデルは `<provider>/<model>` で指定する。

| provider | API形式 | キー |
| --- | --- | --- |
| `anthropic` | Anthropic Messages | `ANTHROPIC_API_KEY`（空ならSDKが `ant auth login` のプロファイルを使う） |
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

`GET /v1/models` の応答から、OpenAI形式（Chat Completionsで呼ぶ）かAnthropic形式（Messagesで呼ぶ）かを自動で判別し、URLも各形式のSDKに合わせて整える。llama.cppとOllamaは、それぞれ `/props` と `/api/show` から実際のコンテキスト長も取る。追加したサーバーのモデルは、`/model` の一覧に自動で出る。設定ファイルに `"providers": { "llama": { "baseUrl": "http://…" } }` とURLだけ書いた場合も、起動時に判別する（結果は `~/.sasacode/endpoints.json` にキャッシュする）。

プロジェクトの `.sasacode/config.json` に書いたエンドポイントは、そのプロジェクトを信頼するまで使わない（[プロジェクトの信頼](#プロジェクトの信頼)）。キーを使わないサーバーでも、会話とコードが送られるため。

**モデル一覧の自動取得**：TUIの起動時に、キーのあるプロバイダー（とOllama）のModels API（`GET /v1/models`）をバックグラウンドで呼び、`/model` に一覧とコンテキスト長を出す。取得したコンテキスト長は、フッターの使用率や圧縮のしきい値にも使う（設定の `modelOverrides` があればそちらが優先）。Ollamaは独自の `/api/show` から読み、Modelfileの `num_ctx` があればそれを実際の長さとして使い、なければモデルの最大長を参考として表示する（メモリに載っているモデルは `/api/ps` の実際の長さを使う）。`num_ctx` のないモデルを選ぶと、Ollamaが既定の長さを超えた入力を気づかないうちに切り詰めることを一度だけ警告する。OpenAIのModels APIはコンテキスト長を返さないので、組み込みのモデル表の値で補う。Command Codeのように1つのキーで複数のAPI形式を提供するサービスでは、モデルごとに対応する形式のプロバイダーへ自動で振り分ける（例：Claude系は `commandcode-anthropic/…`）。

**APIキー**は次の順で探す：シェルの環境変数 → `~/.sasacode/.env` → OSキーチェーン（macOS: `security add-generic-password -s sasacode -a <provider> -w`、Linux: `secret-tool store --label sasacode service sasacode account <provider>`）。どこでも空欄は無視する。作業ディレクトリの `.env` と `bunfig.toml` は読まない（リポジトリが `ANTHROPIC_BASE_URL` などで接続先を差し替えたり、起動時にスクリプトを実行させたりできないようにするため）。セッションのログには、使用中のキーを `[REDACTED]` に置き換えてから書く。`~/.sasacode/.env` にだけ書いたキーは、エージェントが実行するコマンド（bash、バックグラウンドジョブ）には渡さない。`~/.sasacode`（セッションのログ、入力履歴、長い出力の保存先 `tmp/`）は本人しか読めない権限（0700）にする。

## 設定

`~/.sasacode/config.json`（全体）と `<project>/.sasacode/config.json`（プロジェクト）をマージし、プロジェクトが優先する。ただし、プロジェクト設定のうち信頼が必要な部分は、信頼するまで使わない（下記）。権限ルールと `disabled`（ツール・プラグイン）の配列は両方を合わせ、全体設定のdeny / askや無効化をプロジェクト側から消すことはできない。型の合わない値や未知のキーは、警告を出して無視する。

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
  "plugins": { "disabled": ["web-fetch"], "settings": { "permission-presets": { "presets": ["guard", "tests"] } } },
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
| `edits`（既定） | 作業ディレクトリ内のread / write / editは自動で許可し、bashやディレクトリ外への操作は確認する |
| `ask` | 読み取りを含む、すべてのツール実行を確認する |
| `agent` | 作業ディレクトリ内のreadは自動で許可し、それ以外は現在のモデルに危険度を判定させる。安全なら許可、そうでなければ確認する。判定には、設定の `judgeModel`（`"provider/model"`）で別のモデルを使える。速くて安いモデルにすると、確認の待ち時間と費用が減る。判定するモデルは呼び出しの引数も読むため、モデルを誘導する文が判定も誘導しうる。利便のための機能で、プロンプトインジェクションへの防御ではない（それにはdeny / askルールと `guard` プリセットを使う） |
| `auto` | すべて許可する（denyとaskのルールは効く）。TUIのフッターでは赤字で表示する |

- ルールは `ツール名` か `ツール名(パターン)`。bashはコマンド文字列を `*` のワイルドカードで、read / write / editはパスをglobで照合する。`*` そのものは `\*` と書く。パスのルールは書いたツールにだけ効くので、`.env` を守るならread / edit / writeを並べ、サブディレクトリも含めるには `**/` を付ける（`edit(.env)` は作業ディレクトリ直下の `.env` のeditだけを止める）。コマンドのルールはコマンドの文字列に合わせるだけなので、オプションの順を変えたり絶対パスで呼んだりすれば当たらない。間違いを防ぐ柵であって、完全な防御ではない。
- 確認の「常に許可」で足すルールは、見たものより広くならない。コマンドはそのまま（中の `*` はワイルドカードにしない）、ファイルを触るツールはそのパスだけを許可する。ルールはsasacodeを終了するまで残る（`/clear` をまたぐ）。
- 判定の順番はdeny → softDeny → ask → allow → モード → `permission` フック（プラグイン）。`softDeny` はdenyと同じく拒否するが、`jev-guard` などのプラグインが「ユーザーに確認」まで下げられる（確認なしに実行されることはない）。広すぎて正当な操作も巻き込むパターン向け。allowルールで通るのは、`&&` `;` `|` でつないだコマンドの**すべて**が許可されている場合だけ。`$(…)`、バッククォート、`>` を含むコマンドは、allowルールでは通らない。
- 同梱の `permission-presets` が、既定で `guard`（sudo、`rm -rf /`、ディスク操作、`| sh` などを常に拒否。ホーム配下の `rm -rf ~…` とforce pushはsoftDeny。`ssh`、`scp`、`sftp`、リモートへの `rsync` は `auto` モードでも常に確認）を有効にしている。
- ヘッドレスでは確認できる人がいないので、確認が必要な呼び出しは実行せず、その理由をモデルに返す。`--control stdio` を付けると、親プロセスがstdinとstdoutで答える（[ヘッドレスの制御](#ヘッドレスの制御)）。
- パスは、シンボリックリンクをたどった先（実パス）で判定する。作業ディレクトリ内のリンクが外を指していれば、リンク先がまだ存在しなくても外への操作として扱う。プロジェクト内のリンクで、ルールの対象をすり替えることもできない。リンクの循環などで行き先が決まらないパスは、モードによらず確認する。
- 権限は、ツールを呼ぶ前の判定であって隔離（サンドボックス）ではない。判定から実行までの間にリンクを差し替えるような操作や、許可したbashコマンドの中身までは防げない。信頼できないコードを扱うときは、コンテナなどOS側で隔離すること。
- 中断（esc）した後は、承認済みでもまだ始まっていないツールは実行しない。

### Jevによる実行前判断（配布プラグイン `jev-guard`）

ルールと権限モードの判定の後に、判断専用モデル [Jev](https://commandcode.ai/models/jev)（TypeSafe）の判定を重ねるプラグイン。0.9.5 までは同梱していたが、既定で無効でキーも要るので、配布プラグインに分けた。

```bash
sasacode plugin install sasacode-plugin-jev-guard
```

入れた後は `/jev on` でそのセッションで有効になり、いつも使うなら設定に `{ "plugins": { "settings": { "jev-guard": { "enabled": true } } } }` と書く（0.9.5 までの設定はそのまま使える。設定があるのにプラグインが入っていなければ、起動時に入れ方を知らせる）。接続先、判定の変え方、閾値などは [plugins/jev-guard/README.md](plugins/jev-guard/README.md) にある。

### プロジェクトの信頼

クローンしたリポジトリは、そのままでは「制限を強める」ことしかできない。次のものは、コードを実行するか、データの送り先を変えうるので、プロジェクトを信頼するまで使わない。

- `.sasacode/plugins` のプラグイン
- `.sasacode/config.json` の `providers`（エンドポイント）、`modelOverrides`、`plugins`（無効化と設定）、`mcpServers`、`allow` ルール、全体設定より緩い権限モード、全体設定より大きい `maxTurns`（0 は上限なし）と `maxRetries`、そのプロジェクトにしかないプロバイダーを指す `model`

これらがあるプロジェクトで起動すると、最初に一覧を出して信頼するかを尋ねる（ヘッドレスでは `--trust-project`）。答えは `~/.sasacode/trust.json` に保存し、上の設定かプラグインのコード（`.sasacode/plugins` 以下のファイル。依存パッケージの中身は除く）が変わったら再び尋ねる。全体設定の `trustedProjects` に入れたプロジェクトは、尋ねずに信頼する。モデルの選択、`thinking`、`instructions`、deny / askルールやツール無効化の追加、より厳しい権限モードや小さい上限などは、信頼しなくても使う。

## プラグイン・MCP・Skills

### 同梱プラグイン（`plugins.disabled` で外せる）

`~/.sasacode/plugins` やプロジェクトに同じ名前のプラグインがあれば、同梱版の代わりにそちらを読み込む。

| 名前 | 内容 |
| --- | --- |
| `tool-repair` | 検証の前に、形式の崩れたツール呼び出しを直す。直す対象は、JSONの崩れ（末尾カンマ、閉じ括弧、クォートのないキー、`True`/`None`、コードフェンス、二重エンコード）、キー名（`file` → `path`）、型（`"20"` → `20`）、ツール名のtypo。候補が1つに絞れるときだけ直す。直したことはモデルに短く伝え、履歴には直した後の形を残し、元の出力はセッションに記録する |
| `repetition-guard` | 生成中に同じ文を繰り返し始めたら止め、1回分だけ残して、続きを促す（「なるほど。」のような短い繰り返しも、長く続けば止める） |
| `loop-guard` | 同じツール呼び出し（またはA→B→A→Bのような3手までの周期）が同じ結果で続いたら、3回目に注意を添え、5回目からはその周期の呼び出しをすべて止める（ファイルを変更するか、次の依頼で解除）。同じ応答の中で、間に状態を変えうる呼び出しがない重複は1回だけ実行する。同じエラーの繰り返し（引数の不備などで実行されなかった呼び出しを含む）も知らせる。ツールが1件も実行されないターンが5回続いたら、実行を止める |
| `agents-md` | `~/.sasacode/AGENTS.md` と、リポジトリのルートから作業ディレクトリまでの `AGENTS.md` を読む（CLAUDE.mdは読まない） |
| `compaction` | コンテキストが 80% に達するか上限に来たら、古い履歴を要約する。`/compact` で手動実行 |
| `subagent` | `task` ツール。別の履歴を持つサブエージェントに作業を任せ、報告だけを受け取る（1ターンに複数あれば並行して動く）。`background: true` なら待たずに作業を続け、終わると報告がメッセージで届く（メインをEscで止めると一緒に止まる）。`task_status`・`task_send`・`task_stop` で進み具合を見る・指示を足す・止める。途中で失敗したときは理由と最後の発言・ツール呼び出しを返す。ユーザーは `/task`（一覧・詳細、`/task send <id> <内容>`、`/task stop <id>`）で同じことができる。サブエージェントは `task` 系・`todo_write`・`set_goal` を使えない（呼び出し元のTODOとゴールを書き換えないため） |
| `todo` | `todo_write` ツール。作業計画をセッションに残し、進捗をフッターに出す |
| `web-fetch` | `web_fetch` ツール。URLを取ってきてテキストにする |
| `browsr` | [browsr-4-agent](https://github.com/sasanokusa/browsr-4-agent) によるWeb検索（`search`）と本文の閲覧（`open`）。`browsr-agent` がPATHにあれば自動で起動する |
| `permission-presets` | 権限ルールのプリセット（`guard`、`read-only-shell`、`tests`） |
| `openai-codex` | ChatGPTプランのモデル（`openai-codex/…`）。`sasacode login` の認証情報を使う |
| `usage` | `/usage` コマンド。この起動の使用量、今日・直近7日・30日・全期間の合計、ChatGPTプランの残り（5時間・週の枠と、リセットまでの時間）を出す。`/usage graph` は日ごとの使用量をGitHubのContributions Graphのように、`/usage history [日数]` は日別の記録を、`/usage models` はモデル別の合計を、`/usage plan` はプランの残りだけを出す。記録は `~/.sasacode/sessions` のセッションログから読む（圧縮とサブエージェントの分は含まない） |
| `goal` | セッションのゴール（最終目的）。`/goal <目的>` で設定し、`done` / `pause` / `resume` / `clear` で状態を変える。「これをゴールにして」と頼めば、モデルが `set_goal` ツールで会話から要約して設定する。作業中のゴールはシステムプロンプトとフッターに出る |
| `background-sessions` | `bg_start` ツール。ビルドやテストなど長いコマンドをバックグラウンドで動かし、終わったら終了コードと出力の末尾を会話に届ける（待機中なら自動で続きを始める）。bashと同じ権限ルールで判定する（`permissionsAs`）。sasacodeを終了してもジョブは続き、次の起動で結果を知らせる。`/bg` で一覧・詳細・停止。WindowsではGit Bashで動かす |
| `auto-reconnect` | 実行がエラーで終わったとき、使用中のエンドポイントに届かなければ、復旧を待って作業を自動で再開する（既定は最大10分）。`/reconnect` で手動でも同じことをする |
| `checkpoints` | write / editが変えたファイルの直前の内容をセッションに退避する。`/undo` で直近の1件から1つずつ戻せる（`/undo list` で一覧、`/undo <path>` で対象を絞る。bashの変更は対象外） |
| `image-attach` | 画像対応モデルにメッセージと画像を送る。`@パス`、ターミナルへのドラッグ&ドロップ（貼られた絶対パス）、`/image パス…`、`/image` だけならクリップボード（macOS、Linuxは `wl-paste` か `xclip`）。形式は中身で判定（PNG / JPEG / GIF / WebP）、1枚 5 MB・1メッセージ 8 枚まで。`plugins.settings.image-attach` の `dropPaths: false` で `@` なしのパスを無視 |
| `diagnostics` | APIの診断（既定で無効）。`/diagnostics on` で、リクエストの成否・停止理由・トークン数などのメタデータだけを記録し、`show` で直近100件を見る。本文や秘密値は記録しない |
| `skills` / `mcp` | Agent SkillsとMCPのアダプタ |

### MCP

```json
{
  "mcpServers": {
    "github": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" } },
    "remote": { "url": "https://example.com/mcp", "headers": { "Authorization": "Bearer ${REMOTE_TOKEN}" } }
  }
}
```

stdioとStreamable HTTPに対応する。ツールは `mcp__<server>__<tool>` という名前になり、通常のツールと同じ権限確認とフックを通る。promptsはスラッシュコマンドになる。サーバーはバックグラウンドで接続するので、起動を待たせない。`${VAR}` は環境変数から展開する。落ちたサーバーは自動で再接続する（間隔を空けて数回まで。`/mcp reconnect [name]` で手動に）。サーバーのツールの一覧が変わったら、外れたツールはモデルに渡さなくなる。

### Skills

`~/.sasacode/skills/<name>/SKILL.md`、`.sasacode/skills/<name>/SKILL.md`、プラグインの `skills` ディレクトリから、標準のAgent Skills形式（frontmatterの `name` と `description`）を読む。システムプロンプトには名前・説明・パスだけを載せ、本文はモデルが必要なときにreadツールで読む。`/skill:<name> <依頼>` で明示的に使わせることもできる。

組み込みのSkillとして `tool-authoring`（sasacodeのツールとプラグインの作り方）を同梱している。`/skill:tool-authoring 天気を返すツールを作って` のように使う。

### ツールの遅延ロード

MCPとプラグインのツールの定義が、合計でコンテキストの 10% 以上か 30 個以上になると、定義を全部は送らない。名前と説明の一覧を持つ `tool_search` ツールだけを渡し、モデルが必要なものを検索して読み込む。組み込みの4ツールと `alwaysLoad` 付きのツールは常に送る。

### プラグインを書く

`~/.sasacode/plugins/hello.ts` に置くだけで読み込む（ビルドは不要）。エディタで型補完を使うなら `npm install --save-dev @sasacode/plugin-api`（[npm](https://www.npmjs.com/package/@sasacode/plugin-api)）。

```ts
import { text, type Plugin } from "@sasacode/plugin-api";

export default ((api) => {
  api.registerTool({
    name: "hello",
    description: "Greet someone by name.",
    parameters: { type: "object", properties: { name: { type: "string" } }, required: ["name"] },
    kind: "read",
    execute: async ({ name }) => ({ content: [text(`Hello, ${name}!`)] }),
  });
}) satisfies Plugin;
```

公開API（現在 1.10.0。1.4.0 で締め切り、1.xの間は追加だけ。[互換性の約束](docs/plugins.md#互換性の約束140-で確定)）でできることは次のとおり。

- **登録**：ツール（別のツールの権限ルールを当てる `permissionsAs` つき）、スラッシュコマンド（引数の補完つき）、プロバイダー（モデル一覧の取得、通信の差し替え）、権限ルール（`softDeny` を含む）
- **フック**：`session_start/end`、`user_prompt`、`system_prompt`、`before_request`、`stream_delta`、`assistant_message`、`tool_call_raw`、`tool_call`、`permission`、`tool_result`、`turn_end`、`agent_end`、`context_limit`
- **UI**：通知・確認・選択（単一・複数）・自由入力・長文表示・ステータス行、ツール結果の描画
- **セッション**：状態の保存、履歴の差し替え、メッセージの差し込み
- **エージェント**：サブエージェント、単発の呼び出し

詳しくは [docs/plugins.md](docs/plugins.md)。

### プラグインを共有する

```bash
sasacode plugin search 天気                          # おすすめ一覧（sasanokusa.com）と npm から探す
sasacode plugin install sasacode-plugin-weather     # 入れる前に中身を見せて確認する
sasacode plugin publish weather.ts --license MIT    # 1ファイルのプラグインを npm に公開する
```

公開すると、npmのキーワード `sasacode-plugin` で誰の `search` にも出る（語なしの `search` で全部）。sasanokusa.comの一覧（`site/plugins.json`）はおすすめを載せるだけの数KBのJSONで、プラグインの本体はnpmやGitHubから直接取る。一覧に載せたいときは `site/plugins.json` に1件足すpull requestを送る。入れるのも公開するのもsasacodeに内蔵のBunを使うので、Bunやnpmを入れる必要はない（公開だけはnpmが要る）。端末のないスクリプトから `plugin install` するときは `--yes` を付ける。詳しくは [docs/plugins.md](docs/plugins.md#公開する)。

## セッション

`~/.sasacode/sessions/<作業ディレクトリ>-<ハッシュ>/<日時>_<id>.jsonl` に、メッセージとツールの結果を1件ずつ追記して保存する。途中で強制終了しても再開できる。結果が保存される前に止まった呼び出しは「実行されたか不明」としてモデルに伝え、状態を確かめてからやり直させる。書きかけの最終行は `<ファイル>.torn` に退避する。セッションIDは、TUIのフッター、`/session`、終了時の表示、ヘッドレスの出力（テキストならstderrの最後、JSONLなら先頭の `{"type":"session"}`）に出す。`SASACODE_HOME` で `~/.sasacode` の場所を変えられる。

## 開発

```bash
bun test            # LLM なしの E2E を含む全テスト（応答の再生と、テスト用の MCP サーバーを使う）
bun run typecheck
bun run build       # dist/sasacode（このマシン向け）。--all で全ターゲット
```

**リリースの手順**：`v*` のタグをpushすると、GitHub ActionsがバイナリをビルドしてGitHub Releaseを作る。タグと `packages/cli/package.json` の版が合っているかと型チェックを確かめ、macOS版はmacOSのランナーでビルドし、7種類のバイナリをすべて動く環境で起動してから（x64のmacOSはRosetta、muslはAlpineのコンテナ）、署名つきの来歴を付けて公開する。sasanokusa.comからのインストールも、それだけで新しい版になる。案内ページを変えたときと、新しい版を出した後に `scripts/publish-site.sh <ホスト>`（または環境変数 `SASACODE_SITE_HOST`）を実行する。ページの版の表記は、`packages/cli/package.json` の版で実行時に埋める。リリースが公開されたら `cd npm/sasacode && npm publish` でnpmの `sasacode` も同じ版にする（`npm/sasacode/package.json` の版は `packages/cli` と合わせておく。合っていないとリリースのワークフローが止まる）。

| パッケージ | 役割 |
| --- | --- |
| `@sasacode/ai` | 正規化したメッセージ型、Anthropic / OpenAI Chat / OpenAI Responsesのアダプタ、Models API、応答の録画・再生 |
| `@sasacode/agent` | ループ、フック、ツール実行（修復・検証・権限）、ツールの遅延ロード、セッション、プラグインのホスト |
| `@sasacode/plugin-api` | プラグインが依存する唯一の公開API（npmで公開） |
| `@sasacode/tools` | read / write / edit / bash（プラグインAPIで登録） |
| `@sasacode/tui` | pi-tuiの上に作った対話UI |
| `@sasacode/cli` | 引数、設定、キー、モデル一覧、プラグインの検出・読み込み・信頼確認、ヘッドレス実行 |
| `@sasacode/mcp` / `@sasacode/skills` / `@sasacode/bundled` | MCPとSkillsのアダプタ、同梱プラグイン |
| `@sasacode/host` | TUI・CLI・同梱プラグインが共有するもの：画面の言語（訳の表）とセッションの索引 |

コア（ai・agent・plugin-api・tools）は約 3,950 行で、上限は 4,000 行。実行時に依存するパッケージとその理由は [docs/dependencies.md](docs/dependencies.md) にまとめている。
