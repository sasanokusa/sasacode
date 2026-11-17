//! More of the window's words, each in Japanese and English on one line: those of what was added
//! after `i18n.rs` (the git tab's hunks, branches and pull request, comments on a diff, and the
//! rest). Reached as `s.w()` from the window's [`crate::i18n::Strings`].

macro_rules! words {
    ($($(#[$doc:meta])* $name:ident: $ja:expr, $en:expr;)*) => {
        #[derive(Debug, PartialEq, Eq)]
        pub struct Words {
            $($(#[$doc])* pub $name: &'static str,)*
        }
        pub const JA: Words = Words { $($name: $ja,)* };
        pub const EN: Words = Words { $($name: $en,)* };

        #[cfg(test)]
        impl Words {
            fn all(&self) -> Vec<&'static str> {
                vec![$(self.$name,)*]
            }
        }
    };
}

words! {
    // The git tab: hunks, comments, branches.
    staged_head: "ステージ済み", "Staged";
    unstaged_head: "未ステージ", "Not staged";
    quote_hint: "行をクリックするとコメントを書けます。コメントは次のメッセージに添えて送ります", "Click a line to comment on it. Comments go with the next message";
    comment_ph: "この行へのコメント（↵で足す）", "A comment on this line (↵ adds it)";
    comment_add: "コメントを足す", "Add comment";
    comment_quote: "場所だけ入力欄へ", "Put the place in the input";
    cancel: "やめる", "Cancel";
    comments_head: "差分へのコメント（次のメッセージに添える）", "Comments on the diff (sent with the next message)";
    /// The line before the comments in the message sent.
    comments_block: "差分へのコメント:", "Comments on the diff:";
    remove: "外す", "Remove";
    branch_switch: "ブランチを切り替える", "Switch branch";
    branch_ph: "ブランチを探すか、新しいブランチの名前を入力", "Find a branch, or type a new branch's name";
    branch_new: "新しいブランチ「{name}」を作る", "Create branch “{name}”";
    branch_here: "（今のブランチ）", "(checked out)";
    branch_loading: "ブランチを読み込み中…", "Reading the branches…";

    // The git tab: the pull request and its checks.
    pr_head: "プルリクエスト", "Pull request";
    pr_none: "このブランチのプルリクエストはまだありません", "This branch has no pull request yet";
    pr_loading: "プルリクエストを読み込み中…", "Reading the pull request…";
    pr_open: "OPEN", "Open";
    pr_merged: "マージ済み", "Merged";
    pr_closed: "クローズ", "Closed";
    approved: "承認済み", "Approved";
    changes_requested: "修正を求められています", "Changes requested";
    review_required: "レビュー待ち", "Review required";
    checks: "チェック", "Checks";
    check_passed: "成功", "passed";
    check_failed: "失敗", "failed";
    check_pending: "実行中", "running";
    check_skipped: "飛ばした", "skipped";
    fix_checks: "失敗したチェックを直させる", "Have the agent fix the failing checks";
    fix_reading: "失敗したログを読み込み中…", "Reading the failed logs…";
    fix_prompt: "このブランチのプルリクエスト（{url}）で、次のチェックが失敗しています。原因を調べて直してください。\n\n{logs}", "These checks fail on this branch's pull request ({url}). Find out why and fix them.\n\n{logs}";
    merge: "マージ", "Merge";
    merge_sure: "もう一度押すとマージします", "Click again to merge";
    reviews: "レビューのコメント", "Review comments";
    address: "入力欄へ", "To the input";
    address_prompt: "プルリクエストのレビューのコメントに対応してください。\n\n{who}: {body}", "Address this review comment on the pull request.\n\n{who}: {body}";
    open_browser: "ブラウザで開く", "Open in the browser";
    set_auto_archive: "ブランチのプルリクエストがマージかクローズされた会話をアーカイブする", "Archive a conversation when its branch's pull request is merged or closed";
    swept: "プルリクエストが閉じた会話を{n}件アーカイブしました", "Archived {n} conversations whose pull request closed";
    // The list of conversations: deleting one, and groups.
    c_delete: "会話を削除", "Delete conversation";
    delete_sure: "もう一度押すと会話のログを削除します（元に戻せません）", "Click again to delete the conversation's log (this can't be undone)";
    delete_running: "実行中の会話は削除できません。止めてからもう一度", "A running conversation can't be deleted. Stop it first";
    deleted: "会話を削除しました", "Conversation deleted";
    c_group: "会話をグループに入れる", "Put conversation in a group";
    group_title: "グループ", "Group";
    group_ph: "グループの名前", "The group's name";
    group_hint: "空にするとグループから外します", "Leave it empty to take it out of its group";
    ungrouped: "その他", "Other";
    // The files tab.
    tab_files: "ファイル", "Files";
    files_ph: "ファイルを探す", "Find a file";
    files_listing: "ファイルを読み込み中…", "Reading the files…";
    files_more: "ほか{n}件。名前で絞り込んでください", "{n} more. Type to narrow them down";
    files_back: "ファイル一覧", "All files";
    files_cut: "先頭{shown}行だけ表示しています（全{n}行）", "Showing the first {shown} of {n} lines";
    files_unreadable: "表示できません: {why}", "Can't show it: {why}";
    files_hint: "行をクリックするとコメントを書けます", "Click a line to comment on it";
    view: "表示", "View";
    // The folder's scripts.
    c_scripts: "スクリプトを実行", "Run a script";
    script_ph: "実行するスクリプトを探す", "Find a script to run";
    script_edit: "スクリプトを編集…", "Edit scripts…";
    set_scripts: "このフォルダのスクリプト", "This folder's scripts";
    set_scripts_hint: "ログインシェルでこのフォルダから実行し、出力はインスペクタのコマンドタブに出ます（⌘⇧R）", "Run by your login shell in this folder; the output shows in the inspector's Commands tab (⌘⇧R)";
    script_name_ph: "名前", "Name";
    script_command_ph: "コマンド（例: npm test）", "Command (e.g. npm test)";
    script_add: "スクリプトを追加", "Add script";
    script_stopped: "（止めました）", "(stopped)";
    script_prompt: "`{command}`を実行した出力です。見て、問題があれば直してください。\n\n```\n{output}\n```", "This is the output of `{command}`. Look at it and fix what is wrong.\n\n```\n{output}\n```";
    script_runs: "スクリプトの実行", "Script runs";
    script_running: "実行中", "Running";
    script_exit: "終了コード {code}", "Exit code {code}";
    script_again: "もう一度", "Run again";
    script_to_agent: "エージェントに見せる", "Show the agent";
    stop: "止める", "Stop";
    // The list of keys, where they are changed.
    key_change: "変更", "Change";
    key_default: "既定に戻す", "Reset";
    key_all_default: "すべて既定に戻す", "Reset all";
    key_none: "なし", "None";
    key_press: "新しいキーを押してください（Escでやめる）", "Press the new key (Esc to cancel)";
    key_waiting: "キーを待っています…", "Waiting for a key…";
    key_needs_mod: "{mod}と一緒に押してください", "Hold {mod} with it";
    key_reserved: "{key}は文字の編集かシステムが使うので割り当てられません", "{key} is kept for editing text or the system";
    key_taken: "{key}は「{other}」に使われています", "{key} is already “{other}”";
    keys_fixed: "決まっているキー", "Fixed keys";
    keys_yours: "変えられるキー", "Keys you can change";
    // The outline.
    c_outline: "会話の目次", "Conversation outline";
    outline_ph: "メッセージや見出しを探して移動", "Find a message or a heading to go to";
    // The conversation beside the one shown.
    c_split: "会話を横に開く", "Open a conversation beside";
    split_close: "横の会話を閉じる", "Close the conversation beside";
    split_ph: "横に開く会話を探す（一覧で⌘クリックでも開けます）", "Find a conversation to open beside (or ⌘-click it in the list)";
    split_swap: "こちらで続ける", "Continue here";
    split_swap_hint: "メインに移して入力や承認ができるようにします。今の会話はこちらに来ます", "Moves it to the main place to type or answer; the current one comes here";
    split_waiting: "承認を待っています。続けるにはメインに移してください", "Waiting for an approval. Move it to the main place to answer";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_word_is_said_in_both_languages_with_the_same_marks() {
        for (ja, en) in JA.all().into_iter().zip(EN.all()) {
            assert!(!ja.is_empty() && !en.is_empty());
            let marks = |s: &str| -> Vec<String> {
                let mut found: Vec<String> = s
                    .split('{')
                    .skip(1)
                    .filter_map(|rest| rest.split_once('}').map(|(m, _)| m.to_owned()))
                    .collect();
                found.sort();
                found
            };
            assert_eq!(marks(ja), marks(en), "{ja} / {en}");
        }
    }
}
