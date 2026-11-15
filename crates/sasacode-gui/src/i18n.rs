//! The window's words, in Japanese or English (`SASACODE_LANG`, else the system's `LANG`).

use sasacode_protocol::StopCause;

pub struct Strings {
    pub ja: bool,
    pub new_session: &'static str,
    pub sessions: &'static str,
    pub no_sessions: &'static str,
    pub placeholder: &'static str,
    pub placeholder_running: &'static str,
    pub send: &'static str,
    pub stop: &'static str,
    pub allow: &'static str,
    pub always: &'static str,
    pub deny: &'static str,
    pub feedback: &'static str,
    pub approve_question: &'static str,
    pub allowed: &'static str,
    pub always_allowed: &'static str,
    pub denied: &'static str,
    pub cancelled: &'static str,
    pub running: &'static str,
    pub idle: &'static str,
    pub thinking: &'static str,
    pub model_default: &'static str,
    pub permission: &'static str,
    pub effort: &'static str,
    pub context_full: &'static str,
    pub compacted: &'static str,
    pub retrying: &'static str,
    pub not_found: &'static str,
    pub exited: &'static str,
    pub empty: &'static str,
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

    pub fn stopped(&self, cause: StopCause) -> String {
        let why = match (cause, self.ja) {
            (StopCause::Aborted, true) => "中断しました",
            (StopCause::Aborted, false) => "aborted",
            (StopCause::MaxTurns, true) => "ターン数の上限に達しました",
            (StopCause::MaxTurns, false) => "turn limit reached",
            (StopCause::ContextLimit, true) => "コンテキストがいっぱいです",
            (StopCause::ContextLimit, false) => "the context window is full",
            (StopCause::Refusal, true) => "モデルが応答を拒否しました",
            (StopCause::Refusal, false) => "the model refused",
            (StopCause::Error, true) => "エラーで止まりました",
            (StopCause::Error, false) => "stopped by an error",
            (_, true) => "止まりました",
            (_, false) => "stopped",
        };
        format!("[{why}]")
    }
}

const JA: Strings = Strings {
    ja: true,
    new_session: "＋ 新しい会話",
    sessions: "このフォルダの会話",
    no_sessions: "まだ会話はありません",
    placeholder: "メッセージを入力（Enterで送信、Shift+Enterで改行）",
    placeholder_running: "実行中です。入力すると次のターンでモデルに届きます",
    send: "送信",
    stop: "中断",
    allow: "許可",
    always: "常に許可",
    deny: "拒否",
    feedback: "拒否の理由（任意）",
    approve_question: "を実行しますか？",
    allowed: "許可しました",
    always_allowed: "常に許可しました",
    denied: "拒否しました",
    cancelled: "取り消されました",
    running: "実行中…",
    idle: "待機中",
    thinking: "思考",
    model_default: "既定のモデル",
    permission: "権限",
    effort: "推論",
    context_full: "コンテキストの上限に近づいています",
    compacted: "会話を圧縮しました",
    retrying: "接続が切れたため、もう一度問い合わせています",
    not_found: "sasacode を起動できませんでした。PATHに入れるか、SASACODE_BIN か --sasacode で場所を指定してください",
    exited: "sasacode が終了しました（終了コード {code}）",
    empty: "下の欄にメッセージを書いて始めます",
};

const EN: Strings = Strings {
    ja: false,
    new_session: "+ New conversation",
    sessions: "Conversations in this folder",
    no_sessions: "No conversations yet",
    placeholder: "Message (Enter to send, Shift+Enter for a new line)",
    placeholder_running: "Running. What you send reaches the model at its next turn",
    send: "Send",
    stop: "Stop",
    allow: "Allow",
    always: "Always allow",
    deny: "Deny",
    feedback: "Why not (optional)",
    approve_question: "— run this?",
    allowed: "allowed",
    always_allowed: "always allowed",
    denied: "denied",
    cancelled: "cancelled",
    running: "Running…",
    idle: "Idle",
    thinking: "Thinking",
    model_default: "default model",
    permission: "Permission",
    effort: "Effort",
    context_full: "The context window is nearly full",
    compacted: "The conversation was compacted",
    retrying: "The connection dropped; asking again",
    not_found: "Could not start sasacode. Put it on PATH, or give its location with SASACODE_BIN or --sasacode",
    exited: "sasacode exited (code {code})",
    empty: "Write a message below to start",
};
