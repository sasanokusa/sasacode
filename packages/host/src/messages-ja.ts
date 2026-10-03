/**
 * English text as written in the code → Japanese. Everything the interface shows in English (the
 * CLI: help, errors, the trust prompt, plugin / endpoint / login) that has a Japanese form lives
 * here; anything missing simply stays English.
 */
export const TO_JA: Record<string, string> = {
  // ── main.ts: help and top-level errors ─────────────────────────────
  "sasacode — a small, pluggable coding agent\n\nUsage:\n  sasacode [prompt]              interactive session (optional first prompt)\n  sasacode -p \"<prompt>\"         run headless and print the answer\n\nOptions:\n  -p, --print <prompt>           headless mode (stdin is appended when piped)\n      --stdin                    with -p: wait for piped input however long it takes\n      --output <text|jsonl>      headless output format (default text)\n  -m, --model <provider/model>   e.g. anthropic/claude-opus-5, openai/gpt-5.5, ollama/gemma4:e4b\n                                 (the TUI's /model lists what your providers offer)\n      --permission <mode>        {modes} (default edits)\n      --thinking <level>         off | low | medium | high | xhigh | max\n      --max-turns <n>            turns per run before stopping (default 200; 0 = no limit)\n  -c, --continue                 resume the most recent session in this directory\n  -r, --resume <id>              resume a session by id\n      --no-session               do not save this session\n      --trust-project            trust this project's plugins, endpoints, MCP servers and\n                                 permission settings without asking\n\nSubcommands:\n  sasacode plugin search [words]              find plugins (sasanokusa.com's list and npm)\n  sasacode plugin install <npm-spec|git-url> [--project] [--yes]\n  sasacode plugin update [name] | remove <name> | list   (flags may go anywhere)\n  sasacode plugin publish <file.ts|dir> [--name <pkg>] [--license <id>] [--dry-run]\n  sasacode endpoint add <name> <url> [--key-env VAR] [--project]   add a server (format detected)\n  sasacode endpoint remove <name> [--project]\n  sasacode endpoint list\n  sasacode login [status|logout]    use a ChatGPT plan instead of an API key (openai-codex/… models)\n  sasacode update [--check] [tag]   download and install the newest release (checksum-verified)\n  -h, --help\n  -v, --version":
    `sasacode — 小さく、プラグインで組み替えられるコーディングエージェント

使い方:
  sasacode [prompt]              対話セッション（最初のプロンプトは省略可）
  sasacode -p "<prompt>"         ヘッドレスで実行し、答えを表示

オプション:
  -p, --print <prompt>           ヘッドレスモード（パイプした stdin は後ろに付く）
      --stdin                    -p と併用: パイプ入力をどれだけ待っても読む
      --output <text|jsonl>      ヘッドレス出力の形式（既定 text）
  -m, --model <provider/model>   例 anthropic/claude-opus-5, openai/gpt-5.5, ollama/gemma4:e4b
                                 （TUI の /model がプロバイダーの提供一覧を表示）
      --permission <mode>        {modes}（既定 edits）
      --thinking <level>         off | low | medium | high | xhigh | max
      --max-turns <n>            1回の実行のターン数上限（既定 200、0 で無制限）
  -c, --continue                 このディレクトリの直近のセッションを再開
  -r, --resume <id>              セッション ID を指定して再開
      --no-session               このセッションを保存しない
      --trust-project            このプロジェクトのプラグイン、エンドポイント、MCP サーバー、
                                 権限設定を確認なしで信頼する

サブコマンド:
  sasacode plugin search [words]              プラグインを探す（sasanokusa.com の一覧と npm）
  sasacode plugin install <npm-spec|git-url> [--project] [--yes]
  sasacode plugin update [name] | remove <name> | list   （フラグはどこに書いてもよい）
  sasacode plugin publish <file.ts|dir> [--name <pkg>] [--license <id>] [--dry-run]
  sasacode endpoint add <name> <url> [--key-env VAR] [--project]   サーバーを追加（形式は自動判別）
  sasacode endpoint remove <name> [--project]
  sasacode endpoint list
  sasacode login [status|logout]    API キーの代わりに ChatGPT プランを使う（openai-codex/… のモデル用）
  sasacode update [--check] [tag]   最新リリースをダウンロードしてインストール（チェックサム検証つき）
  -h, --help
  -v, --version`,
  "created {path}: add your API key there": "{path} を作成。ここに API キーを書く",
  "warning: {warning}": "警告: {warning}",
  "--output must be text or jsonl": "--output は text か jsonl",
  "sasacode: {error}": "sasacode: {error}",
  '--max-turns needs a whole number (0 = no limit), got "{value}"': '--max-turns は整数で指定（0 で無制限）: "{value}"',
  'unknown login subcommand "{sub}" (status, logout)': '不明な login サブコマンド "{sub}"（status, logout）',

  // ── trust.ts: the project trust prompt ─────────────────────────────
  "plugin {name}": "プラグイン {name}",
  "This project wants to use:": "このプロジェクトは次を使う:",
  "Plugins and MCP servers run code as you; endpoints receive your conversation and code.":
    "プラグインと MCP サーバーはあなたの権限でコードを実行し、エンドポイントは会話とコードを受け取る。",
  "The plugins' code could not all be read (too many files, unreadable, or a link loop), so you will be asked every time.":
    "プラグインのコードをすべて読めなかった（ファイル数超過・読み取り不能・リンクループ）。毎回確認する。",
  "Trust {cwd}? [y/N] ": "{cwd} を信頼する？ [y/N] ",

  // ── endpoints.ts: custom endpoints ─────────────────────────────────
  'provider {name} has neither "api" nor "baseUrl"; ignored': 'プロバイダー {name} は "api" も "baseUrl" もないため無視',
  "provider {name} ({url}): {error}; ignored": "プロバイダー {name}（{url}）: {error}; 無視",
  "name may contain letters, digits, _ . - only": "name は英数字と _ . - だけ",
  '"{name}" is a built-in provider; choose another name': '"{name}" は組み込みプロバイダー。別の名前にする',
  "warning: {keyEnv} is not set; detecting without a key": "警告: {keyEnv} が未設定。キーなしで判別",
  "{name}: {api}{flavour} at {url}": "{name}: {api}{flavour}（{url}）",
  "({ctx} ctx)": "（コンテキスト {ctx}）",
  "(max {max})": "（最大 {max}）",
  "… {n} more (see /model)": "… 他 {n} 件（/model を参照）",
  "saved to {path}": "保存先: {path}",
  "try: sasacode -m {spec}": "試す: sasacode -m {spec}",
  "note: endpoints in a project config are used once you trust the project (sasacode asks on the next start)":
    "注: プロジェクト設定のエンドポイントは、プロジェクトを信頼してから使う（sasacode が次回起動時に確認）",
  "{name} is not in {path}": "{name} は {path} にない",
  "removed {name} from {path}": "{name} を {path} から削除",
  "no custom endpoints (add one: sasacode endpoint add <name> <url>)": "カスタムエンドポイントなし（追加: sasacode endpoint add <name> <url>）",
  "key: {env}": "キー: {env}",
  "usage: sasacode endpoint <add <name> <url> [--key-env VAR]|remove <name>|list> [--project]":
    "使い方: sasacode endpoint <add <name> <url> [--key-env VAR]|remove <name>|list> [--project]",

  // ── loader.ts: plugins ─────────────────────────────────────────────
  "plugin {path} skipped: {error}": "プラグイン {path} を飛ばした: {error}",
  "needs plugin API {need}, host provides {have}": "プラグイン API {need} が必要（ホストは {have} を提供）",
  "{file} is not a file inside {dir}": "{file} は {dir} の中のファイルではない",
  "{file} has no default export function": "{file} に default export の関数がない",
  "{cmd} failed": "{cmd} が失敗",
  "stopped: pass --yes to go ahead without a question (no terminal to ask on)": "中止: 質問なしで進めるには --yes（確認できる端末がない）",
  "{question} [y/N] ": "{question} [y/N] ",
  "Install it?": "入れる？",
  "usage: sasacode plugin <command> [--project]\n  search [words]                       find plugins (the list on sasanokusa.com and npm); no words lists all\n  install <npm-spec|git-url|dir> [--yes]  add one (shows what it is and asks first)\n  update [name] [--yes]                update npm plugins, or pull a git one (shows the changes and asks)\n  remove <name> [--yes]\n  list\n  publish <file.ts|dir> [--name <pkg>] [--version <v>] [--api <range>] [--license <id>] [--description <text>] [--otp <code>] [--dry-run]":
    `使い方: sasacode plugin <command> [--project]
  search [words]                       プラグインを探す（sasanokusa.com の一覧と npm）。語を指定しなければ全件
  install <npm-spec|git-url|dir> [--yes]  ひとつ追加（内容を示してから確認）
  update [name] [--yes]                npm のプラグインを更新、git のものは pull（変更を示してから確認）
  remove <name> [--yes]
  list
  publish <file.ts|dir> [--name <pkg>] [--version <v>] [--api <range>] [--license <id>] [--description <text>] [--otp <code>] [--dry-run]`,
  "error: {error}": "エラー: {error}",
  "warning: could not search {error}": "警告: 検索できなかった: {error}",
  "A plugin runs inside sasacode with your permissions: install only code you trust.":
    "プラグインはあなたの権限で sasacode 内部から動く。信頼できるコードだけ入れる",
  "{path} already exists; remove it first or use plugin update": "{path} はすでに存在。先に消すか plugin update を使う",
  "cloned into {path}": "{path} に clone",
  '{name} has no plugin.json or "sasacode" field in package.json, so it is not a sasacode plugin and was not installed':
    '{name} には plugin.json も package.json の "sasacode" もないため、sasacode のプラグインではない。インストールしなかった',
  "installed {name} at commit {commit}": "{name} を commit {commit} に入れた",
  "installed {name}@{version} into {dir}": "{name}@{version} を {dir} に入れた",
  "{spec} is not an installed npm or git plugin here": "{spec} はここでは npm でも git でもインストール済みのプラグインではない",
  "no npm or git plugins to update": "更新する npm か git のプラグインがない",
  "{name}: {version} (already the newest)": "{name}: {version}（すでに最新）",
  "{name}: {version} →": "{name}: {version} →",
  "Update {name}?": "{name} を更新する？",
  "{name}: {before} → {after}": "{name}: {before} → {after}",
  "{name}: already the newest ({commit})": "{name}: すでに最新（{commit}）",
  "{name}: new commits\n{log}\n{stat}": "{name}: 新しいコミット\n{log}\n{stat}",
  "{name}: now at {commit}": "{name}: いまは {commit}",
  "publishing uses npm (npm publish): install Node.js / npm first": "公開は npm 経由（npm publish）。先に Node.js / npm を入れる",
  "{name}@{version}  (plugin API {api})\n  {dir}": "{name}@{version}（plugin API {api}）\n  {dir}",
  "note: {note}": "注: {note}",
  "published: others can run  sasacode plugin install {name}": "公開した。他の人はこれで入れられる  sasacode plugin install {name}",
  "{spec}: give the plugin's name as `sasacode plugin list` shows it, not a path":
    "{spec}: パスではなく `sasacode plugin list` に表示されるプラグイン名を指定する",
  "Remove {name} from {dir}?": "{name} を {dir} から消す？",
  "removed {name}": "{name} を削除",

  // ── plugin-share.ts: search, describe, publish ─────────────────────
  "{url} returned {status}": "{url} は {status} を返した",
  "list: {error}": "一覧: {error}",
  "npm: {error}": "npm: {error}",
  "no plugins found": "プラグインが見つからない",
  "by {author}": "作者: {author}",
  "{name}@{version}  (local: {dir})": "{name}@{version}（ローカル: {dir}）",
  '{name} is not a sasacode plugin (no "sasacode" field in its package.json), so it was not installed.':
    '{name} は sasacode のプラグインではない（package.json に "sasacode" がない）ため、インストールしなかった。',
  "Did you mean {alt}?  sasacode plugin install {alt}": "{alt} のことですか？  sasacode plugin install {alt}",
  "{name}: not a sasacode plugin, skipped (sasacode plugin remove {name})": "{name}: sasacode のプラグインではないので飛ばした（sasacode plugin remove {name}）",
  "{name}  [{scope}]  not a sasacode plugin, never loaded: sasacode plugin remove {name}{flag}":
    "{name}  [{scope}]  sasacode のプラグインではなく、読み込まれていない: sasacode plugin remove {name}{flag}",
  "(range {range}: the newest match is installed)": "（{range} のうち最新をインストール）",
  "published by {people}": "公開者: {people}",
  "on {date}": "（{date}）",
  "{count} files, {size}": "{count} ファイル、{size}",
  "size unknown": "サイズ不明",
  "depends on {deps}": "依存: {deps}",
  "has install scripts (not run: packages are added without them)": "install scripts あり（実行されない: それなしで追加する）",
  "source {repo}": "ソース: {repo}",
  "{file}: a plugin file ends in .ts, .js or .mjs": "{file}: プラグインファイルは .ts, .js, .mjs で終わる",
  '"{name}" is not a valid npm package name; pass --name': '"{name}" は npm パッケージ名として不正。--name を指定',
  "it imports other local files, which are not included: publish its directory instead":
    "他のローカルファイルを import しているが同梱されない。ディレクトリごと公開する",
  "no license given: others may not reuse it (add one with --license MIT, for example)":
    "license 未指定。他人は再利用できない（--license MIT などで足す）",
  "plugin API {api} does not match this sasacode ({version})": "plugin API {api} はこの sasacode（{version}）と合わない",
  "{dir} has no package.json; publish a single .ts file instead, or add one": "{dir} に package.json がない。1つの .ts を公開するか、足す",
  '{path} has no "sasacode" field (the manifest: extensions, apiVersion, skills)':
    '{path} に "sasacode" がない（マニフェスト: extensions, apiVersion, skills）',
  'added the "{keyword}" keyword to package.json so that plugin search finds it':
    '検索に引っかかるよう package.json に "{keyword}" キーワードを追加',

  // ── headless.ts: `sasacode -p` output ──────────────────────────────
  "↻ repaired {name}: {note}": "↻ {name} を修復: {note}",
  "plugin {plugin} ({hook}): {error}": "プラグイン {plugin}（{hook}）: {error}",
  "[stopped: {cause}{hint}]": "[停止: {cause}{hint}]",
  'session {id} · continue: sasacode -r {id} -p "…"': 'session {id} · 続ける: sasacode -r {id} -p "…"',
  "turn limit reached (--max-turns or maxTurns in config; 0 = no limit)": "ターン数の上限（--max-turns か config の maxTurns。0 で無制限）",
  "context window is full": "コンテキストウィンドウが満杯",

  // ── setup.ts: warnings and resume errors ───────────────────────────
  "session model {model} unavailable: {error}": "セッションのモデル {model} は使えない: {error}",
  "no previous session in this directory": "このディレクトリに過去のセッションがない",
  "session {id} belongs to {cwd}: run sasacode from there": "セッション {id} は {cwd} のもの。そこで sasacode を実行する",
  "session not found: {id}": "セッションがない: {id}",
  "project plugins not loaded until you trust this project: {names}": "このプロジェクトを信頼するまでプロジェクトのプラグインは読まない: {names}",
  "plugin {name}: skills folder {folder} is outside the plugin; ignored": "プラグイン {name}: skills フォルダ {folder} がプラグインの外。無視",
  "plugin {name} skipped: {error}": "プラグイン {name} を飛ばした: {error}",

  // ── config.ts: config warnings ─────────────────────────────────────
  "{source}: not a JSON object; ignored": "{source}: JSON オブジェクトではない。無視",
  '{source}: unknown key "{key}" ignored': '{source}: 不明なキー "{key}" は無視',
  '{source}: "{key}" has an invalid value; ignored': '{source}: "{key}" の値が不正。無視',
  ".sasacode/config.json: trustedProjects is only read from the global config; ignored":
    ".sasacode/config.json: trustedProjects はグローバル設定からのみ読む。無視",
  ".sasacode/config.json: this project selects the model {model}": ".sasacode/config.json: このプロジェクトがモデル {model} を選ぶ",
  ".sasacode/config.json: not applied until you trust this project ({items})":
    ".sasacode/config.json: このプロジェクトを信頼するまで適用しない（{items}）",

  // ── env.ts: the generated ~/.sasacode/.env comments ────────────────
  "sasacode API keys. Fill in the providers you use; blank entries are ignored.": "sasacode の API キー。使うプロバイダーだけ埋める。空欄は無視する。",
  "Read on every start, from any directory. A project's ./.env is not read.": "どのディレクトリからでも起動のたびに読む。プロジェクトの ./.env は読まない。",
  "Commands the agent runs (bash, background jobs) do not see what is set here.": "エージェントが実行するコマンド（bash・バックグラウンドジョブ）には渡らない。",
  "(e.g. -m {example})": "（例 -m {example}）",
  "Ollama (ollama/<model>) runs locally and needs no key.": "Ollama（ollama/<model>）はローカルで動き、キーが要らない。",

  // ── update.ts: `sasacode update` ───────────────────────────────────
  "cannot tell the latest version ({status} from {base})": "最新版がわからない（{base} から {status}）",
  "a newer version is available: {latest} (this is {version}) — run `{how}`":
    "新しい版がある: {latest}（いまは {version}）— `{how}` を実行",
  "unsupported OS: {platform}": "未対応の OS: {platform}",
  "unsupported CPU: {arch}": "未対応の CPU: {arch}",
  "this sasacode runs from source: update the checkout (git pull && bun install) instead of `sasacode update`":
    "この sasacode はソースから動いている。`sasacode update` の代わりにチェックアウトを更新（git pull && bun install）",
  "newer version available: {wanted} (this is {version})": "新しい版がある: {wanted}（いまは {version}）",
  "sasacode is up to date ({version})": "sasacode は最新（{version}）",
  "downloading {url} …": "{url} をダウンロード中…",
  "{file} is not listed in SHA256SUMS": "{file} は SHA256SUMS にない",
  "checksum mismatch for {file}": "{file} のチェックサムが合わない",
  "could not unpack {file}": "{file} を展開できなかった",
  "the downloaded binary does not run on this system": "ダウンロードしたバイナリがこのシステムで動かない",
  "updated {from} → {to}: {target} (restart sasacode to use it)": "{from} → {to} に更新: {target}（使うには sasacode を再起動）",
  // ── the rest: trust-prompt items, stdin, JSON files ────────────────
  "endpoint {name} ({url})": "エンドポイント {name}（{url}）",
  "model {model}": "モデル {model}",
  "models {models}": "モデル一覧 {models}",
  "model overrides for {models}": "モデル設定の上書き（{models}）",
  "MCP server {name}": "MCP サーバー {name}",
  "disables plugins {names}": "プラグインの無効化 {names}",
  "settings for plugins {names}": "プラグインの設定 {names}",
  "allow rules {rules}": "許可ルール {rules}",
  "permission mode {mode}": "権限モード {mode}",
  "maxTurns unlimited": "maxTurns 無制限",
  "unknown permission mode \"{mode}\" ({modes})": "不明な権限モード \"{mode}\"（{modes}）",
  "failed to read {path}: {error}": "{path} を読めなかった: {error}",
  "warning: nothing arrived on stdin within {s}s, so it was not read (pass --stdin to wait for it)": "警告: {s} 秒待っても stdin に何も届かなかったので読まなかった（待つには --stdin）",
  // ── bundled plugins ───────────────────────────────────────────────
  "unknown permission preset \"{name}\" ({known})": "不明な権限プリセット \"{name}\"（{known}）",
  "browsr: unknown mode \"{mode}\" ({known})": "browsr: 不明なモード \"{mode}\"（{known}）",
  "jev-guard is no longer bundled, so the Jev check your config turns on is not running: sasacode plugin install sasacode-plugin-jev-guard":
    "jev-guard は同梱ではなくなったため、config で有効にしている Jev の判定は動いていない: sasacode plugin install sasacode-plugin-jev-guard",
  "could not pack {dir}": "{dir} をまとめられなかった",
  "the connection dropped mid-reply; asking again": "接続が応答の途中で切れたため、やり直す",
  "this sasacode was installed with npm: update it with `npm install -g sasacode@latest`": "この sasacode は npm で入れたもの。更新は `npm install -g sasacode@latest`",
};
