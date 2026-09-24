# sasacode

プラグインで組み替えられる、ターミナル向けの小さなコーディングエージェント（TypeScript / Bun）。

コアに入っているのは、エージェントループ、プロバイダー、4つの組み込みツール（read / write / edit / bash）、TUI、セッション、権限確認だけ。それ以外の振る舞いは、すべて公開のプラグイン API で足す。組み込みツールも同じ API で登録している。MCP サーバーと Agent Skills（`SKILL.md`）はそのまま使える。小型モデルでも実用になるよう、壊れたツール呼び出しの修復と、同じ文の繰り返しの抑止も同梱している。

要件は [docs/requirements.md](docs/requirements.md)。マイルストーン M0〜M4 はすべて完了した（検証の記録は [docs/milestones.md](docs/milestones.md)）。

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

- macOS（arm64 / x64）と Linux（x64 / arm64、glibc / musl、AVX2 のない CPU 向けの baseline 版）の単一バイナリ。Bun は不要。
- インストーラーもバイナリも、[GitHub Releases](https://github.com/sasanokusa/sasacode/releases) の最新版から取得して SHA256 を検証する。sasanokusa.com の URL は、最新リリースの `install.sh` へのリダイレクト。
- `SASACODE_VERSION=v0.8.1` で版を固定でき、`SASACODE_INSTALL_DIR` で置き場所を変えられる。Windows は WSL から使う。

ソースから使う場合（[Bun](https://bun.sh) 1.4 以上）：

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
echo "$LOG" | sasacode -p "原因は？"   # パイプした入力は -p の後ろに付く
sasacode -c                         # このディレクトリの直近のセッションを再開
sasacode -r <id> -p "続き"            # セッション ID を指定して続ける（tmux がなくても会話を続けられる）
```

| オプション | 内容 |
| --- | --- |
| `-m <provider/model>` | モデル（例 `anthropic/claude-opus-5`、`openai/gpt-5.5`、`ollama/gemma4:e4b`） |
| `--permission <edits\|ask\|agent\|auto>` | 権限モード（既定 `edits`） |
| `--thinking <off\|low\|medium\|high\|xhigh\|max>` | 推論の深さ |
| `--max-turns <n>` | 1回の実行のターン数の上限（既定 200、0 で無制限） |
| `-c` / `-r <id>` / `--no-session` | 再開 / ID を指定して再開 / 保存しない |
| `--trust-project` | プロジェクトを確認なしで信頼する（ヘッドレス向け。[プロジェクトの信頼](#プロジェクトの信頼)） |
| `sasacode plugin install\|remove\|list` | プラグインの管理（npm か git URL） |
| `sasacode endpoint add\|remove\|list` | 自前のサーバーの追加（形式は自動判別） |
| `sasacode login [status\|logout]` | ChatGPT プランでログイン（`openai-codex/…` のモデル用） |

### TUI

| キー | 動作 |
| --- | --- |
| enter / shift+enter・alt+enter | 送信 / 改行 |
| ↑↓ | 入力履歴 |
| tab | 補完（スラッシュコマンド、`/model` のモデル名、ファイルパス） |
| esc | 生成・ツール実行を中断（中断したことは次の入力でモデルに伝わる） |
| 実行中に enter | メッセージを予約し、次のターンで届ける |
| ctrl+o | ツール出力と thinking の折りたたみを切り替え |
| shift+tab | 権限モードを順に切り替え |
| `/exit`・ctrl+c ×2・ctrl+d | 終了（終了時に `再開: sasacode -r <id>` を表示） |

起動時に、①のような幅の曖昧な文字（East Asian Ambiguous）を端末が何マスで表示するかを問い合わせて、表示の計算を合わせる。端末が答えない場合は1マスとして扱う。`SASACODE_AMBIGUOUS_WIDTH=1` か `2` で明示できる。

| コマンド | 内容 |
| --- | --- |
| `/model` | プロバイダーから取得したモデル一覧から選ぶ。文字を打つと絞り込める。`/model <provider/model>` で直接指定、`/model --refresh` で取り直す |
| `/resume` `/clear` `/fork` `/session` | 過去のセッション / 新しいセッション / 過去のメッセージから分岐 / ID と保存先 |
| `/effort` | 推論の深さ（`off` / `low` / `medium` / `high` / `xhigh` / `max`）を切り替える。引数なしなら一覧から選ぶ。現在の値はフッターに出る |
| `/permission` `/help` `/exit`（`/quit`） | 権限モード / ヘルプ / 終了 |
| `/compact` `/mcp` `/skills` `/skill:<name>` `/presets` `/browsr` | 同梱プラグインのコマンド |

フッターには、モデル、権限モード、コンテキストの使用量、セッション ID と、プラグインのステータス（MCP の接続数、TODO の進捗など）を出す。

## プロバイダーとモデル

モデルは `<provider>/<model>` で指定する。

| provider | API 形式 | キー |
| --- | --- | --- |
| `anthropic` | Anthropic Messages | `ANTHROPIC_API_KEY`（空なら SDK が `ant auth login` のプロファイルを使う） |
| `openai` / `openai-chat` | OpenAI Responses / Chat Completions | `OPENAI_API_KEY` |
| `openrouter` | Chat Completions | `OPENROUTER_API_KEY` |
| `commandcode` / `commandcode-responses` / `commandcode-anthropic` | Command Code の Chat Completions / Responses / Messages | `CMD_API_KEY` |
| `ollama` | Chat Completions（localhost:11434） | 不要 |
| `openai-codex` | ChatGPT プランの Codex バックエンド（Responses） | 不要（`sasacode login` で ChatGPT アカウントにログイン） |

**ChatGPT プランで使う**：`sasacode login` を実行するとブラウザが開き、ChatGPT アカウントでログインすると `openai-codex/…` のモデル（`/model` に一覧が出る）が API キーなしで使える。利用分はプランの Codex の枠から引かれる。認証情報は `~/.sasacode/codex-auth.json`（本人だけが読める権限）に保存し、期限が近づけば自動で更新する。SSH 先などでブラウザがこのマシンに戻れないときは、ログイン後にブラウザが表示した URL を貼り付ける。`sasacode login status` で状態を確認、`sasacode login logout` で削除。API キーが1つもなくログインだけしている場合は、`openai-codex/gpt-5.5` が既定のモデルになる。これは OpenAI の Codex CLI と同じ仕組み（公開クライアントと PKCE）を使う非公式の対応で、OpenAI の都合で使えなくなることがある。

### 自前のサーバー（llama.cpp、vLLM、LM Studio、プロキシなど）

```bash
sasacode endpoint add llama http://192.168.1.20:8080            # 形式を判別して ~/.sasacode/config.json に保存
sasacode endpoint add mygw https://llm.example.com/v1 --key-env MYGW_KEY
sasacode endpoint list
sasacode -m llama/<モデル名>
```

`GET /v1/models` の応答から、OpenAI 形式（Chat Completions で呼ぶ）か Anthropic 形式（Messages で呼ぶ）かを自動で判別し、URL も各形式の SDK に合わせて整える。llama.cpp と Ollama は、それぞれ `/props` と `/api/show` から実際のコンテキスト長も取る。追加したサーバーのモデルは、`/model` の一覧に自動で出る。設定ファイルに `"providers": { "llama": { "baseUrl": "http://…" } }` と URL だけ書いた場合も、起動時に判別する（結果は `~/.sasacode/endpoints.json` にキャッシュする）。

プロジェクトの `.sasacode/config.json` に書いたエンドポイントは、そのプロジェクトを信頼するまで使わない（[プロジェクトの信頼](#プロジェクトの信頼)）。キーを使わないサーバーでも、会話とコードが送られるため。

**モデル一覧の自動取得**：TUI の起動時に、キーのあるプロバイダー（と Ollama）の Models API（`GET /v1/models`）をバックグラウンドで呼び、`/model` に一覧とコンテキスト長を出す。取得したコンテキスト長は、フッターの使用率や圧縮のしきい値にも使う（設定の `modelOverrides` があればそちらが優先）。Ollama は独自の `/api/show` から読み、Modelfile の `num_ctx` があればそれを実際の長さとして使い、なければモデルの最大長を参考として表示する。OpenAI の Models API はコンテキスト長を返さないので、組み込みのモデル表の値で補う。Command Code のように1つのキーで複数の API 形式を提供するサービスでは、モデルごとに対応する形式のプロバイダーへ自動で振り分ける（例：Claude 系は `commandcode-anthropic/…`）。

**API キー**は次の順で探す：シェルの環境変数 → `~/.sasacode/.env` → OS キーチェーン（macOS: `security add-generic-password -s sasacode -a <provider> -w`、Linux: `secret-tool store --label sasacode service sasacode account <provider>`）。どこでも空欄は無視する。作業ディレクトリの `.env` と `bunfig.toml` は読まない（リポジトリが `ANTHROPIC_BASE_URL` などで接続先を差し替えたり、起動時にスクリプトを実行させたりできないようにするため）。セッションのログには、使用中のキーを `[REDACTED]` に置き換えてから書く。

## 設定

`~/.sasacode/config.json`（全体）と `<project>/.sasacode/config.json`（プロジェクト）をマージし、プロジェクトが優先する。ただし、プロジェクト設定のうち信頼が必要な部分は、信頼するまで使わない（下記）。権限ルールと `disabled`（ツール・プラグイン）の配列は両方を合わせ、全体設定の deny / ask や無効化をプロジェクト側から消すことはできない。型の合わない値や未知のキーは、警告を出して無視する。

```json
{
  "model": "commandcode/deepseek/deepseek-v4-flash",
  "models": ["commandcode/deepseek/deepseek-v4-pro", "ollama/gemma4:e4b"],
  "thinking": "high",
  "providers": { "myproxy": { "api": "openai-chat", "baseUrl": "https://…/v1", "apiKeyEnv": "MY_KEY" } },
  "modelOverrides": { "myproxy/some-model": { "contextWindow": 200000 } },
  "permissions": {
    "mode": "edits",
    "allow": ["bash(git status*)", "bash(bun test*)"],
    "ask": ["bash(git push*)"],
    "deny": ["bash(rm -rf *)", "edit(.env)"]
  },
  "instructions": "システムプロンプトに追記するテキスト",
  "mcpServers": { "github": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"] } },
  "plugins": { "disabled": ["web-fetch"], "settings": { "permission-presets": { "presets": ["guard", "tests"] } } },
  "tools": { "disabled": [] },
  "toolSearch": { "mode": "auto", "percent": 10, "count": 30 },
  "trustedProjects": ["/path/to/project"]
}
```

`models` は `/model` の一覧の先頭に出す。`providers` で OpenAI 互換や Anthropic 互換のエンドポイントを足せる。

`maxTurns`（既定 200、0 で無制限）は、1回の依頼でモデルに送るリクエスト数の上限。達すると、対話では続けるかを尋ね、ヘッドレスでは止まる。これとは別に、同梱の `loop-guard` が、ツール呼び出しがどれも実行されない（拒否や引数の不備が続く）ターンが5回続いたら実行を止める（`plugins.settings["loop-guard"].noProgressTurns` で変更、0 で無効）。

### 権限

| モード | 動作 |
| --- | --- |
| `edits`（既定） | 作業ディレクトリ内の read / write / edit は自動で許可し、bash やディレクトリ外への操作は確認する |
| `ask` | 読み取りを含む、すべてのツール実行を確認する |
| `agent` | 作業ディレクトリ内の read は自動で許可し、それ以外は現在のモデルに危険度を判定させる。安全なら許可、そうでなければ確認する |
| `auto` | すべて許可する（deny ルールだけは効く） |

- ルールは `ツール名` か `ツール名(パターン)`。bash はコマンド文字列を `*` のワイルドカードで、read / write / edit はパスを glob で照合する。
- 判定の順番は deny → ask → allow → モード。allow ルールで通るのは、`&&` `;` `|` でつないだコマンドの**すべて**が許可されている場合だけ。`$(…)`、バッククォート、`>` を含むコマンドは、allow ルールでは通らない。
- 同梱の `permission-presets` が、既定で `guard`（sudo、`rm -rf ~`、force push、`| sh` などを常に拒否）を有効にしている。
- ヘッドレスでは確認できる人がいないので、確認が必要な呼び出しは実行せず、その理由をモデルに返す。
- パスは、シンボリックリンクをたどった先（実パス）で判定する。作業ディレクトリ内のリンクが外を指していれば、リンク先がまだ存在しなくても外への操作として扱う。プロジェクト内のリンクで、ルールの対象をすり替えることもできない。リンクの循環などで行き先が決まらないパスは、モードによらず確認する。
- 権限は、ツールを呼ぶ前の判定であって隔離（サンドボックス）ではない。判定から実行までの間にリンクを差し替えるような操作や、許可した bash コマンドの中身までは防げない。信頼できないコードを扱うときは、コンテナなど OS 側で隔離すること。
- 中断（esc）した後は、承認済みでもまだ始まっていないツールは実行しない。

### プロジェクトの信頼

クローンしたリポジトリは、そのままでは「制限を強める」ことしかできない。次のものは、コードを実行するか、データの送り先を変えうるので、プロジェクトを信頼するまで使わない。

- `.sasacode/plugins` のプラグイン
- `.sasacode/config.json` の `providers`（エンドポイント）、`modelOverrides`、`plugins`（無効化と設定）、`mcpServers`、`allow` ルール、全体設定より緩い権限モード、全体設定より大きい `maxTurns`（0 は上限なし）と `maxRetries`、そのプロジェクトにしかないプロバイダーを指す `model`

これらがあるプロジェクトで起動すると、最初に一覧を出して信頼するかを尋ねる（ヘッドレスでは `--trust-project`）。答えは `~/.sasacode/trust.json` に保存し、上の設定かプラグインのコード（`.sasacode/plugins` 以下のファイル。依存パッケージの中身は除く）が変わったら再び尋ねる。全体設定の `trustedProjects` に入れたプロジェクトは、尋ねずに信頼する。モデルの選択、`thinking`、`instructions`、deny / ask ルールやツール無効化の追加、より厳しい権限モードや小さい上限などは、信頼しなくても使う。

## プラグイン・MCP・Skills

### 同梱プラグイン（`plugins.disabled` で外せる）

| 名前 | 内容 |
| --- | --- |
| `tool-repair` | 検証の前に、壊れたツール呼び出しを直す。直す対象は、JSON の崩れ（末尾カンマ、閉じ括弧、クォートのないキー、`True`/`None`、コードフェンス、二重エンコード）、キー名（`file` → `path`）、型（`"20"` → `20`）、ツール名の typo。候補が1つに絞れるときだけ直す。直したことはモデルに短く伝え、履歴には直した後の形を残し、元の出力はセッションに記録する |
| `repetition-guard` | 生成中に同じ文を繰り返し始めたら止め、1回分だけ残して、続きを促す（「なるほど。」のような短い繰り返しも、長く続けば止める） |
| `loop-guard` | 同じツール呼び出し（または A→B→A→B のような3手までの周期）が同じ結果で続いたら、3回目に注意を添え、5回目からはその周期の呼び出しをすべて止める（ファイルを変更するか、次の依頼で解除）。同じ応答の中で、間に状態を変えうる呼び出しがない重複は1回だけ実行する。同じエラーの繰り返し（引数の不備などで実行されなかった呼び出しを含む）も知らせる。ツールが1件も実行されないターンが5回続いたら、実行を止める |
| `agents-md` | `~/.sasacode/AGENTS.md` と、リポジトリのルートから作業ディレクトリまでの `AGENTS.md` を読む（CLAUDE.md は読まない） |
| `compaction` | コンテキストが 80% に達するか上限に来たら、古い履歴を要約する。`/compact` で手動実行 |
| `subagent` | `task` ツール。別の履歴を持つサブエージェントに作業を任せ、報告だけを受け取る（1ターンに複数あれば並行して動く） |
| `todo` | `todo_write` ツール。作業計画をセッションに残し、進捗をフッターに出す |
| `web-fetch` | `web_fetch` ツール。URL を取ってきてテキストにする |
| `browsr` | [browsr-4-agent](https://github.com/sasanokusa/browsr-4-agent) による Web 検索（`search`）と本文の閲覧（`open`）。`browsr-agent` が PATH にあれば自動で起動する |
| `permission-presets` | 権限ルールのプリセット（`guard`、`read-only-shell`、`tests`） |
| `openai-codex` | ChatGPT プランのモデル（`openai-codex/…`）。`sasacode login` の認証情報を使う |
| `skills` / `mcp` | Agent Skills と MCP のアダプタ |

### MCP

```json
{
  "mcpServers": {
    "github": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" } },
    "remote": { "url": "https://example.com/mcp", "headers": { "Authorization": "Bearer ${REMOTE_TOKEN}" } }
  }
}
```

stdio と Streamable HTTP に対応する。ツールは `mcp__<server>__<tool>` という名前になり、通常のツールと同じ権限確認とフックを通る。prompts はスラッシュコマンドになる。サーバーはバックグラウンドで接続するので、起動を待たせない。`${VAR}` は環境変数から展開する。

### Skills

`~/.sasacode/skills/<name>/SKILL.md`、`.sasacode/skills/<name>/SKILL.md`、プラグインの `skills` ディレクトリから、標準の Agent Skills 形式（frontmatter の `name` と `description`）を読む。システムプロンプトには名前・説明・パスだけを載せ、本文はモデルが必要なときに read ツールで読む。`/skill:<name> <依頼>` で明示的に使わせることもできる。

組み込みの Skill として `tool-authoring`（sasacode のツールとプラグインの作り方）を同梱している。`/skill:tool-authoring 天気を返すツールを作って` のように使う。

### ツールの遅延ロード

MCP とプラグインのツールの定義が、合計でコンテキストの 10% 以上か 30 個以上になると、定義を全部は送らない。名前と説明の一覧を持つ `tool_search` ツールだけを渡し、モデルが必要なものを検索して読み込む。組み込みの4ツールと `alwaysLoad` 付きのツールは常に送る。

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

公開 API（現在 1.6.0。1.4.0 で締め切り、1.x の間は追加だけ。[互換性の約束](docs/plugins.md#互換性の約束140-で確定)）でできることは次のとおり。

- **登録**：ツール、スラッシュコマンド（引数の補完つき）、プロバイダー、権限ルール
- **フック**：`session_start/end`、`user_prompt`、`system_prompt`、`before_request`、`stream_delta`、`assistant_message`、`tool_call_raw`、`tool_call`、`tool_result`、`turn_end`、`agent_end`、`context_limit`
- **UI**：通知・確認・選択・ステータス行、ツール結果の描画
- **セッション**：状態の保存、履歴の差し替え、メッセージの差し込み
- **エージェント**：サブエージェント、単発の呼び出し

詳しくは [docs/plugins.md](docs/plugins.md)。

## セッション

`~/.sasacode/sessions/<作業ディレクトリ>-<ハッシュ>/<日時>_<id>.jsonl` に、メッセージとツールの結果を1件ずつ追記して保存する。途中で強制終了しても再開できる。結果が保存される前に止まった呼び出しは「実行されたか不明」としてモデルに伝え、状態を確かめてからやり直させる。書きかけの最終行は `<ファイル>.torn` に退避する。セッション ID は、TUI のフッター、`/session`、終了時の表示、ヘッドレスの出力（テキストなら stderr の最後、JSONL なら先頭の `{"type":"session"}`）に出す。`SASACODE_HOME` で `~/.sasacode` の場所を変えられる。

## 開発

```bash
bun test            # LLM なしの E2E を含む全テスト（応答の再生と、テスト用の MCP サーバーを使う）
bun run typecheck
bun run build       # dist/sasacode（このマシン向け）。--all で全ターゲット
```

**リリースの手順**：`v*` のタグを push すると、GitHub Actions がバイナリをビルドして GitHub Release を作る。macOS 版は macOS のランナーでビルドし、起動を確認する。sasanokusa.com からのインストールも、それだけで新しい版になる。案内ページを変えたときだけ `scripts/publish-site.sh` を実行する。

| パッケージ | 役割 |
| --- | --- |
| `@sasacode/ai` | 正規化したメッセージ型、Anthropic / OpenAI Chat / OpenAI Responses のアダプタ、Models API、応答の録画・再生 |
| `@sasacode/agent` | ループ、フック、ツール実行（修復・検証・権限）、ツールの遅延ロード、セッション、プラグインのホスト |
| `@sasacode/plugin-api` | プラグインが依存する唯一の公開 API（npm で公開） |
| `@sasacode/tools` | read / write / edit / bash（プラグイン API で登録） |
| `@sasacode/tui` | pi-tui の上に作った対話 UI |
| `@sasacode/cli` | 引数、設定、キー、モデル一覧、プラグインの検出・読み込み・信頼確認、ヘッドレス実行 |
| `@sasacode/mcp` / `@sasacode/skills` / `@sasacode/bundled` | MCP と Skills のアダプタ、同梱プラグイン |

コア（ai・agent・plugin-api・tools）は約 3,000 行で、上限は 4,000 行。実行時に依存するパッケージとその理由は [docs/dependencies.md](docs/dependencies.md) にまとめている。
