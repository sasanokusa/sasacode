//! English text as written in the code → Japanese. Everything the interface shows in English (the
//! CLI: help, errors, the trust prompt, plugin / endpoint / login) that has a Japanese form lives
//! here; anything missing simply stays English.
//!
//! Generated from `packages/host/src/messages-ja.ts` (every entry verbatim, in its order; a key the TS
//! object listed twice appears once, with the value that won).

/// 201 entries.
pub static TO_JA: &[(&str, &str)] = &[
    (
        "sasacode — a small, pluggable coding agent\n\nUsage:\n  sasacode [prompt]              interactive session (optional first prompt)\n  sasacode -p \"<prompt>\"         run headless and print the answer\n\nOptions:\n  -p, --print <prompt>           headless mode (stdin is appended when piped)\n      --stdin                    with -p: wait for piped input however long it takes\n      --output <text|jsonl>      headless output format (default text)\n      --control stdio            with -p --output jsonl: the parent answers approvals, sends messages and can abort\n                                 over stdin as JSON Lines (stdin is then not appended to the prompt)\n  -m, --model <provider/model>   e.g. anthropic/claude-opus-5, openai/gpt-5.5, ollama/gemma4:e4b\n                                 (the TUI's /model lists what your providers offer)\n      --permission <mode>        {modes} (default edits)\n      --thinking <level>         off | low | medium | high | xhigh | max\n      --max-turns <n>            turns per run before stopping (default 200; 0 = no limit)\n  -c, --continue                 resume the most recent session in this directory\n  -r, --resume <id>              resume a session by id\n      --no-session               do not save this session\n      --append-system-prompt-file <path>\n                                 add the file's text to the system prompt, after the config's instructions\n      --trust-project            trust this project's endpoints, MCP servers, plugin settings and\n                                 permission settings without asking\n      --allow <rule>             allow calls matching a permission rule for this run, as answering \"always\" does\n                                 (repeatable; e.g. \"bash(npm test)\", the rules of approval_request)\n      --slash                    with -p: a prompt that starts with /<command> runs that plugin command\n\nSubcommands:\n  sasacode endpoint add <name> <url> [--key-env VAR] [--project]   add a server (format detected)\n  sasacode endpoint remove <name> [--project]\n  sasacode endpoint list\n  sasacode list models|commands|skills [--json]   what a run here can use (the GUI asks with --json)\n  sasacode trust [--yes] [--json]   is this folder trusted, and what would trust enable; --yes trusts it and lets git run here\n  sasacode plugin list --json       the bundled plugins and whether each is on (the GUI asks)\n  sasacode login [status|logout]    use a ChatGPT plan instead of an API key (openai-codex/… models)\n  sasacode update [--check] [tag]   download and install the newest release (checksum-verified)\n  sasacode web-search install       install RustXNG for the web_search tool (checksum-verified)\n  sasacode gui [folder]             open the desktop GUI on a folder (sasacode gui install puts it in)\n  sasacode gui install [--path <dir>]   build the desktop GUI on this machine with Rust (docs/gui.md)\n  -h, --help\n  -v, --version",
        "sasacode — 小さく、プラグインで組み替えられるコーディングエージェント\n\n使い方:\n  sasacode [prompt]              対話セッション（最初のプロンプトは省略可）\n  sasacode -p \"<prompt>\"         ヘッドレスで実行し、答えを表示\n\nオプション:\n  -p, --print <prompt>           ヘッドレスモード（パイプした stdin は後ろに付く）\n      --stdin                    -p と併用: パイプ入力をどれだけ待っても読む\n      --output <text|jsonl>      ヘッドレス出力の形式（既定 text）\n      --control stdio            -p --output jsonl と併用: 親プロセスが stdin に JSON Lines を送り、\n                                 承認への答え・メッセージ・中断を伝える（stdin はプロンプトに付かない）\n  -m, --model <provider/model>   例 anthropic/claude-opus-5, openai/gpt-5.5, ollama/gemma4:e4b\n                                 （TUI の /model がプロバイダーの提供一覧を表示）\n      --permission <mode>        {modes}（既定 edits）\n      --thinking <level>         off | low | medium | high | xhigh | max\n      --max-turns <n>            1回の実行のターン数上限（既定 200、0 で無制限）\n  -c, --continue                 このディレクトリの直近のセッションを再開\n  -r, --resume <id>              セッション ID を指定して再開\n      --no-session               このセッションを保存しない\n      --append-system-prompt-file <path>\n                                 ファイルの中身を、config の instructions の後ろにシステムプロンプトとして足す\n      --trust-project            このプロジェクトのエンドポイント、MCP サーバー、プラグインの設定、\n                                 権限設定を確認なしで信頼する\n      --allow <rule>             権限ルールに合う呼び出しを、この実行のあいだ「常に許可」と答えたように許す\n                                 （繰り返し指定できる。例: \"bash(npm test)\"。approval_requestのrulesと同じ形）\n      --slash                    -pと併用: /<command>で始まるプロンプトを、そのプラグインのコマンドとして実行する\n\nサブコマンド:\n  sasacode endpoint add <name> <url> [--key-env VAR] [--project]   サーバーを追加（形式は自動判別）\n  sasacode endpoint remove <name> [--project]\n  sasacode endpoint list\n  sasacode list models|commands|skills [--json]   ここでの実行が使えるもの（GUIは--jsonで尋ねる）\n  sasacode trust [--yes] [--json]   このフォルダを信頼しているか、信頼で何が有効になるか。--yesで信頼し、gitの実行も許す\n  sasacode plugin list --json       同梱プラグインと、それぞれが有効か（GUIが尋ねる）\n  sasacode login [status|logout]    API キーの代わりに ChatGPT プランを使う（openai-codex/… のモデル用）\n  sasacode update [--check] [tag]   最新リリースをダウンロードしてインストール（チェックサム検証つき）\n  sasacode web-search install       web_search ツール用の RustXNG を入れる（チェックサム検証つき）\n  sasacode gui [folder]             フォルダをデスクトップGUIで開く（sasacode gui installで入れる）\n  sasacode gui install [--path <dir>]   デスクトップGUIを、このマシンでRustを使ってビルドして入れる（docs/gui.md）\n  -h, --help\n  -v, --version",
    ),
    (
        "created {path}: add your API key there",
        "{path} を作成。ここに API キーを書く",
    ),
    ("warning: {warning}", "警告: {warning}"),
    (
        "{provider} has no model \"{spec}\". Did you mean: {similar}",
        "{provider} に \"{spec}\" というモデルはありません。近い名前: {similar}",
    ),
    (
        "{provider} has no model \"{spec}\" (the TUI's /model lists what it offers)",
        "{provider} に \"{spec}\" というモデルはありません（TUI の /model で一覧を見られます）",
    ),
    ("--output must be text or jsonl", "--output は text か jsonl"),
    ("--control must be stdio", "--control は stdio だけ"),
    (
        "--control stdio needs -p and --output jsonl",
        "--control stdio は -p と --output jsonl が必要",
    ),
    // ── --allow, --slash (for the GUI) ──
    ("--slash needs -p", "--slashは-pと併用する"),
    (
        "--allow \"{rule}\" is not a permission rule (like bash, bash(npm test) or edit(src/**))",
        "--allowの「{rule}」は権限ルールではない（bash、bash(npm test)、edit(src/**)のような形で書く）",
    ),
    ("unknown command: /{name}", "不明なコマンド: /{name}"),
    ("/{name} failed: {error}", "/{name}が失敗: {error}"),
    ("sasacode: {error}", "sasacode: {error}"),
    (
        "`sasacode plugin` was removed in 0.20: the plugins now ship with sasacode (turn the optional ones on with plugins.enabled in config.json); add tools with MCP servers or Skills",
        "`sasacode plugin` は 0.20 で廃止: プラグインは sasacode に同梱になった（任意のものは config.json の plugins.enabled で有効にする）。ツールを足すには MCP サーバーか Skills を使う",
    ),
    (
        "--max-turns needs a whole number (0 = no limit), got \"{value}\"",
        "--max-turns は整数で指定（0 で無制限）: \"{value}\"",
    ),
    (
        "unknown login subcommand \"{sub}\" (status, logout)",
        "不明な login サブコマンド \"{sub}\"（status, logout）",
    ),
    (
        "unknown web-search subcommand \"{sub}\" (install)",
        "不明な web-search サブコマンド \"{sub}\"（install）",
    ),
    ("usage: sasacode gui [folder]", "使い方: sasacode gui [folder]"),
    (
        "usage: sasacode gui install [--path <sasacode checkout>]",
        "使い方: sasacode gui install [--path <sasacodeのチェックアウト>]",
    ),
    (
        "{path} is not a sasacode checkout (no crates/sasacode-gui): give --path the repository",
        "{path}はsasacodeのチェックアウトではない（crates/sasacode-guiがない）。--pathにはリポジトリを渡す",
    ),
    (
        "sasacode gui install builds the desktop GUI on this machine with Rust, and cargo was not found (on PATH or in ~/.cargo/bin).\nInstall Rust from this page, open a new terminal, and run sasacode gui install again:\n  https://rustup.rs",
        "sasacode gui installは、デスクトップGUIをこのマシンでRustを使ってビルドする。cargoが見つからなかった（PATHにも~/.cargo/binにもない）。\n次のページからRustを入れ、新しいターミナルを開いてから、もう一度sasacode gui installを実行する。\n  https://rustup.rs",
    ),
    (
        "building sasacode-gui with {cargo} from {source} (a few minutes: cargo builds it from scratch every time)",
        "{cargo}で、{source}からsasacode-guiをビルドする（cargoは毎回はじめからビルドするので、数分かかる）",
    ),
    (
        "building sasacode-gui with {cargo} from {source} (a few minutes the first time)",
        "{cargo}で、{source}からsasacode-guiをビルドする（初回は数分かかる）",
    ),
    (
        "cargo could not build sasacode-gui (exit code {code}). If this sasacode is a development build whose tag {tag} is not released, build from a checkout: sasacode gui install --path <checkout>",
        "cargoがsasacode-guiをビルドできなかった（終了コード{code}）。このsasacodeが、タグ{tag}がまだない開発版なら、チェックアウトからビルドする: sasacode gui install --path <チェックアウト>",
    ),
    (
        "cargo could not build sasacode-gui (exit code {code})",
        "cargoがsasacode-guiをビルドできなかった（終了コード{code}）",
    ),
    (
        "cargo finished, but {path} does not run",
        "cargoは終わったが、{path}が動かない",
    ),
    (
        "installed sasacode-gui {version}: {path}\nopen it with sasacode gui [folder], or /gui in the TUI",
        "sasacode-gui {version}を入れた: {path}\nsasacode gui [フォルダ]か、TUIの/guiで開く",
    ),
    (
        "the desktop GUI is not updated with sasacode: sasacode gui install builds it for {to}",
        "デスクトップGUIは、sasacodeと一緒には更新されない。sasacode gui installで、{to}のGUIをビルドできる",
    ),
    (
        "usage: sasacode list models|commands|skills [--json]",
        "使い方: sasacode list models|commands|skills [--json]",
    ),
    (
        "usage: sasacode trust [--yes] [--json]",
        "使い方: sasacode trust [--yes] [--json]",
    ),
    ("up to {size}", "最大{size}"),
    ("TUI only", "TUIのみ"),
    (
        "(the bundled plugins' commands are listed by a run: the `commands` line of --control stdio)",
        "（同梱プラグインのコマンドは、実行が一覧を出す。--control stdioの`commands`行）",
    ),
    ("{path} is not a folder", "{path}はフォルダではない"),
    (
        "installed {version}: {path} (web_search uses it from the next request)",
        "{version} を入れました: {path}（web_search は次のリクエストから使います）",
    ),
    ("plugin {name}", "プラグイン {name}"),
    ("This project wants to use:", "このプロジェクトは次を使う:"),
    (
        "MCP servers run code as you; endpoints receive your conversation and code.",
        "MCP サーバーはあなたの権限でコードを実行し、エンドポイントは会話とコードを受け取る。",
    ),
    ("Trust {cwd}? [y/N] ", "{cwd} を信頼する？ [y/N] "),
    (
        "Run git in {cwd}? A repository's config can start programs. [y/N] ",
        "{cwd}でgitを実行してよい？リポジトリの設定はプログラムを起動しうる。[y/N] ",
    ),
    (
        "{cwd}: nothing in this project needs trust",
        "{cwd}: このプロジェクトには信頼が要るものがない",
    ),
    ("{cwd}: git may run here", "{cwd}: ここではgitを実行してよい"),
    ("{cwd} is trusted", "{cwd}は信頼済み"),
    ("{cwd} is not trusted", "{cwd}は信頼していない"),
    (
        "trust it with: sasacode trust --yes",
        "信頼するには: sasacode trust --yes",
    ),
    (
        "provider {name} has neither \"api\" nor \"baseUrl\"; ignored",
        "プロバイダー {name} は \"api\" も \"baseUrl\" もないため無視",
    ),
    (
        "provider {name} ({url}): {error}; ignored",
        "プロバイダー {name}（{url}）: {error}; 無視",
    ),
    (
        "name may contain letters, digits, _ . - only",
        "name は英数字と _ . - だけ",
    ),
    (
        "\"{name}\" is a built-in provider; choose another name",
        "\"{name}\" は組み込みプロバイダー。別の名前にする",
    ),
    (
        "warning: {keyEnv} is not set; detecting without a key",
        "警告: {keyEnv} が未設定。キーなしで判別",
    ),
    ("{name}: {api}{flavour} at {url}", "{name}: {api}{flavour}（{url}）"),
    ("({ctx} ctx)", "（コンテキスト {ctx}）"),
    ("(max {max})", "（最大 {max}）"),
    ("… {n} more (see /model)", "… 他 {n} 件（/model を参照）"),
    ("saved to {path}", "保存先: {path}"),
    ("try: sasacode -m {spec}", "試す: sasacode -m {spec}"),
    (
        "note: endpoints in a project config are used once you trust the project (sasacode asks on the next start)",
        "注: プロジェクト設定のエンドポイントは、プロジェクトを信頼してから使う（sasacode が次回起動時に確認）",
    ),
    ("{name} is not in {path}", "{name} は {path} にない"),
    ("removed {name} from {path}", "{name} を {path} から削除"),
    (
        "no custom endpoints (add one: sasacode endpoint add <name> <url>)",
        "カスタムエンドポイントなし（追加: sasacode endpoint add <name> <url>）",
    ),
    ("key: {env}", "キー: {env}"),
    (
        "usage: sasacode endpoint <add <name> <url> [--key-env VAR]|remove <name>|list> [--project]",
        "使い方: sasacode endpoint <add <name> <url> [--key-env VAR]|remove <name>|list> [--project]",
    ),
    (
        "plugin {path} skipped: {error}",
        "プラグイン {path} を飛ばした: {error}",
    ),
    (
        "needs plugin API {need}, host provides {have}",
        "プラグイン API {need} が必要（ホストは {have} を提供）",
    ),
    (
        "{file} is not a file inside {dir}",
        "{file} は {dir} の中のファイルではない",
    ),
    (
        "{file} is inside node_modules or .git, which the project's trust does not cover",
        "{file}はnode_modulesか.gitの中にあり、プロジェクトの信頼の対象外",
    ),
    ("\"{name}\" is not a package name", "「{name}」はパッケージ名ではない"),
    (
        "{file} has no default export function",
        "{file} に default export の関数がない",
    ),
    ("{cmd} failed", "{cmd} が失敗"),
    (
        "stopped: pass --yes to go ahead without a question (no terminal to ask on)",
        "中止: 質問なしで進めるには --yes（確認できる端末がない）",
    ),
    ("{question} [y/N] ", "{question} [y/N] "),
    ("Install it?", "入れる？"),
    (
        "usage: sasacode plugin <command> [--project]\n  search [words]                       find plugins (the list on sasanokusa.com and npm); no words lists all\n  install <npm-spec|git-url|dir> [--yes]  add one (shows what it is and asks first)\n  update [name] [--yes]                update npm plugins, or pull a git one (shows the changes and asks)\n  remove <name> [--yes]\n  list\n  publish <file.ts|dir> [--name <pkg>] [--version <v>] [--api <range>] [--license <id>] [--description <text>] [--otp <code>] [--dry-run]",
        "使い方: sasacode plugin <command> [--project]\n  search [words]                       プラグインを探す（sasanokusa.com の一覧と npm）。語を指定しなければ全件\n  install <npm-spec|git-url|dir> [--yes]  ひとつ追加（内容を示してから確認）\n  update [name] [--yes]                npm のプラグインを更新、git のものは pull（変更を示してから確認）\n  remove <name> [--yes]\n  list\n  publish <file.ts|dir> [--name <pkg>] [--version <v>] [--api <range>] [--license <id>] [--description <text>] [--otp <code>] [--dry-run]",
    ),
    ("error: {error}", "エラー: {error}"),
    ("warning: could not search {error}", "警告: 検索できなかった: {error}"),
    (
        "A plugin runs inside sasacode with your permissions: install only code you trust.",
        "プラグインはあなたの権限で sasacode 内部から動く。信頼できるコードだけ入れる",
    ),
    (
        "{path} already exists; remove it first or use plugin update",
        "{path} はすでに存在。先に消すか plugin update を使う",
    ),
    ("cloned into {path}", "{path} に clone"),
    (
        "{name} has no plugin.json or \"sasacode\" field in package.json, so it is not a sasacode plugin and was not installed",
        "{name} には plugin.json も package.json の \"sasacode\" もないため、sasacode のプラグインではない。インストールしなかった",
    ),
    (
        "installed {name} at commit {commit}",
        "{name} を commit {commit} に入れた",
    ),
    (
        "installed {name}@{version} into {dir}",
        "{name}@{version} を {dir} に入れた",
    ),
    (
        "{spec} is not an installed npm or git plugin here",
        "{spec} はここでは npm でも git でもインストール済みのプラグインではない",
    ),
    (
        "no npm or git plugins to update",
        "更新する npm か git のプラグインがない",
    ),
    (
        "{name}: {version} (already the newest)",
        "{name}: {version}（すでに最新）",
    ),
    ("{name}: {version} →", "{name}: {version} →"),
    ("Update {name}?", "{name} を更新する？"),
    ("{name}: {before} → {after}", "{name}: {before} → {after}"),
    (
        "{name}: already the newest ({commit})",
        "{name}: すでに最新（{commit}）",
    ),
    (
        "{name}: new commits\n{log}\n{stat}",
        "{name}: 新しいコミット\n{log}\n{stat}",
    ),
    ("{name}: now at {commit}", "{name}: いまは {commit}"),
    (
        "publishing uses npm (npm publish): install Node.js / npm first",
        "公開は npm 経由（npm publish）。先に Node.js / npm を入れる",
    ),
    (
        "{name}@{version}  (plugin API {api})\n  {dir}",
        "{name}@{version}（plugin API {api}）\n  {dir}",
    ),
    ("note: {note}", "注: {note}"),
    (
        "published: others can run  sasacode plugin install {name}",
        "公開した。他の人はこれで入れられる  sasacode plugin install {name}",
    ),
    (
        "{spec}: give the plugin's name as `sasacode plugin list` shows it, not a path",
        "{spec}: パスではなく `sasacode plugin list` に表示されるプラグイン名を指定する",
    ),
    ("Remove {name} from {dir}?", "{name} を {dir} から消す？"),
    ("removed {name}", "{name} を削除"),
    (
        "{spec}: no plugin by that name in {dir} (see sasacode plugin list)",
        "{spec}: その名前のプラグインは{dir}にない（sasacode plugin listで確認できる）",
    ),
    ("{name} is still installed at {path}", "{name}はまだ{path}に残っている"),
    ("{url} returned {status}", "{url} は {status} を返した"),
    ("list: {error}", "一覧: {error}"),
    ("npm: {error}", "npm: {error}"),
    ("no plugins found", "プラグインが見つからない"),
    ("by {author}", "作者: {author}"),
    (
        "{name}@{version}  (local: {dir})",
        "{name}@{version}（ローカル: {dir}）",
    ),
    (
        "{name} is not a sasacode plugin (no \"sasacode\" field in its package.json), so it was not installed.",
        "{name} は sasacode のプラグインではない（package.json に \"sasacode\" がない）ため、インストールしなかった。",
    ),
    (
        "Did you mean {alt}?  sasacode plugin install {alt}",
        "{alt} のことですか？  sasacode plugin install {alt}",
    ),
    (
        "{name}: not a sasacode plugin, skipped (sasacode plugin remove {name})",
        "{name}: sasacode のプラグインではないので飛ばした（sasacode plugin remove {name}）",
    ),
    (
        "{name}  [{scope}]  not a sasacode plugin, never loaded: sasacode plugin remove {name}{flag}",
        "{name}  [{scope}]  sasacode のプラグインではなく、読み込まれていない: sasacode plugin remove {name}{flag}",
    ),
    (
        "(range {range}: the newest match is installed)",
        "（{range} のうち最新をインストール）",
    ),
    (
        "no version of {name} matches {range}",
        "{name}には{range}に合う版がない",
    ),
    ("published by {people}", "公開者: {people}"),
    ("on {date}", "（{date}）"),
    ("{count} files, {size}", "{count} ファイル、{size}"),
    ("size unknown", "サイズ不明"),
    ("depends on {deps}", "依存: {deps}"),
    (
        "has install scripts (not run: packages are added without them)",
        "install scripts あり（実行されない: それなしで追加する）",
    ),
    ("source {repo}", "ソース: {repo}"),
    (
        "{file}: a plugin file ends in .ts, .js or .mjs",
        "{file}: プラグインファイルは .ts, .js, .mjs で終わる",
    ),
    (
        "\"{name}\" is not a valid npm package name; pass --name",
        "\"{name}\" は npm パッケージ名として不正。--name を指定",
    ),
    (
        "it imports other local files, which are not included: publish its directory instead",
        "他のローカルファイルを import しているが同梱されない。ディレクトリごと公開する",
    ),
    (
        "no license given: others may not reuse it (add one with --license MIT, for example)",
        "license 未指定。他人は再利用できない（--license MIT などで足す）",
    ),
    (
        "plugin API {api} does not match this sasacode ({version})",
        "plugin API {api} はこの sasacode（{version}）と合わない",
    ),
    (
        "{dir} has no package.json; publish a single .ts file instead, or add one",
        "{dir} に package.json がない。1つの .ts を公開するか、足す",
    ),
    (
        "{path} has no \"sasacode\" field (the manifest: extensions, apiVersion, skills)",
        "{path} に \"sasacode\" がない（マニフェスト: extensions, apiVersion, skills）",
    ),
    (
        "added the \"{keyword}\" keyword to package.json so that plugin search finds it",
        "検索に引っかかるよう package.json に \"{keyword}\" キーワードを追加",
    ),
    ("↻ repaired {name}: {note}", "↻ {name} を修復: {note}"),
    (
        "plugin {plugin} ({hook}): {error}",
        "プラグイン {plugin}（{hook}）: {error}",
    ),
    ("[stopped: {cause}{hint}]", "[停止: {cause}{hint}]"),
    (
        "session {id} · continue: sasacode -r {id} -p \"…\"",
        "session {id} · 続ける: sasacode -r {id} -p \"…\"",
    ),
    (
        "turn limit reached (--max-turns or maxTurns in config; 0 = no limit)",
        "ターン数の上限（--max-turns か config の maxTurns。0 で無制限）",
    ),
    ("context window is full", "コンテキストウィンドウが満杯"),
    (
        "session model {model} unavailable: {error}",
        "セッションのモデル {model} は使えない: {error}",
    ),
    (
        "no previous session in this directory",
        "このディレクトリに過去のセッションがない",
    ),
    (
        "session {id} belongs to {cwd}: run sasacode from there",
        "セッション {id} は {cwd} のもの。そこで sasacode を実行する",
    ),
    ("session not found: {id}", "セッションがない: {id}"),
    (
        "project plugins not loaded until you trust this project: {names}",
        "このプロジェクトを信頼するまでプロジェクトのプラグインは読まない: {names}",
    ),
    (
        "plugin {name}: skills folder {folder} is outside the plugin; ignored",
        "プラグイン {name}: skills フォルダ {folder} がプラグインの外。無視",
    ),
    (
        "plugin {name} skipped: {error}",
        "プラグイン {name} を飛ばした: {error}",
    ),
    (
        "{source}: not a JSON object; ignored",
        "{source}: JSON オブジェクトではない。無視",
    ),
    (
        "{source}: unknown key \"{key}\" ignored",
        "{source}: 不明なキー \"{key}\" は無視",
    ),
    (
        "{source}: \"{key}\" has an invalid value; ignored",
        "{source}: \"{key}\" の値が不正。無視",
    ),
    (
        ".sasacode/config.json: trustedProjects is only read from the global config; ignored",
        ".sasacode/config.json: trustedProjects はグローバル設定からのみ読む。無視",
    ),
    (
        ".sasacode/config.json: this project selects the model {model}",
        ".sasacode/config.json: このプロジェクトがモデル {model} を選ぶ",
    ),
    (
        ".sasacode/config.json: this project selects the judge model {model}",
        ".sasacode/config.json: このプロジェクトが判定モデル {model} を選ぶ",
    ),
    (
        ".sasacode/config.json: not applied until you trust this project ({items})",
        ".sasacode/config.json: このプロジェクトを信頼するまで適用しない（{items}）",
    ),
    (
        "sasacode API keys. Fill in the providers you use; blank entries are ignored.",
        "sasacode の API キー。使うプロバイダーだけ埋める。空欄は無視する。",
    ),
    (
        "Read on every start, from any directory. A project's ./.env is not read.",
        "どのディレクトリからでも起動のたびに読む。プロジェクトの ./.env は読まない。",
    ),
    (
        "Commands the agent runs (bash, background jobs) do not see what is set here.",
        "エージェントが実行するコマンド（bash・バックグラウンドジョブ）には渡らない。",
    ),
    ("(e.g. -m {example})", "（例 -m {example}）"),
    (
        "Ollama (ollama/<model>) runs locally and needs no key.",
        "Ollama（ollama/<model>）はローカルで動き、キーが要らない。",
    ),
    (
        "cannot tell the latest version ({status} from {base})",
        "最新版がわからない（{base} から {status}）",
    ),
    (
        "a newer version is available: {latest} (this is {version}) — run `{how}`",
        "新しい版がある: {latest}（いまは {version}）— `{how}` を実行",
    ),
    ("unsupported OS: {platform}", "未対応の OS: {platform}"),
    ("unsupported CPU: {arch}", "未対応の CPU: {arch}"),
    (
        "this sasacode runs from source: update the checkout (git pull && cargo build --release) instead of `sasacode update`",
        "この sasacode はソースから動いている。`sasacode update` の代わりにチェックアウトを更新（git pull && cargo build --release）",
    ),
    (
        "newer version available: {wanted} (this is {version})",
        "新しい版がある: {wanted}（いまは {version}）",
    ),
    ("sasacode is up to date ({version})", "sasacode は最新（{version}）"),
    ("downloading {url} …", "{url} をダウンロード中…"),
    ("{file} is not listed in SHA256SUMS", "{file} は SHA256SUMS にない"),
    ("checksum mismatch for {file}", "{file} のチェックサムが合わない"),
    ("could not unpack {file}", "{file} を展開できなかった"),
    (
        "the downloaded binary does not run on this system",
        "ダウンロードしたバイナリがこのシステムで動かない",
    ),
    (
        "updated {from} → {to}: {target} (restart sasacode to use it)",
        "{from} → {to} に更新: {target}（使うには sasacode を再起動）",
    ),
    ("endpoint {name} ({url})", "エンドポイント {name}（{url}）"),
    ("model {model}", "モデル {model}"),
    ("judge model {model}", "判定モデル {model}"),
    ("models {models}", "モデル一覧 {models}"),
    ("model override {model}: {value}", "モデル設定の上書き {model}: {value}"),
    ("MCP server {name}: {run}", "MCP サーバー {name}: {run}"),
    ("in {dir}", "作業フォルダ {dir}"),
    ("environment {names}", "環境変数 {names}"),
    ("headers {names}", "ヘッダー {names}"),
    ("no command or url", "commandもurlもない"),
    (
        "command {value} (not a string, so nothing is started)",
        "commandが文字列でない {value}（何も起動しない）",
    ),
    (
        "applied on top of the global server of the same name",
        "同名のグローバルのサーバーに重ねる",
    ),
    ("disables plugins {names}", "プラグインの無効化 {names}"),
    ("enables plugins {names}", "プラグインの有効化 {names}"),
    (
        "settings for plugin {name}: {value}",
        "プラグイン {name} の設定: {value}",
    ),
    ("allow rules {rules}", "許可ルール {rules}"),
    ("permission mode {mode}", "権限モード {mode}"),
    ("maxTurns unlimited", "maxTurns 無制限"),
    (
        "unknown permission mode \"{mode}\" ({modes})",
        "不明な権限モード \"{mode}\"（{modes}）",
    ),
    (
        "unknown thinking level \"{level}\" ({levels})",
        "不明な思考レベル \"{level}\"（{levels}）",
    ),
    ("failed to read {path}: {error}", "{path} を読めなかった: {error}"),
    (
        "warning: nothing arrived on stdin within {s}s, so it was not read (pass --stdin to wait for it)",
        "警告: {s} 秒待っても stdin に何も届かなかったので読まなかった（待つには --stdin）",
    ),
    (
        "unknown permission preset \"{name}\" ({known})",
        "不明な権限プリセット \"{name}\"（{known}）",
    ),
    (
        "browsr: unknown mode \"{mode}\" ({known})",
        "browsr: 不明なモード \"{mode}\"（{known}）",
    ),
    (
        "jev-guard is no longer bundled, so the Jev check your config turns on is not running: sasacode plugin install sasacode-plugin-jev-guard",
        "jev-guard は同梱ではなくなったため、config で有効にしている Jev の判定は動いていない: sasacode plugin install sasacode-plugin-jev-guard",
    ),
    ("could not pack {dir}", "{dir} をまとめられなかった"),
    (
        "the connection dropped mid-reply; asking again",
        "接続が応答の途中で切れたため、やり直す",
    ),
    (
        "this sasacode was installed with npm: update it with `npm install -g sasacode@latest`",
        "この sasacode は npm で入れたもの。更新は `npm install -g sasacode@latest`",
    ),
    ("gh is not installed", "ghが入っていない"),
    ("gh is not signed in", "ghにログインしていない"),
    ("gh did not finish", "ghが終わらなかった"),
    ("not the official releases", "公式のリリースではない"),
    (
        "build provenance was not checked ({why}). By hand: gh attestation verify {file} --repo {repo}",
        "ビルドの来歴は確かめていない（{why}）。手で確かめるには: gh attestation verify {file} --repo {repo}",
    ),
    ("build provenance verified", "ビルドの来歴を確かめた"),
    (
        "build provenance check failed for {file}; nothing was installed:\n{said}",
        "{file}のビルドの来歴を確かめられなかったので、何も入れていない:\n{said}",
    ),
    (
        "this sasacode was built from \"{rev}\", which is not a commit hash: build from a checkout with sasacode gui install --path <checkout>",
        "このsasacodeは\"{rev}\"からビルドされていて、これはコミットのハッシュではない。チェックアウトから、sasacode gui install --path <checkout>でビルドする",
    ),
    (
        "usage: sasacode web-search install",
        "使い方: sasacode web-search install",
    ),
    (
        "{base} is not an https URL (http is allowed only for localhost)",
        "{base}はhttpsのURLではない（httpはlocalhostにだけ使える）",
    ),
];
