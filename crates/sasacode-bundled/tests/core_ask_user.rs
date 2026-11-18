//! plugins/ask-user/index.test.ts. The fake front ends of the TS (`fakeUI`, a host before API 1.10;
//! `modernUI`, one with free text and multiple choice) are `AskUi` implementations.

mod support_core;

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use sasacode_bundled::ask_user::{self, Args, AskUi, ask};
use sasacode_plugin_api::{InputOptions, Lang, SelectOption, ToolResult};
use support_core::*;

fn take<T>(q: &Mutex<VecDeque<Option<T>>>) -> Option<T> {
    q.lock().unwrap_or_else(|e| e.into_inner()).pop_front().flatten()
}

/// `fakeUI`: `select` only, as hosts before API 1.10 had.
struct FakeUi {
    picks: Mutex<VecDeque<Option<String>>>,
    lang: Lang,
    shown: Mutex<Vec<(String, Vec<SelectOption>)>>,
}

fn fake_ui(picks: &[Option<&str>], lang: Lang) -> FakeUi {
    FakeUi {
        picks: Mutex::new(picks.iter().map(|p| p.map(str::to_string)).collect()),
        lang,
        shown: Mutex::default(),
    }
}

impl FakeUi {
    fn shown(&self) -> Vec<(String, Vec<SelectOption>)> {
        self.shown.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl AskUi for FakeUi {
    fn interactive(&self) -> bool {
        true
    }
    fn lang(&self) -> Lang {
        self.lang
    }
    async fn select(&self, title: &str, options: &[SelectOption]) -> Option<String> {
        self.shown
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((title.to_string(), options.to_vec()));
        take(&self.picks)
    }
}

/// `modernUI`: select, selectMany and input (API 1.10).
struct ModernUi {
    picks: Mutex<VecDeque<Option<String>>>,
    many: Mutex<VecDeque<Option<Vec<String>>>>,
    typed: Mutex<VecDeque<Option<String>>>,
    seen: Mutex<Vec<String>>,
}

fn modern_ui(picks: &[Option<&str>], many: Vec<Option<Vec<&str>>>, typed: &[Option<&str>]) -> ModernUi {
    ModernUi {
        picks: Mutex::new(picks.iter().map(|p| p.map(str::to_string)).collect()),
        many: Mutex::new(
            many.into_iter()
                .map(|m| m.map(|v| v.into_iter().map(str::to_string).collect()))
                .collect(),
        ),
        typed: Mutex::new(typed.iter().map(|p| p.map(str::to_string)).collect()),
        seen: Mutex::default(),
    }
}

impl ModernUi {
    fn see(&self, s: String) {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).push(s);
    }
    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl AskUi for ModernUi {
    fn interactive(&self) -> bool {
        true
    }
    fn lang(&self) -> Lang {
        Lang::En
    }
    async fn select(&self, _title: &str, options: &[SelectOption]) -> Option<String> {
        self.see(format!(
            "select {}",
            options.last().map(|o| o.label.as_str()).unwrap_or("")
        ));
        take(&self.picks)
    }
    fn has_input(&self) -> bool {
        true
    }
    fn has_select_many(&self) -> bool {
        true
    }
    async fn input(&self, _title: &str, _options: &InputOptions) -> Option<String> {
        self.see("input".into());
        take(&self.typed)
    }
    async fn select_many(&self, _title: &str, _options: &[SelectOption]) -> Option<Vec<String>> {
        self.see("selectMany".into());
        take(&self.many)
    }
}

/// A picker that never closes.
struct OpenUi;

#[async_trait]
impl AskUi for OpenUi {
    fn interactive(&self) -> bool {
        true
    }
    fn lang(&self) -> Lang {
        Lang::En
    }
    async fn select(&self, _title: &str, _options: &[SelectOption]) -> Option<String> {
        std::future::pending().await
    }
}

fn body(r: &ToolResult) -> String {
    r.content.first().and_then(|c| c.as_text()).unwrap_or("").to_string()
}

fn q(question: &str, labels: &[&str]) -> Value {
    json!({ "question": question, "options": labels.iter().map(|l| json!({ "label": l })).collect::<Vec<_>>() })
}

fn multi(question: &str, labels: &[&str]) -> Value {
    let mut v = q(question, labels);
    v["multiSelect"] = json!(true);
    v
}

fn args(questions: Vec<Value>) -> Args {
    serde_json::from_value(json!({ "questions": questions })).unwrap()
}

fn answers(r: &ToolResult) -> Value {
    r.details
        .as_ref()
        .and_then(|d| d.get("answers"))
        .cloned()
        .unwrap_or(Value::Null)
}

async fn run(ui: &dyn AskUi, questions: Vec<Value>) -> ToolResult {
    ask(ui, &args(questions), &CancellationToken::new()).await
}

#[tokio::test]
async fn asks_each_question_in_turn_and_reports_the_chosen_labels() {
    let ui = fake_ui(&[Some("1"), Some("0")], Lang::En);
    let r = run(
        &ui,
        vec![q("Which DB?", &["SQLite", "Postgres"]), q("ORM?", &["None", "Drizzle"])],
    )
    .await;
    let shown = ui.shown();
    assert_eq!(
        shown.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(),
        vec!["Question 1/2\nWhich DB?", "Question 2/2\nORM?"]
    );
    assert_eq!(shown[0].1.last().unwrap().label, "Other (answer in chat)");
    assert_eq!(body(&r), "Q: Which DB?\nA: Postgres\n\nQ: ORM?\nA: None");
    assert_eq!(r.is_error, None);
}

#[tokio::test]
async fn duplicate_labels_are_told_apart_by_position() {
    let ui = fake_ui(&[Some("1")], Lang::En);
    let r = run(
        &ui,
        vec![json!({ "question": "Pick", "options": [{ "label": "A", "description": "first" }, { "label": "A", "description": "second" }] })],
    )
    .await;
    assert_eq!(answers(&r)[0]["answer"], json!(["A"]));
    assert!(body(&r).contains("A: A"));
}

#[tokio::test]
async fn choosing_other_stops_and_tells_the_model_to_wait_for_the_users_words() {
    let ui = fake_ui(&[Some("0"), Some("\0other")], Lang::Ja);
    let r = run(
        &ui,
        vec![q("One?", &["x", "y"]), q("Two?", &["x", "y"]), q("Three?", &["x", "y"])],
    )
    .await;
    let shown = ui.shown();
    assert_eq!(shown.len(), 2);
    assert_eq!(shown[0].1.last().unwrap().label, "その他（チャットで答える）");
    let b = body(&r);
    assert!(b.contains("A: x"), "{b}");
    assert!(b.contains("End your turn now"), "{b}");
    assert!(b.contains("1 later question(s) were not asked"), "{b}");
}

#[tokio::test]
async fn a_dismissed_question_is_not_answered_for_the_user() {
    let ui = fake_ui(&[None], Lang::En);
    let r = run(&ui, vec![q("One?", &["x", "y"])]).await;
    assert!(body(&r).contains("dismissed"));
    assert_eq!(answers(&r), json!([{ "question": "One?", "outcome": "dismissed" }]));
}

#[tokio::test]
async fn an_interrupt_ends_the_call_while_the_picker_is_open() {
    let signal = CancellationToken::new();
    let (s, a) = (signal.clone(), args(vec![q("One?", &["x", "y"])]));
    let pending = tokio::spawn(async move { ask(&OpenUi, &a, &s).await });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    signal.cancel();
    let r = pending.await.unwrap();
    assert_eq!(r.is_error, Some(true));
}

#[tokio::test]
async fn runs_without_an_approval_prompt_and_says_so_when_no_user_is_there() {
    let cwd = path_str(&std::env::current_dir().unwrap());
    let h = load_plugin("ask-user", ask_user::plugin, &cwd, json!({})).await;
    let r = h
        .invoke("ask_user", json!({ "questions": [q("One?", &["x", "y"])] }))
        .await;
    assert!(!r.is_error());
    assert!(body(&r).contains("No user is available"), "{}", body(&r));
}

#[tokio::test]
async fn the_schema_rejects_a_question_with_a_single_option() {
    let cwd = path_str(&std::env::current_dir().unwrap());
    let h = load_plugin("ask-user", ask_user::plugin, &cwd, json!({})).await;
    let r = h.invoke("ask_user", json!({ "questions": [q("One?", &["x"])] })).await;
    assert_eq!(r.is_error, Some(true));
}

#[tokio::test]
async fn with_api_1_10_other_asks_for_the_answer_right_there() {
    let ui = modern_ui(&[Some("\0other"), Some("1")], vec![], &[Some("  MySQL 8  ")]);
    let r = run(
        &ui,
        vec![q("Which DB?", &["SQLite", "Postgres"]), q("ORM?", &["None", "Drizzle"])],
    )
    .await;
    assert_eq!(
        ui.seen(),
        vec![
            "select Other (type an answer)",
            "input",
            "select Other (type an answer)"
        ]
    );
    assert_eq!(
        body(&r),
        "Q: Which DB?\nA: (in the user's own words) MySQL 8\n\nQ: ORM?\nA: Drizzle"
    );
}

#[tokio::test]
async fn an_empty_or_cancelled_free_text_answer_counts_as_dismissed() {
    for typed in [Some(""), None] {
        let ui = modern_ui(&[Some("\0other")], vec![], &[typed]);
        let r = run(&ui, vec![q("One?", &["x", "y"]), q("Two?", &["x", "y"])]).await;
        let b = body(&r);
        assert!(b.contains("dismissed"), "{b}");
        assert!(b.contains("1 later question(s) were not asked"), "{b}");
    }
}

#[tokio::test]
async fn multi_select_checks_several_options_with_other_among_them() {
    let ui = modern_ui(&[], vec![Some(vec!["0", "2", "\0other"])], &[Some("Svelte")]);
    let r = run(&ui, vec![multi("Which frameworks?", &["React", "Vue", "Solid"])]).await;
    assert_eq!(ui.seen(), vec!["selectMany", "input"]);
    assert_eq!(
        body(&r),
        "Q: Which frameworks?\nA: React; Solid; (in the user's own words) Svelte"
    );
}

#[tokio::test]
async fn confirming_no_options_in_a_multi_select_is_a_dismissal() {
    let ui = modern_ui(&[], vec![Some(vec![])], &[]);
    let r = run(&ui, vec![multi("Which?", &["a", "b"])]).await;
    assert!(body(&r).contains("dismissed"));
}

#[tokio::test]
async fn multi_select_falls_back_to_one_choice_on_a_host_before_1_10() {
    let ui = fake_ui(&[Some("1")], Lang::En);
    let r = run(&ui, vec![multi("Which?", &["a", "b"])]).await;
    assert_eq!(ui.shown().len(), 1);
    assert_eq!(body(&r), "Q: Which?\nA: b");
}
