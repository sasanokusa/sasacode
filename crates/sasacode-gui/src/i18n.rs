//! The window's words, in Japanese or English (`SASACODE_LANG`, else the system's `LANG`).
//! `{n}`-style marks in a string are filled with [`fill`].

use std::fmt::Display;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Strings {
    pub ja: bool,

    // The list of conversations.
    pub sessions: &'static str,
    pub new_session: &'static str,
    pub find: &'static str,
    pub msgs: &'static str,
    pub just_now: &'static str,
    pub mins_ago: &'static str,
    pub hours_ago: &'static str,
    pub days_ago: &'static str,
    pub no_sessions: &'static str,
    pub no_match: &'static str,
    pub unread: &'static str,

    // The state of the conversation.
    pub st_running: &'static str,
    pub st_waiting: &'static str,
    pub st_aborted: &'static str,
    pub st_error: &'static str,
    pub st_max: &'static str,
    pub st_plugin: &'static str,
    pub st_done: &'static str,
    pub st_loaded: &'static str,

    // What the calls did.
    pub v_read: &'static str,
    pub v_read_live: &'static str,
    pub v_write: &'static str,
    pub v_overwrite: &'static str,
    pub v_write_live: &'static str,
    pub v_write_any: &'static str,
    pub v_wrote: &'static str,
    /// For a call that has not run, or never did.
    pub v_edit_any: &'static str,
    pub v_bash_any: &'static str,
    pub v_edit: &'static str,
    pub v_edit_live: &'static str,
    pub v_bash: &'static str,
    pub v_bash_live: &'static str,
    pub v_task: &'static str,
    pub lines: &'static str,
    pub out_lines: &'static str,
    pub no_output: &'static str,
    pub exit: &'static str,
    pub killed_abort: &'static str,
    pub killed_timeout: &'static str,
    pub repaired: &'static str,
    pub calls: &'static str,
    pub failed: &'static str,
    pub tag_waiting: &'static str,
    pub tag_failed: &'static str,
    pub tag_denied: &'static str,
    pub tag_not_run: &'static str,
    pub more_lines: &'static str,
    pub plan_made: &'static str,
    pub plan_steps: &'static str,
    pub plan_updated: &'static str,
    pub step_done: &'static str,
    pub thinking: &'static str,
    pub note_harness: &'static str,
    pub note_task: &'static str,
    pub waiting_appr: &'static str,
    pub you_allowed: &'static str,
    pub you_always: &'static str,
    pub you_denied: &'static str,
    pub appr_cancelled: &'static str,

    // A finished or running exchange.
    pub worked: &'static str,
    pub show_steps: &'static str,
    pub hide_steps: &'static str,
    pub live_work: &'static str,
    pub to_stop: &'static str,
    pub done_in: &'static str,
    pub files_changed: &'static str,
    pub review: &'static str,
    pub next_turn: &'static str,
    pub end_aborted: &'static str,
    pub end_aborted_p: &'static str,
    pub end_aborted_p0: &'static str,
    pub end_error: &'static str,
    pub end_max: &'static str,
    pub end_saved: &'static str,
    pub end_plugin: &'static str,
    pub end_refusal: &'static str,
    pub end_context: &'static str,
    pub end_other: &'static str,
    pub continue_btn: &'static str,
    pub continue_text: &'static str,
    pub compacted: &'static str,
    pub discarded: &'static str,
    pub context_full: &'static str,
    pub loaded_note: &'static str,

    // The approval card.
    pub appr_title: &'static str,
    pub appr_reason: &'static str,
    pub appr_write: &'static str,
    pub allow: &'static str,
    pub always_cmd: &'static str,
    pub always_file: &'static str,
    pub always_call: &'static str,
    pub deny: &'static str,
    pub more_waiting: &'static str,

    // The input.
    pub ph_new: &'static str,
    pub ph_running: &'static str,
    pub ph_waiting: &'static str,
    pub ph_idle: &'static str,
    pub send: &'static str,
    pub send_more: &'static str,
    pub deny_send: &'static str,
    pub stop: &'static str,
    pub h_send: &'static str,
    pub h_nl: &'static str,
    pub h_now: &'static str,
    pub h_allow: &'static str,
    pub model_ph: &'static str,
    pub default_tag: &'static str,
    pub m_model: &'static str,
    pub m_mode: &'static str,
    pub m_effort: &'static str,
    /// What the two menus show while nothing is chosen in them.
    pub mode_ph: &'static str,
    pub effort_ph: &'static str,
    pub next_run: &'static str,
    pub mode_edits: &'static str,
    pub mode_ask: &'static str,
    pub mode_agent: &'static str,
    pub mode_auto: &'static str,

    // The inspector.
    pub tab_changes: &'static str,
    pub tab_plan: &'static str,
    pub tab_cmd: &'static str,
    pub tab_usage: &'static str,
    pub ch_empty: &'static str,
    pub ch_empty_p: &'static str,
    pub ch_from_args: &'static str,
    pub new_file: &'static str,
    pub pl_empty: &'static str,
    pub pl_empty_p: &'static str,
    pub pl_done: &'static str,
    pub cm_empty: &'static str,
    pub cm_empty_p: &'static str,
    pub running: &'static str,
    pub us_head: &'static str,
    pub us_in: &'static str,
    pub us_out: &'static str,
    pub us_cr: &'static str,
    pub us_cw: &'static str,
    pub us_cost: &'static str,
    pub us_req: &'static str,
    pub us_last: &'static str,
    pub us_note: &'static str,

    // The rest.
    pub blank_h: &'static str,
    pub blank_p: &'static str,
    pub a_side: &'static str,
    pub a_insp: &'static str,
    pub copy: &'static str,
    pub copied: &'static str,
    pub copy_cmd: &'static str,
    pub copy_out: &'static str,
    pub copy_id: &'static str,
    pub latest: &'static str,
    pub not_found: &'static str,
    pub exited: &'static str,
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

    /// `1分12秒` / `1m 12s`. Under a second shows as one: a call that ran did take time.
    pub fn duration(&self, d: Duration) -> String {
        let s = d.as_secs().max(1);
        let (h, m, sec) = (s / 3600, s % 3600 / 60, s % 60);
        match (self.ja, h, m) {
            (true, 0, 0) => format!("{sec}秒"),
            (true, 0, _) => format!("{m}分{sec}秒"),
            (true, _, _) => format!("{h}時間{m}分"),
            (false, 0, 0) => format!("{sec}s"),
            (false, 0, _) => format!("{m}m {sec}s"),
            (false, _, _) => format!("{h}h {m}m"),
        }
    }

    /// How long ago, for the list of conversations.
    pub fn ago(&self, d: Duration) -> String {
        let s = d.as_secs();
        match s {
            0..60 => self.just_now.to_owned(),
            60..3600 => fill(self.mins_ago, "n", s / 60),
            3600..86_400 => fill(self.hours_ago, "n", s / 3600),
            _ => fill(self.days_ago, "n", s / 86_400),
        }
    }
}

