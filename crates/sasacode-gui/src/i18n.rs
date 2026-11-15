//! The window's words, in Japanese or English (`SASACODE_LANG`, else the system's `LANG`).
//!
//! The Japanese follows yomiyasu (https://github.com/nanaism/yomiyasu): plain sentences that say
//! who does what, no decoration, no dashes, and the same word for the same thing everywhere.

use std::time::{Duration, SystemTime};

use sasacode_protocol::StopCause;

use crate::transcript::Activity;

pub struct Strings {
    pub ja: bool,
    pub new_session: &'static str,
    pub find: &'static str,
    pub within_day: &'static str,
    pub within_week: &'static str,
    pub older: &'static str,
    pub no_sessions: &'static str,
    pub untitled: &'static str,
    pub placeholder: &'static str,
    pub placeholder_running: &'static str,
    pub newline: &'static str,
    pub send: &'static str,
    pub stop: &'static str,
    pub allow: &'static str,
    pub always: &'static str,
    pub deny: &'static str,
    pub feedback: &'static str,
    pub denied: &'static str,
    pub cancelled: &'static str,
    pub awaiting: &'static str,
    pub title_awaiting: &'static str,
    pub thinking: &'static str,
    pub failed: &'static str,
    pub queued: &'static str,
    pub returned: &'static str,
    pub plan: &'static str,
    pub copy: &'static str,
    pub copied: &'static str,
    pub model: &'static str,
    pub permission_label: &'static str,
    pub effort_label: &'static str,
    pub model_default: &'static str,
    pub model_default_note: &'static str,
    pub effort_default: &'static str,
    pub effort_default_note: &'static str,
    pub context_full: &'static str,
    pub compacted: &'static str,
    pub retrying: &'static str,
    pub not_found: &'static str,
    pub exited: &'static str,
    /// What the first screen opens with: one of these, another each time.
    pub empty_leads: &'static [&'static str],
    pub more_lines: &'static str,
    pub usage_in: &'static str,
    pub usage_out: &'static str,
    pub usage_cache: &'static str,
    pub changes: &'static str,
    pub changes_empty: &'static str,
    pub show_changes: &'static str,
    pub hide_changes: &'static str,
    pub show_sidebar: &'static str,
    pub hide_sidebar: &'static str,
    pub find_root: &'static str,
    pub find_model: &'static str,
    pub find_permission: &'static str,
    pub find_effort: &'static str,
    pub group_actions: &'static str,
    pub group_sessions: &'static str,
    pub do_new: &'static str,
    pub do_model: &'static str,
    pub do_permission: &'static str,
    pub do_effort: &'static str,
    pub do_stop: &'static str,
    pub do_copy: &'static str,
    pub find_keys: &'static str,
    pub no_match: &'static str,
}

impl Strings {
    pub fn detect() -> Self {
        let lang = std::env::var("SASACODE_LANG")
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default();
        Self::get(&lang)
    }

    pub fn get(lang: &str) -> Self {
        if lang.starts_with("ja") { JA } else { EN }
    }

    /// A shortcut as the keyboard in front of the person names it: `key("K")` is ⌘K on a Mac.
    pub fn key(&self, key: &str) -> String {
        if cfg!(target_os = "macos") {
            format!("⌘{key}")
        } else {
            format!("Ctrl+{key}")
        }
    }

