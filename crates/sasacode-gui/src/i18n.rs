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
    pub search: &'static str,
    pub no_sessions: &'static str,
    pub no_match: &'static str,
    pub untitled: &'static str,
    pub placeholder: &'static str,
    pub placeholder_running: &'static str,
    pub keys: &'static str,
    pub send: &'static str,
    pub stop: &'static str,
    pub allow: &'static str,
    pub always: &'static str,
    pub deny: &'static str,
    pub feedback: &'static str,
    pub approval_keys: &'static str,
    pub approval_why: &'static str,
    pub allowed: &'static str,
    pub always_allowed: &'static str,
    pub denied: &'static str,
    pub cancelled: &'static str,
    pub awaiting: &'static str,
    pub thinking: &'static str,
    pub work: &'static str,
    pub changed: &'static str,
    pub queued: &'static str,
    pub returned: &'static str,
    pub plan: &'static str,
    pub copy: &'static str,
    pub copied: &'static str,
    pub model_default: &'static str,
    pub model_other: &'static str,
    pub model_placeholder: &'static str,
    pub model_set: &'static str,
    pub effort_default: &'static str,
    pub context_full: &'static str,
    pub compacted: &'static str,
    pub retrying: &'static str,
    pub not_found: &'static str,
    pub exited: &'static str,
    pub empty_lead: &'static str,
    pub empty_note: &'static str,
    pub more_lines: &'static str,
    pub usage_in: &'static str,
    pub usage_out: &'static str,
    pub usage_cache: &'static str,
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

    pub fn effort(&self, level: &str) -> String {
        let ja = match level {
            "off" => "推論なし",
            "low" => "推論 低",
            "medium" => "推論 中",
            "high" => "推論 高",
            "xhigh" => "推論 特に高い",
            "max" => "推論 最大",
            _ => return level.into(),
        };
        if self.ja { ja.into() } else { format!("Effort {level}") }
    }

    pub fn steps(&self, n: usize) -> String {
        if self.ja {
            format!("{} {n}件", self.work)
        } else {
            format!("{} · {n}", self.work)
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
    search: "会話を探す",
    no_sessions: "このフォルダにはまだ会話がありません",
    no_match: "一致する会話がありません",
    untitled: "無題の会話",
    placeholder: "頼みたいことを書く",
    placeholder_running: "追加の指示を書く",
    keys: "Enterで送信、Shift+Enterで改行",
    send: "送信",
    stop: "中断",
    allow: "許可",
    always: "常に許可",
    deny: "拒否",
    feedback: "拒否する理由（任意）",
    approval_keys: "Ctrl+Enterでも許可できます",
    approval_why: "確認の理由",
    allowed: "許可しました",
    always_allowed: "常に許可しました",
    denied: "拒否しました",
    cancelled: "取り消されました",
    awaiting: "承認を待っています",
    thinking: "思考",
    work: "作業の記録",
    changed: "変更したファイル",
    queued: "次のターンで届きます",
    returned: "届かなかった指示を入力欄に戻しました",
    plan: "作業リスト",
    copy: "コピー",
    copied: "コピーしました",
    model_default: "既定のモデル",
    model_other: "モデル名を入力する",
    model_placeholder: "provider/model の形で入力してEnter",
    model_set: "決定",
    effort_default: "推論は既定",
    context_full: "コンテキストの上限に近づいています",
    compacted: "会話を圧縮しました",
    retrying: "接続が切れたため、もう一度問い合わせています",
    not_found: "sasacode を起動できませんでした。PATHに入れるか、SASACODE_BIN か --sasacode で場所を指定してください",
    exited: "sasacode が終了しました（終了コード {code}）",
    empty_lead: "このフォルダで頼みたいことを下の欄に書いてください",
    empty_note: "sasacodeはファイルを編集したりコマンドを実行したりする前に、権限の設定に従って確認を求めます",
    more_lines: "ほか {n} 行",
    usage_in: "入力",
    usage_out: "出力",
    usage_cache: "キャッシュ",
};

const EN: Strings = Strings {
    ja: false,
    new_session: "New conversation",
    search: "Find a conversation",
    no_sessions: "No conversations in this folder yet",
    no_match: "No conversation matches",
    untitled: "Untitled conversation",
    placeholder: "Write what you want done",
    placeholder_running: "Add an instruction",
    keys: "Enter to send, Shift+Enter for a new line",
    send: "Send",
    stop: "Stop",
    allow: "Allow",
    always: "Always allow",
    deny: "Deny",
    feedback: "Why not (optional)",
    approval_keys: "Ctrl+Enter also allows",
    approval_why: "Asked because",
    allowed: "Allowed",
    always_allowed: "Always allowed",
    denied: "Denied",
    cancelled: "Cancelled",
    awaiting: "Waiting for your approval",
    thinking: "Thinking",
    work: "Work log",
    changed: "Changed files",
    queued: "Reaches the model at its next turn",
    returned: "An instruction did not reach the model and is back in the input",
    plan: "Task list",
    copy: "Copy",
    copied: "Copied",
    model_default: "Default model",
    model_other: "Type a model name",
    model_placeholder: "provider/model, then Enter",
    model_set: "Set",
    effort_default: "Default effort",
    context_full: "The context window is nearly full",
    compacted: "The conversation was compacted",
    retrying: "The connection dropped; asking again",
    not_found: "Could not start sasacode. Put it on PATH, or give its location with SASACODE_BIN or --sasacode",
    exited: "sasacode exited (code {code})",
    empty_lead: "Write below what you want done in this folder",
    empty_note: "Before it edits a file or runs a command, sasacode asks as the permission setting says",
    more_lines: "{n} more lines",
    usage_in: "in",
    usage_out: "out",
    usage_cache: "cache",
};
