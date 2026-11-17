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
    delete_sure: "もう一度押すと会話を削除します（7日間は元に戻せます）", "Click again to delete the conversation (it can be brought back for 7 days)";
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
    // What the model is asked apart from any conversation.
    draft_commit: "メッセージを書かせる", "Write it for me";
    c_draft_commit: "コミットメッセージをモデルに書かせる", "Write the commit message with the model";
    draft_commit_hint: "変更（ステージ済みならそれだけ）と最近のコミットからモデルが書きます", "The model writes it from the change (only what is staged, if anything is) and the recent commits";
    drafting: "書いています…", "Writing…";
    draft_failed: "書けませんでした: {error}", "Could not write it: {error}";
    pr_title: "プルリクエストを作る", "Create a pull request";
    pr_title_ph: "タイトル", "Title";
    pr_body_ph: "説明（Markdown）", "Description (Markdown)";
    pr_draft: "モデルに書かせる", "Write it with the model";
    pr_fill: "gh に任せる (--fill)", "Let gh fill it (--fill)";
    pr_create: "作成", "Create";
    pr_base: "{base} へ · {n}件のコミット", "Into {base} · {n} commits";
    suggest_label: "次に", "Next";
    suggest_take: "入力欄に入れる", "Put it in the input";
    set_suggest: "返答のあとに次の入力を提案する（モデルを1回呼びます）", "Suggest the next message after a reply (asks the model once)";
    // Asked before the window closes on a run, and before `auto`.
    quit_title: "実行中のものがあります", "Something is running";
    quit_body: "{n}件の実行が動いています。窓を閉じると止まります。", "{n} runs are going on. Closing the window stops them.";
    quit_yes: "止めて閉じる", "Stop them and close";
    auto_title: "確認なしで実行するモード", "Run without asking";
    auto_body: "autoでは、エージェントがコマンドの実行やファイルの変更を確認せずに行います。信頼できるフォルダでだけ使ってください。この確認は一度だけです。", "In auto, the agent runs commands and changes files without asking. Use it only in folders you trust. This is asked once.";
    auto_yes: "autoにする", "Use auto";
    mark_unread: "この会話を未読にする", "Mark this conversation unread";
    mark_read: "この会話を既読にする", "Mark this conversation read";
    copy_folder: "フォルダのパスをコピー", "Copy the folder's path";
    copy_log: "会話のログのパスをコピー", "Copy the conversation log's path";
    exported: "{path}に書き出しました", "Written to {path}";
    export_failed: "書き出せませんでした: {error}", "Could not write it: {error}";
    c_tui: "ターミナルのTUIで続ける", "Continue in the terminal's TUI";
    tui_failed: "ターミナルを開けませんでした", "Could not open a terminal";
    set_width: "会話の幅", "Conversation width";
    width_normal: "標準", "Normal";
    width_wide: "広く", "Wide";
    width_full: "窓いっぱい", "Full";
    show_whole: "全文を表示", "Show all";
    fold_whole: "たたむ", "Fold";
    set_lang: "言語", "Language";
    lang_system: "システムに合わせる", "As the system";
    // The git tab's scopes, and more of what it does.
    scope_changes: "変更", "Changes";
    scope_branch: "ブランチ全体", "Whole branch";
    scope_commit: "コミットを選ぶ", "Pick a commit";
    scope_base: "{base}との比較", "Compared with {base}";
    scope_loading: "読んでいます…", "Reading…";
    scope_empty: "違いはありません", "No differences";
    split_diff: "2列", "Side by side";
    hide_ws: "空白を無視", "Ignore white space";
    fold_all: "全部たたむ", "Fold all";
    open_all: "全部開く", "Open all";
    viewed: "見た", "Viewed";
    viewed_n: "{n}/{all}件見た", "{n}/{all} viewed";
    default_sure: "{branch}に直接書き込みます。もう一度押すと続けます", "This goes straight to {branch}. Press again to go on";
    git_init: "ここでgitを始める", "Start git here";
    pull: "取り込む", "Pull";
    behind: "↓{n}", "↓{n}";
    update_branch: "ベースを取り込む", "Update from base";
    auto_merge: "チェックが通ったらマージ", "Merge when checks pass";
    auto_merge_on: "自動マージ待ち", "Auto-merge armed";
    ready: "レビュー可能にする", "Ready for review";
    to_draft: "下書きに戻す", "Back to draft";
    draft_tag: "下書き", "Draft";
    pr_as_draft: "下書きとして作る", "Create as a draft";
    merge_how: "マージの仕方", "Merge method";
    // The conversation beside the one shown.
    c_split: "会話を横に開く", "Open a conversation beside";
    split_close: "横の会話を閉じる", "Close the conversation beside";
    split_ph: "横に開く会話を探す（一覧で⌘クリックでも開けます）", "Find a conversation to open beside (or ⌘-click it in the list)";
    split_swap: "こちらで続ける", "Continue here";
    split_swap_hint: "メインに移して入力や承認ができるようにします。今の会話はこちらに来ます", "Moves it to the main place to type or answer; the current one comes here";
    split_waiting: "承認を待っています。続けるにはメインに移してください", "Waiting for an approval. Move it to the main place to answer";
    // The list: its menu, rows chosen together, its filter, the trash and taking back.
    menu_open: "開く", "Open";
    menu_window: "新しいウィンドウで開く", "Open in a new window";
    menu_select: "選択に加える", "Add to selection";
    menu_unselect: "選択から外す", "Remove from selection";
    menu_copy_id: "会話IDをコピー", "Copy conversation ID";
    menu_copy_log: "ログのパスをコピー", "Copy log path";
    menu_copy_path: "パスをコピー", "Copy path";
    menu_mention: "@で入力欄に入れる", "Mention in the input (@)";
    selected_n: "{n}件を選択中", "{n} selected";
    select_hint: "⇧クリックで会話をまとめて選べます", "⇧-click conversations to choose several";
    select_none: "選択をやめる", "Clear selection";
    bulk_unread: "未読・既読を切り替える", "Toggle read";
    delete_n_sure: "もう一度押すと{n}件の会話を削除します（7日間は元に戻せます）", "Click again to delete {n} conversations (they can be brought back for 7 days)";
    deleted_n: "{n}件の会話を削除しました", "Deleted {n} conversations";
    archived_one: "アーカイブしました", "Archived";
    unarchived_one: "一覧に戻しました", "Moved back to the list";
    archived_n_done: "{n}件をアーカイブしました", "Archived {n} conversations";
    unarchived_n: "{n}件を一覧に戻しました", "Moved {n} conversations back to the list";
    archived_idle: "しばらく使っていない会話を{n}件アーカイブしました", "Archived {n} conversations not used for a while";
    c_undo: "一覧の変更を元に戻す", "Undo the last list change";
    undo: "元に戻す", "Undo";
    undone: "元に戻しました", "Undone";
    undo_none: "元に戻せる変更はありません", "Nothing to undo";
    read_all: "すべての会話を既読にする", "Mark all conversations read";
    list_filter: "絞り込みと並べ替え", "Filter and sort";
    show_all: "すべて", "All";
    show_running: "動作中", "Running";
    show_waiting: "承認待ち", "Waiting";
    show_unread: "未読", "Unread";
    order_updated: "更新順", "Last updated";
    order_created: "作成順", "Created";
    row_model: "モデル: {model}", "Model: {model}";
    row_branch: "ブランチ: {branch}", "Branch: {branch}";
    row_pr: "プルリクエスト: #{n}（{state}）", "Pull request: #{n} ({state})";
    set_archive_after: "使っていない会話を自動でアーカイブ", "Archive conversations not used for";
    archive_never: "しない", "Never";
    archive_days: "{n}日", "{n} days";
    // The conversation shown: branching after a reply, its search, its steps.
    fork_after: "ここから分岐", "Branch from here";
    fork_after_hint: "この返事までを持つ新しい会話を作ります。元の会話はそのまま残ります", "Starts a new conversation holding this one through this reply; this one is kept as it is";
    forked: "ここまでの内容で新しい会話を作りました", "Started a new conversation from here";
    found_at: "{at}/{n}件", "{at} of {n}";
    find_prev: "前を探す（⇧⌘G）", "Previous (⇧⌘G)";
    find_next: "次を探す（↵ / ⌘G）", "Next (↵ / ⌘G)";
    ref_missing: "{path} は見つかりませんでした", "{path} was not found";
    set_failed_only: "ツール呼び出しは失敗したものだけ表示する", "Show only the tool calls that failed";
    set_show_thinking: "モデルの考えを表示する", "Show what the model thought";
    alert_note: "メモ", "Note";
    alert_tip: "ヒント", "Tip";
    alert_important: "重要", "Important";
    alert_warning: "注意", "Warning";
    alert_caution: "警告", "Caution";
    // Follow-ups, `!command`, quoting, another conversation as context, the branch.
    set_follow: "実行中に送ったメッセージ", "Messages sent while it runs";
    follow_steer: "すぐ伝える（次のターンで読む）", "Steer (read at its next turn)";
    follow_queue: "終わってから順に送る", "Queue (sent after it ends)";
    follow_head: "実行が終わってから送るメッセージ", "Sent after the run ends";
    follow_now: "今伝える", "Send now";
    follow_edit: "入力欄に戻す", "Edit";
    quote: "選んで引用", "Quote part";
    quote_part_hint: "返事の一部を選んで、入力欄に引用します", "Choose part of the reply and quote it in the input";
    quote_title: "引用する部分を選ぶ", "Choose what to quote";
    quote_how: "ドラッグか⇧と矢印キーで選び、「引用する」を押します", "Select with the mouse or ⇧ and the arrows, then press Quote";
    quote_take: "引用する", "Quote";
    quote_none: "まだ何も選んでいません", "Nothing is chosen yet";
    attach_conversation: "この会話を入力欄に添える", "Attach to the input";
    attached_conversation: "会話をMarkdownにして入力欄に添えました", "Attached the conversation as Markdown";
    branch_moved: "この会話は{was}で始めましたが、今は{now}です", "This conversation started on {was}; {now} is checked out now";
    branch_back: "{branch}に戻る", "Back to {branch}";
    // The plugins of a run: questions, texts, the judge; a call stopped or edited.
    plugin_text: "プラグインの表示", "From a plugin";
    ask_head: "プラグインからの質問", "A plugin asks";
    ask_yes: "はい", "Yes";
    ask_no: "いいえ", "No";
    ask_ok: "決定", "OK";
    ask_dismiss: "答えずに閉じる", "Dismiss";
    ask_more: "ほかに{n}件", "{n} more";
    stop_call: "この呼び出しだけ止める", "Stop this call";
    args_edit: "引数を直して許可", "Edit and allow";
    args_title: "引数を直して許可する", "Edit the arguments, then allow";
    args_how: "JSONのまま直します。直した呼び出しも、拒否のルールとプラグインの確認を受け直します", "Edit the JSON. The edited call is checked again against the deny rules and the plugins";
    args_send: "この引数で許可", "Allow with these";
    args_bad: "引数はJSONのオブジェクトで書いてください（{error}）", "The arguments must be a JSON object ({error})";
    judged_allow: "判定役が許可", "The judge allowed it";
    judged_ask: "判定役が確認に回した", "The judge asked you";
    allowed_head: "この会話で常に許可したもの", "Always allowed in this conversation";
    allowed_none: "まだありません", "Nothing yet";
    slash_skill: "スキル", "skill";
    trust_head: "このフォルダの設定は、信頼するまで使われません", "This folder's settings are not used until it is trusted";
    trust_save: "信頼する", "Trust";
    trust_once: "このウィンドウでだけ使う", "Use in this window only";
    trust_partial: "プラグインのコードをすべては読めなかったので、信頼を保存できません", "Its plugins' code could not all be read, so trust cannot be saved";
    ext_title: "プラグイン・スキル・MCP", "Plugins, skills and MCP";
    ext_plugins: "プラグイン", "Plugins";
    ext_skills: "スキル", "Skills";
    ext_mcp: "このフォルダのMCPサーバー", "This folder's MCP servers";
    ext_off: "オフ", "off";
    ext_none: "ありません", "None";
    ext_old: "このsasacodeは一覧を返しません（0.8より前）", "This sasacode does not list them (before 0.8)";
    ext_reload: "読み直す", "Reload";
    c_extensions: "プラグイン・スキル・MCPを見る", "Plugins, skills and MCP";
    model_pick: "モデルを選ぶ", "Choose a model";
    compact_at: "{n}%（{tokens}）を超えると自動で要約します", "Summarised on its own past {n}% ({tokens})";
    compact_off: "自動の要約はオフです", "Automatic summarising is off";
    image_actual: "等倍で見る", "Actual size";
    image_fit: "窓に合わせる", "Fit";
    image_open: "ほかのアプリで開く", "Open in another app";
    side_more: "続けて聞く", "Ask more";
    side_send: "聞く", "Ask";
    side_bring: "このやり取りを入力欄に引用する", "Quote this in the input";
    title_write: "モデルに題名を付けさせる", "Have the model title it";
    set_auto_title: "名前のない会話に、モデルが題名を付ける（最初の実行のあと）", "The model titles unnamed conversations (after their first run)";
    set_notify_done: "実行が終わったとき", "When a run ends";
    set_notify_wait: "答えを待っているとき", "When a run waits for an answer";
    set_sound: "通知で音を鳴らす", "Play a sound with notifications";
    set_editor: "ファイルを開くエディタ", "Editor files open in";
    editor_ph: "空欄ならVS Codeか既定のアプリ（例: zed、subl）", "Empty: VS Code or the system's (e.g. zed, subl)";
    edit_start: "編集", "Edit";
    edit_save: "保存", "Save";
    edit_cancel: "やめる", "Cancel";
    edit_saved: "保存しました", "Saved";
    edit_changed: "読んだあとにファイルが書き換えられています。やめてから開き直してください", "The file changed since it was read: cancel and open it again";
    edit_unsaved: "編集中のファイルを保存するか、やめてからにしてください", "Save or cancel the file being edited first";
    edit_hint: "{mod}Sで保存します", "{mod}S saves";
    preview_on: "プレビュー", "Preview";
    preview_off: "ソース", "Source";
    grep_names: "名前", "Names";
    grep_text: "中身", "Contents";
    grep_ph: "ファイルの中身を探す（Enterで検索）", "Search what files hold (Enter)";
    grep_busy: "探しています…", "Searching…";
    grep_none: "見つかりませんでした", "Nothing found";
    grep_more: "{n}件まで表示しています", "Showing the first {n}";
    c_scratch: "一時的な会話を始める（離れると消えます）", "New scratch conversation (gone once left)";
    scratch_tag: "一時的", "Scratch";
    c_about: "このアプリについて・ログ", "About and log";
    c_review: "変更のレビューを頼む", "Ask for a review of the changes";
    c_instructions: "このフォルダのAGENTS.mdを開く", "Open this folder's AGENTS.md";
    about_title: "このアプリについて", "About";
    about_unknown: "不明", "unknown";
    about_program: "CLI", "CLI";
    about_folder: "フォルダ", "Folder";
    about_home: "設定と会話の保存先", "Settings and conversations";
    about_system: "システム", "System";
    about_log: "実行が標準エラーに書いたこと（新しいものが下）", "What runs wrote on stderr (newest last)";
    about_log_none: "まだ何もありません", "Nothing yet";
    about_copy: "まとめてコピー", "Copy all";
    about_open_home: "保存先を開く", "Open the folder";
    about_old: "CLIが古いため、バージョンがわかりません", "The CLI is too old to say its version";
    review_ask: "レビューを頼む", "Ask for a review";
    review_prompt: "まだコミットしていない変更（git statusとgit diffで見られます）をレビューしてください。バグ、扱いの抜け、足りないテストを、重いものから順に、ファイルと行を添えて挙げてください。直すのは、私が頼むまで待ってください。", "Review the uncommitted changes (see git status and git diff). List bugs, unhandled cases and missing tests, most serious first, each with its file and line. Wait for me before fixing anything.";
    instr_head: "指示ファイル（AGENTS.md）", "Instructions (AGENTS.md)";
    instr_none: "このフォルダで読まれる指示ファイルはありません", "No instruction file applies to this folder";
    instr_make: "このフォルダに作る", "Make one here";
    instr_global: "全体の指示を開く", "Open the global one";
    k_back: "前に見ていた会話へ戻る", "Back to the conversation left";
    k_forward: "戻る前の会話へ進む", "Forward again";
    k_pin: "ピン留めする・外す", "Pin or unpin";
    k_ask: "脇の質問をする", "Ask a side question";
    k_model: "モデルの欄へ", "To the model field";
    k_effort: "推論の深さを次へ", "Next effort";
    k_permission: "権限モードを次へ", "Next permission mode";
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