    pub fn stopped(&self, cause: StopCause) -> &'static str {
        match (cause, self.ja) {
            (StopCause::Aborted, true) => "中断しました",
            (StopCause::Aborted, false) => "Stopped",
            (StopCause::MaxTurns, true) => "ターン数の上限に達したので止まりました",
            (StopCause::MaxTurns, false) => "Stopped at the turn limit",
            (StopCause::ContextLimit, true) => "コンテキストがいっぱいになったので止まりました",
            (StopCause::ContextLimit, false) => "Stopped because the context window is full",
            (StopCause::Refusal, true) => "モデルが応答を拒否しました",
            (StopCause::Refusal, false) => "The model refused to answer",
            (StopCause::Error, true) => "エラーで止まりました",
            (StopCause::Error, false) => "Stopped by an error",
            (_, true) => "止まりました",
            (_, false) => "Stopped",
        }
    }

    /// What a tool does, as a word for the row of a step. Tools the window does not know keep their
    /// own name.
    pub fn tool<'a>(&self, name: &'a str) -> &'a str {
        let (ja, en) = match name {
            "bash" => ("実行", "Run"),
            "read" => ("読み取り", "Read"),
            "edit" => ("編集", "Edit"),
            "write" => ("書き込み", "Write"),
            "web_fetch" => ("ページの取得", "Fetch"),
            "web_search" => ("ウェブ検索", "Search"),
            "task" => ("サブエージェント", "Subagent"),
            _ => return name,
        };
        if self.ja { ja } else { en }
    }

    pub fn approval_title(&self, tool: &str) -> String {
        match (tool, self.ja) {
            ("bash", true) => "このコマンドを実行してよいですか".into(),
            ("bash", false) => "Run this command?".into(),
            ("edit", true) => "このファイルを編集してよいですか".into(),
            ("edit", false) => "Edit this file?".into(),
            ("write", true) => "このファイルに書き込んでよいですか".into(),
            ("write", false) => "Write this file?".into(),
            (_, true) => format!("{tool} を実行してよいですか"),
            (_, false) => format!("Run {tool}?"),
        }
    }

    /// Why sasacode asks, from the reason it gives (`packages/agent/src/permission.ts`), as a
    /// sentence. A reason the window does not know (a judging model's own words) is shown as it is.
    pub fn approval_reason(&self, reason: &str) -> String {
        let ja = self.ja;
        if reason == "mode ask" {
            return if ja {
                "権限が「毎回確認」なので確認しています".into()
            } else {
                "Asked because the permission is \"Ask every time\"".into()
            };
        }
        if reason == "mode edits" {
            return if ja {
                "いまの権限では、コマンドの実行とフォルダの外への操作を確認します".into()
            } else {
                "This permission asks before commands and anything outside the folder".into()
            };
        }
        if reason == "mode agent: not judged" {
            return if ja {
                "危険度を判定できなかったので確認しています".into()
            } else {
                "Asked because the risk could not be judged".into()
            };
        }
        if let Some(rule) = reason.strip_prefix("rule ask: ") {
            return if ja {
                format!("設定のルール（{rule}）に従って確認しています")
            } else {
                format!("Asked by a rule in the settings ({rule})")
            };
        }
        if let Some(what) = reason.strip_prefix("cannot resolve ") {
            return if ja {
                format!("{what} の場所を確かめられないので確認しています")
            } else {
                format!("Asked because {what} could not be resolved")
            };
        }
        if ja {
            format!("確認の理由: {reason}")
        } else {
            format!("Asked because: {reason}")
        }
    }

    /// A tool call whose arguments were mended before it ran.
    pub fn repaired(&self, tool: &str, note: &str) -> String {
        if self.ja {
            format!("{tool} の呼び出しを直しました（{note}）")
        } else {
            format!("Mended the call to {tool}: {note}")
        }
    }

    pub fn approval_more(&self, n: usize) -> String {
        if self.ja {
            format!("ほかに{n}件の確認が待っています")
        } else if n == 1 {
            "1 more is waiting".into()
        } else {
            format!("{n} more are waiting")
        }
    }

    pub fn activity(&self, activity: &Activity, awaiting_approval: bool) -> String {
        if awaiting_approval {
            return self.awaiting.into();
        }
        match (activity, self.ja) {
            (Activity::Waiting, true) => "応答を待っています".into(),
            (Activity::Waiting, false) => "Waiting for the model".into(),
            (Activity::Thinking, true) => "考えています".into(),
            (Activity::Thinking, false) => "Thinking".into(),
            (Activity::Writing, true) => "応答を書いています".into(),
            (Activity::Writing, false) => "Writing".into(),
            (Activity::Tool(name), ja) => {
                let (j, e) = match name.as_str() {
                    "bash" => ("コマンドを実行しています", "Running a command"),
                    "read" => ("ファイルを読んでいます", "Reading a file"),
                    "edit" => ("ファイルを編集しています", "Editing a file"),
                    "write" => ("ファイルに書き込んでいます", "Writing a file"),
                    "web_fetch" => ("ページを取得しています", "Fetching a page"),
                    "web_search" => ("ウェブを検索しています", "Searching the web"),
                    "todo_write" => ("作業リストを更新しています", "Updating the task list"),
                    _ if ja => return format!("{name} を実行しています"),
                    _ => return format!("Running {name}"),
                };
                (if ja { j } else { e }).into()
            }
        }
    }

    pub fn permission(&self, mode: &str) -> String {
        let (ja, en) = match mode {
            "edits" => ("読み取りと編集を許可", "Allow reads and edits"),
            "ask" => ("毎回確認", "Ask every time"),
            "agent" => ("エージェントが判断", "Let the agent judge"),
            "auto" => ("すべて許可", "Allow everything"),
            _ => return mode.into(),
        };
        (if self.ja { ja } else { en }).into()
    }

    /// What a permission mode lets sasacode do without asking, as `packages/agent/src/permission.ts`
    /// decides it.
    pub fn permission_note(&self, mode: &str) -> &'static str {
        match (mode, self.ja) {
            ("edits", true) => {
                "作業フォルダの中の読み取りと編集は確認なしで進めます。コマンドの実行と、フォルダの外への操作は確認します。"
            }
            ("edits", false) => {
                "Reads and edits inside the folder go ahead. Commands, and anything outside the folder, are asked first."
            }
            ("ask", true) => "ファイルの読み取りを含め、すべての操作を確認します。",
            ("ask", false) => "Everything is asked first, reading a file included.",
            ("agent", true) => {
                "作業フォルダの中の読み取りは確認なしで進めます。ほかの操作はモデルが危険度を判定し、安全と判定できたものだけ確認なしで進めます。"
            }
            ("agent", false) => {
                "Reads inside the folder go ahead. A model judges the rest, and only what it finds safe goes ahead unasked."
            }
            ("auto", true) => "すべての操作を確認なしで進めます。",
            ("auto", false) => "Everything goes ahead without asking.",
            _ => "",
        }
    }

    /// A level of reasoning where what it is a level of is already said beside it.
    pub fn effort_level(&self, level: &str) -> String {
        let ja = match level {
            "off" => "なし",
            "low" => "低",
            "medium" => "中",
            "high" => "高",
            "xhigh" => "特に高い",
            "max" => "最大",
            _ => return level.into(),
        };
        if self.ja { ja.into() } else { level.into() }
    }

    /// The same where it stands alone.
    pub fn effort(&self, level: &str) -> String {
        match (level, self.ja) {
            ("off", true) => "推論なし".into(),
            (_, true) => format!("推論 {}", self.effort_level(level)),
            (_, false) => format!("Effort {level}"),
        }
    }

    /// Under the first screen's opening line, which is not there to be useful: what to do here.
    pub fn empty_note(&self, folder: &str) -> String {
        if self.ja {
            format!(
                "下の欄に頼みたいことを書くと、sasacodeが {folder} のファイルを読み、編集し、コマンドを実行して進めます。"
            )
        } else {
            format!(
                "Write what you want done below. sasacode reads and edits the files in {folder} and runs commands there to do it."
            )
        }
    }

    pub fn earlier(&self, n: usize) -> String {
        if self.ja {
            format!("以前の依頼 {n}件")
        } else if n == 1 {
            "1 earlier request".into()
        } else {
            format!("{n} earlier requests")
        }
    }

    pub fn steps(&self, n: usize) -> String {
        if self.ja {
            format!("作業 {n}件")
        } else if n == 1 {
            "1 step".into()
        } else {
            format!("{n} steps")
        }
    }

    /// The files a turn changed, as the line under its answer.
    pub fn changed(&self, files: &[&str]) -> String {
        match (files, self.ja) {
            ([], _) => String::new(),
            ([one], true) => format!("{one} を変更"),
            ([one], false) => format!("Changed {one}"),
            ([first, rest @ ..], true) => format!("{first} ほか{}個のファイルを変更", rest.len()),
            ([first, rest @ ..], false) => format!("Changed {first} and {} more", rest.len()),
        }
    }

    pub fn use_model(&self, name: &str) -> String {
        if self.ja {
            format!("「{name}」を使う")
        } else {
            format!("Use \"{name}\"")
        }
    }

    pub fn duration(&self, seconds: u64) -> String {
        let (m, s) = (seconds / 60, seconds % 60);
        match (m, self.ja) {
            (0, true) => format!("{s}秒"),
            (0, false) => format!("{s}s"),
            (_, true) => format!("{m}分{s}秒"),
            (_, false) => format!("{m}m {s}s"),
        }
    }

    /// How long ago a conversation was last written, in the largest unit that fits.
    pub fn ago(&self, then: SystemTime, now: SystemTime) -> String {
        let secs = now.duration_since(then).unwrap_or(Duration::ZERO).as_secs();
        let (n, ja, en) = match secs {
            0..60 => return (if self.ja { "たった今" } else { "just now" }).into(),
            60..3600 => (secs / 60, "分前", "min ago"),
            3600..86_400 => (secs / 3600, "時間前", "h ago"),
            _ => (secs / 86_400, "日前", "d ago"),
        };
        if self.ja {
            format!("{n}{ja}")
        } else {
            format!("{n} {en}")
        }
    }

    pub fn list_separator(&self) -> &'static str {
        if self.ja { "、" } else { ", " }
    }
}