/// `template` with its `{key}` replaced.
pub fn fill(template: &str, key: &str, value: impl Display) -> String {
    template.replace(&format!("{{{key}}}"), &value.to_string())
}

/// `1234` → `1.2k`, `1234567` → `1.2M`.
pub fn compact(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..1_000_000 => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

const JA: Strings = Strings {
    ja: true,

    sessions: "このフォルダの会話",
    new_session: "新しい会話",
    find: "会話を検索",
    msgs: "{n}件のメッセージ",
    just_now: "たった今",
    mins_ago: "{n}分前",
    hours_ago: "{n}時間前",
    days_ago: "{n}日前",
    no_sessions: "まだ会話はありません",
    no_match: "該当する会話がありません",
    unread: "見ていないあいだに動きがありました",

    st_running: "実行中",
    st_waiting: "承認待ち",
    st_aborted: "中断",
    st_error: "エラーで停止",
    st_max: "ターン上限で停止",
    st_plugin: "プラグインが停止",
    st_done: "完了",
    st_loaded: "読み込み済み",

    v_read: "読み込み",
    v_read_live: "読み込み中",
    v_write: "作成",
    v_overwrite: "上書き",
    v_write_live: "書き込み中",
    v_write_any: "書き込み",
    v_wrote: "書き込み",
    v_edit_any: "編集",
    v_bash_any: "実行",
    v_edit: "編集",
    v_edit_live: "編集中",
    v_bash: "実行",
    v_bash_live: "実行中",
    v_task: "サブエージェント",
    lines: "{n}行",
    out_lines: "出力{n}行",
    no_output: "出力なし",
    exit: "終了コード{n}",
    killed_abort: "中断",
    killed_timeout: "タイムアウト",
    repaired: "修復済み",
    calls: "ツール呼び出し{n}回",
    failed: "うち失敗{n}回",
    tag_waiting: "待機中",
    tag_failed: "失敗",
    tag_denied: "拒否",
    tag_not_run: "未実行",
    more_lines: "ほか{n}行",
    plan_made: "計画を作成",
    plan_steps: "{n}件",
    plan_updated: "計画を更新",
    step_done: "完了：",
    thinking: "思考",
    note_harness: "ハーネスからモデルへの注意",
    note_task: "バックグラウンドのタスクが報告",
    waiting_appr: "承認を待っています：",
    you_allowed: "許可しました：",
    you_always: "この実行中は許可にしました：",
    you_denied: "拒否しました：",
    appr_cancelled: "承認は取り消されました：",

    worked: "{d}作業しました",
    show_steps: "手順を表示",
    hide_steps: "手順を隠す",
    live_work: "作業中",
    to_stop: "で中断",
    done_in: "{d}で完了",
    files_changed: "{n}ファイルを変更",
    review: "変更を見る",
    next_turn: "次のターンでモデルに届きます",
    end_aborted: "中断しました",
    end_aborted_p: "{n}件のファイルの変更は作業フォルダに残っています。",
    end_aborted_p0: "変更されたファイルはありません。",
    end_error: "エラーで止まりました",
    end_max: "ターン数の上限に達しました",
    end_saved: "会話はここまで保存されています。",
    end_plugin: "{p}が実行を止めました",
    end_refusal: "モデルが応答を拒否しました",
    end_context: "コンテキストがいっぱいになりました",
    end_other: "実行が止まりました",
    continue_btn: "「続けてください」と送る",
    continue_text: "続けてください",
    compacted: "会話を圧縮しました",
    discarded: "接続が切れたため、もう一度問い合わせています",
    context_full: "コンテキストがいっぱいです",
    loaded_note: "セッションログから読み込んだ会話です。所要時間、承認の記録、サブエージェント内の呼び出しは残っていません。",

    appr_title: "承認が必要です",
    appr_reason: "確認の理由：",
    appr_write: "書き込む内容です。同じパスにファイルがあれば置き換わります。",
    allow: "許可",
    always_cmd: "この実行中は同じコマンドを許可",
    always_file: "この実行中はこのファイルを許可",
    always_call: "この実行中は同じ呼び出しを許可",
    deny: "拒否",
    more_waiting: "ほかに{n}件が承認を待っています",

    ph_new: "メッセージを入力",
    ph_running: "追加の指示（次のターンでモデルに届きます）",
    ph_waiting: "返信すると、拒否して理由を伝えます",
    ph_idle: "続きを入力",
    send: "送信",
    send_more: "追加で送る",
    deny_send: "拒否して送る",
    stop: "中断",
    h_send: "送信",
    h_nl: "改行",
    h_now: "中断して今すぐ送る",
    h_allow: "許可",
    model_ph: "既定のモデル",
    default_tag: "既定",
    m_model: "モデル（provider/model）。空なら設定ファイルの既定を使います",
    m_mode: "権限モード",
    m_effort: "推論の深さ",
    mode_ph: "権限モード",
    effort_ph: "推論の深さ",
    next_run: "次の送信から反映されます",
    mode_edits: "作業フォルダ内の読み書きは確認なし。bashとフォルダ外は確認します",
    mode_ask: "すべての呼び出しで確認します（読み取りも）",
    mode_agent: "フォルダ内の読み取りは実行。それ以外はモデルが判定し、安全でないものだけ確認します",
    mode_auto: "すべて実行します（denyとaskの規則は有効）",

    tab_changes: "変更",
    tab_plan: "計画",
    tab_cmd: "コマンド",
    tab_usage: "使用量",
    ch_empty: "まだ変更はありません",
    ch_empty_p: "writeとeditが変えたファイルの差分が、呼び出しごとにここへ並びます。",
    ch_from_args: "ログには差分が残らないため、呼び出しの引数から作っています。行番号と前後の行はありません。",
    new_file: "新規",
    pl_empty: "まだ計画はありません",
    pl_empty_p: "モデルがtodo_writeで手順を書くと、ここで進み具合が分かります。",
    pl_done: "{a}/{b}件完了",
    cm_empty: "まだ何も実行していません",
    cm_empty_p: "bashで実行したコマンドと出力が、ここに並びます。",
    running: "実行中",
    us_head: "この会話の使用量",
    us_in: "入力",
    us_out: "出力",
    us_cr: "キャッシュ読み取り",
    us_cw: "キャッシュ書き込み",
    us_cost: "料金（見積もり）",
    us_req: "リクエスト数",
    us_last: "直近のリクエストの入力",
    us_note: "料金は組み込みのモデル表からの見積もりで、請求額ではありません。",

    blank_h: "何を変えますか？",
    blank_p: "このフォルダで作業します。読み書きと実行は、権限モードに従って確認します。",
    a_side: "会話の一覧を切り替え",
    a_insp: "インスペクタを切り替え",
    copy: "コピー",
    copied: "コピーしました",
    copy_cmd: "コマンドをコピー",
    copy_out: "出力をコピー",
    copy_id: "クリックでIDをコピー（sasacode -rで続けられます）",
    latest: "最新へ",
    not_found: "sasacodeを起動できませんでした。PATHにあるか確かめるか、--sasacodeでパスを指定してください。",
    exited: "sasacodeが途中で終了しました（終了コード{code}）",
};

const EN: Strings = Strings {
    ja: false,

    sessions: "Conversations in this folder",
    new_session: "New conversation",
    find: "Search conversations",
    msgs: "{n} messages",
    just_now: "just now",
    mins_ago: "{n}m ago",
    hours_ago: "{n}h ago",
    days_ago: "{n}d ago",
    no_sessions: "No conversations yet",
    no_match: "No matching conversation",
    unread: "Something happened while you were away",

    st_running: "Running",
    st_waiting: "Needs your approval",
    st_aborted: "Stopped",
    st_error: "Stopped by an error",
    st_max: "Turn limit reached",
    st_plugin: "Stopped by a plugin",
    st_done: "Done",
    st_loaded: "Loaded",

    v_read: "Read",
    v_read_live: "Reading",
    v_write: "Created",
    v_overwrite: "Overwrote",
    v_write_live: "Writing",
    v_write_any: "Write",
    v_wrote: "Wrote",
    v_edit_any: "Edit",
    v_bash_any: "Run",
    v_edit: "Edited",
    v_edit_live: "Editing",
    v_bash: "Ran",
    v_bash_live: "Running",
    v_task: "Subagent",
    lines: "{n} lines",
    out_lines: "{n} lines of output",
    no_output: "no output",
    exit: "exit {n}",
    killed_abort: "stopped",
    killed_timeout: "timed out",
    repaired: "repaired",
    calls: "{n} tool calls",
    failed: "{n} failed",
    tag_waiting: "waiting",
    tag_failed: "failed",
    tag_denied: "denied",
    tag_not_run: "not run",
    more_lines: "{n} more lines",
    plan_made: "Made a plan",
    plan_steps: "{n} steps",
    plan_updated: "Updated the plan",
    step_done: "Done: ",
    thinking: "Thinking",
    note_harness: "Note from the harness to the model",
    note_task: "A background task reported",
    waiting_appr: "Waiting for your approval: ",
    you_allowed: "You allowed: ",
    you_always: "Allowed for this run: ",
    you_denied: "You denied: ",
    appr_cancelled: "Approval withdrawn: ",

    worked: "Worked for {d}",
    show_steps: "Show steps",
    hide_steps: "Hide steps",
    live_work: "Working",
    to_stop: "to stop",
    done_in: "Done in {d}",
    files_changed: "{n} files changed",
    review: "View changes",
    next_turn: "Reaches the model at its next turn",
    end_aborted: "You stopped the run",
    end_aborted_p: "Changes to {n} files are still in the working folder.",
    end_aborted_p0: "No files were changed.",
    end_error: "Stopped by an error",
    end_max: "Reached the turn limit",
    end_saved: "The conversation is saved up to here.",
    end_plugin: "{p} stopped the run",
    end_refusal: "The model refused to answer",
    end_context: "The context window is full",
    end_other: "The run stopped",
    continue_btn: "Send \"Please continue\"",
    continue_text: "Please continue",
    compacted: "The conversation was compacted",
    discarded: "The connection dropped mid-reply; asking again",
    context_full: "The context window is full",
    loaded_note: "Read back from the session log. Durations, approvals and the calls inside a subagent are not kept there.",

    appr_title: "Approval needed",
    appr_reason: "Why it asks: ",
    appr_write: "This is what would be written. A file at the same path is replaced.",
    allow: "Allow",
    always_cmd: "Allow this command for this run",
    always_file: "Allow this file for this run",
    always_call: "Allow this call for this run",
    deny: "Deny",
    more_waiting: "{n} more waiting for approval",

    ph_new: "Message",
    ph_running: "Add an instruction (it reaches the model at its next turn)",
    ph_waiting: "Reply to deny and say why",
    ph_idle: "Continue the conversation",
    send: "Send",
    send_more: "Send",
    deny_send: "Deny and send",
    stop: "Stop",
    h_send: "send",
    h_nl: "new line",
    h_now: "stop and send now",
    h_allow: "allow",
    model_ph: "Default model",
    default_tag: "default",
    m_model: "Model (provider/model). Empty uses the default of your config",
    m_mode: "Permission mode",
    m_effort: "Reasoning effort",
    mode_ph: "Mode",
    effort_ph: "Effort",
    next_run: "Applies from your next message",
    mode_edits: "Reads and edits inside the folder run. bash and anything outside ask",
    mode_ask: "Every call asks, reads included",
    mode_agent: "Reads inside the folder run. A model judges the rest and asks when unsure",
    mode_auto: "Everything runs (deny and ask rules still apply)",

    tab_changes: "Changes",
    tab_plan: "Plan",
    tab_cmd: "Commands",
    tab_usage: "Usage",
    ch_empty: "No changes yet",
    ch_empty_p: "Diffs of the files write and edit change collect here, one per call.",
    ch_from_args: "A log keeps no diffs, so these are rebuilt from the calls' arguments: no line numbers, no lines around the change.",
    new_file: "NEW",
    pl_empty: "No plan yet",
    pl_empty_p: "When the model writes steps with todo_write, they appear here and tick off.",
    pl_done: "{a} of {b} done",
    cm_empty: "Nothing has run yet",
    cm_empty_p: "Commands run with bash appear here with their output.",
    running: "running",
    us_head: "This conversation",
    us_in: "Input",
    us_out: "Output",
    us_cr: "Cache read",
    us_cw: "Cache write",
    us_cost: "Cost (estimate)",
    us_req: "Requests",
    us_last: "Input of the last request",
    us_note: "Cost is an estimate from the built-in model table, not a bill.",

    blank_h: "What do you want to change?",
    blank_p: "Work happens in this folder. Reads, writes and commands ask as the permission mode says.",
    a_side: "Toggle conversations",
    a_insp: "Toggle inspector",
    copy: "Copy",
    copied: "Copied",
    copy_cmd: "Copy command",
    copy_out: "Copy output",
    copy_id: "Click to copy the id (sasacode -r continues it)",
    latest: "Jump to latest",
    not_found: "Could not start sasacode. Check that it is on PATH, or pass --sasacode <path>.",
    exited: "sasacode ended early (exit code {code})",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_counts_read_naturally() {
        let (ja, en) = (Strings::get("ja_JP.UTF-8"), Strings::get("C"));
        assert_eq!(ja.duration(Duration::from_millis(300)), "1秒");
        assert_eq!(ja.duration(Duration::from_secs(72)), "1分12秒");
        assert_eq!(en.duration(Duration::from_secs(72)), "1m 12s");
        assert_eq!(en.duration(Duration::from_secs(3 * 3600 + 120)), "3h 2m");
        assert_eq!(ja.ago(Duration::from_secs(5)), "たった今");
        assert_eq!(ja.ago(Duration::from_secs(7200)), "2時間前");
        assert_eq!(fill(ja.msgs, "n", 28), "28件のメッセージ");
        assert_eq!(
            (compact(999), compact(1400), compact(2_500_000)),
            ("999".into(), "1.4k".into(), "2.5M".into())
        );
    }

    /// The Japanese text never puts a half-width space between Japanese and Latin letters.
    #[test]
    fn japanese_strings_have_no_space_between_japanese_and_latin() {
        let all = format!("{JA:?}");
        let chars: Vec<char> = all.chars().collect();
        let japanese =
            |c: char| matches!(c, '\u{3040}'..='\u{30ff}' | '\u{4e00}'..='\u{9fff}' | '、' | '。' | '（' | '）' | '：');
        for w in chars.windows(3) {
            let latin = |c: char| c.is_ascii_alphanumeric();
            let bad = w[1] == ' ' && ((japanese(w[0]) && latin(w[2])) || (latin(w[0]) && japanese(w[2])));
            assert!(!bad, "{:?}", w.iter().collect::<String>());
        }
    }
}