const JA: Strings = Strings {
    ja: true,
    new_session: "新しい会話",
    find: "探す、操作する",
    within_day: "24時間以内",
    within_week: "7日以内",
    older: "それより前",
    no_sessions: "このフォルダにはまだ会話がありません",
    untitled: "無題の会話",
    placeholder: "頼みたいことを書く",
    placeholder_running: "追加の指示を書く",
    newline: "改行",
    send: "送信",
    stop: "中断",
    allow: "許可",
    always: "常に許可",
    deny: "拒否",
    feedback: "拒否する理由（任意）",
    denied: "拒否しました",
    cancelled: "答える前に実行が終わりました",
    awaiting: "承認を待っています",
    title_awaiting: "承認待ち",
    thinking: "Thinking",
    failed: "失敗",
    queued: "次のターンで届きます",
    returned: "届かなかった指示を入力欄に戻しました",
    plan: "作業リスト",
    copy: "コピー",
    copied: "コピーしました",
    model: "モデル",
    permission_label: "権限",
    effort_label: "推論",
    model_default: "デフォルトのモデル",
    model_default_note: "sasacodeの設定に従います",
    effort_default: "デフォルト",
    effort_default_note: "sasacodeの設定に従います。設定がなければ「高」です",
    context_full: "コンテキストの上限に近づいています",
    compacted: "会話を圧縮しました",
    retrying: "接続が切れたため、もう一度問い合わせています",
    not_found: "sasacode を起動できませんでした。PATHに入れるか、SASACODE_BIN か --sasacode で場所を指定してください",
    exited: "sasacode が終了しました（終了コード {code}）",
    empty_leads: &[
        "ささっと片付けましょう",
        "「あとで直す」の「あと」が来ました",
        "七夕でなくても、願いごとは受け付けています",
        "今日はどのバグを笹舟で流しますか",
        "パンダが来る前に、書いてしまいましょう",
        "TODOコメントの供養、承ります",
        "竹を割ったようなコードにします。笹ですが",
        "笹の葉さらさら、コードもさらさら",
    ],
    more_lines: "ほか {n} 行",
    usage_in: "入力",
    usage_out: "出力",
    usage_cache: "キャッシュ",
    changes: "変更したファイル",
    changes_empty: "この会話ではまだファイルを変更していません",
    show_changes: "変更したファイルを表示する",
    hide_changes: "変更したファイルを隠す",
    show_sidebar: "会話の一覧を表示する",
    hide_sidebar: "会話の一覧を隠す",
    find_root: "会話や操作を探す",
    find_model: "モデルを探す、または名前を入力する",
    find_permission: "権限を選ぶ",
    find_effort: "推論の強さを選ぶ",
    group_actions: "操作",
    group_sessions: "会話",
    do_new: "新しい会話を始める",
    do_model: "モデルを選ぶ",
    do_permission: "権限を選ぶ",
    do_effort: "推論の強さを選ぶ",
    do_stop: "実行を中断する",
    do_copy: "最後の答えをコピーする",
    find_keys: "↑と↓で選択、Enterで決定、Escで戻る",
    no_match: "一致するものがありません",
};

const EN: Strings = Strings {
    ja: false,
    new_session: "New conversation",
    find: "Find or run",
    within_day: "Last 24 hours",
    within_week: "Last 7 days",
    older: "Earlier",
    no_sessions: "No conversations in this folder yet",
    untitled: "Untitled conversation",
    placeholder: "Write what you want done",
    placeholder_running: "Add an instruction",
    newline: "New line",
    send: "Send",
    stop: "Stop",
    allow: "Allow",
    always: "Always allow",
    deny: "Deny",
    feedback: "Why not (optional)",
    denied: "Denied",
    cancelled: "The run ended before an answer",
    awaiting: "Waiting for your approval",
    title_awaiting: "Needs approval",
    thinking: "Thinking",
    failed: "Failed",
    queued: "Reaches the model at its next turn",
    returned: "An instruction did not reach the model and is back in the input",
    plan: "Task list",
    copy: "Copy",
    copied: "Copied",
    model: "Model",
    permission_label: "Permission",
    effort_label: "Effort",
    model_default: "Default model",
    model_default_note: "As sasacode is configured",
    effort_default: "Default",
    effort_default_note: "As sasacode is configured, or high when nothing is set",
    context_full: "The context window is nearly full",
    compacted: "The conversation was compacted",
    retrying: "The connection dropped; asking again",
    not_found: "Could not start sasacode. Put it on PATH, or give its location with SASACODE_BIN or --sasacode",
    exited: "sasacode exited (code {code})",
    empty_leads: &[
        "The \"later\" in \"fix it later\" is now",
        "What are we untangling today?",
        "Make a wish. It does not have to be Tanabata",
        "Which bug goes down the river today?",
        "Let's finish before the pandas show up",
        "TODO comments laid to rest here",
        "Clean as split bamboo. Well, bamboo grass",
        "It works on your machine. Let's do the others",
    ],
    more_lines: "{n} more lines",
    usage_in: "in",
    usage_out: "out",
    usage_cache: "cache",
    changes: "Changed files",
    changes_empty: "This conversation has not changed a file yet",
    show_changes: "Show changed files",
    hide_changes: "Hide changed files",
    show_sidebar: "Show the conversations",
    hide_sidebar: "Hide the conversations",
    find_root: "Find a conversation or an action",
    find_model: "Find a model, or type its name",
    find_permission: "Choose a permission",
    find_effort: "Choose an effort",
    group_actions: "Actions",
    group_sessions: "Conversations",
    do_new: "Start a new conversation",
    do_model: "Choose a model",
    do_permission: "Choose a permission",
    do_effort: "Choose an effort",
    do_stop: "Stop the run",
    do_copy: "Copy the last answer",
    find_keys: "Up and Down to move, Enter to choose, Esc to go back",
    no_match: "Nothing matches",
};
