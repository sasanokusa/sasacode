//! ask-user (`plugins/ask-user/index.ts`): lets the model ask the user multiple-choice questions
//! instead of guessing. Off until `plugins.enabled` names it.

use async_trait::async_trait;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use sasacode_ai::JsonObject;
use sasacode_plugin_api::{
    InputOptions, Lang, PermissionRules, PluginApi, SelectManyOptions, SelectOption, ToolDef, ToolKind, ToolResult,
};

use crate::repair::js::js_trim;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub question: String,
    pub options: Vec<QuestionOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_select: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Args {
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Chosen,
    OwnWords,
    Dismissed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own: Option<String>,
    pub outcome: Outcome,
}

const OTHER: &str = "\0other";

struct Labels {
    other: &'static str,
    other_hint: &'static str,
    other_chat: &'static str,
    other_chat_hint: &'static str,
    count: fn(usize, usize) -> String,
    own: &'static str,
}

const JA: Labels = Labels {
    other: "その他（自分で答える）",
    other_hint: "選択肢にない答えを入力する",
    other_chat: "その他（チャットで答える）",
    other_chat_hint: "選択肢にない答えをチャットで伝える",
    count: |i, n| format!("質問 {i}/{n}"),
    own: "あなたの答え（空のまま enter で取り消し）",
};

const EN: Labels = Labels {
    other: "Other (type an answer)",
    other_hint: "Answer with something not listed",
    other_chat: "Other (answer in chat)",
    other_chat_hint: "Type an answer that is not listed in the chat",
    count: |i, n| format!("Question {i}/{n}"),
    own: "Your answer (enter on empty to cancel)",
};

/// The front end as `ask` sees it. The TS checked at run time whether the host had `input` and
/// `selectMany` (API 1.10); here a front end says so.
#[async_trait]
pub trait AskUi: Send + Sync {
    fn interactive(&self) -> bool;
    fn lang(&self) -> Lang;
    async fn select(&self, title: &str, options: &[SelectOption]) -> Option<String>;
    /// Free text (API 1.10 and later).
    fn has_input(&self) -> bool {
        false
    }
    /// Multiple choice (API 1.10 and later).
    fn has_select_many(&self) -> bool {
        false
    }
    async fn input(&self, _title: &str, _options: &InputOptions) -> Option<String> {
        None
    }
    async fn select_many(&self, _title: &str, _options: &[SelectOption]) -> Option<Vec<String>> {
        None
    }
}

/// The plugin's own front end: this host has free text and multiple choice.
struct ApiUi(PluginApi);

#[async_trait]
impl AskUi for ApiUi {
    fn interactive(&self) -> bool {
        self.0.ui().interactive()
    }
    fn lang(&self) -> Lang {
        self.0.ui().lang()
    }
    async fn select(&self, title: &str, options: &[SelectOption]) -> Option<String> {
        self.0.ui().select(title, options).await
    }
    fn has_input(&self) -> bool {
        true
    }
    fn has_select_many(&self) -> bool {
        true
    }
    async fn input(&self, title: &str, options: &InputOptions) -> Option<String> {
        self.0.ui().input(title, options).await
    }
    async fn select_many(&self, title: &str, options: &[SelectOption]) -> Option<Vec<String>> {
        self.0
            .ui()
            .select_many(title, options, &SelectManyOptions::default())
            .await
    }
}

/// None when the run is interrupted; the dialog itself stays with the host.
async fn until_aborted<T>(work: impl std::future::Future<Output = T>, signal: &CancellationToken) -> Option<T> {
    if signal.is_cancelled() {
        return None;
    }
    tokio::select! {
        v = work => Some(v),
        () = signal.cancelled() => None,
    }
}

fn details(answers: &[Answer]) -> JsonObject {
    let mut d = JsonObject::new();
    d.insert(
        "answers".into(),
        serde_json::to_value(answers).unwrap_or(Value::Array(Vec::new())),
    );
    d
}

pub async fn ask(ui: &dyn AskUi, args: &Args, signal: &CancellationToken) -> ToolResult {
    if !ui.interactive() {
        return ToolResult::text(
            "No user is available to answer (non-interactive run). Decide yourself, and state the assumption you made in your reply.",
        )
        .with_details(details(&[]));
    }
    let l = if ui.lang() == Lang::Ja { &JA } else { &EN };
    let typed = ui.has_input();
    let mut answers: Vec<Answer> = Vec::new();
    let interrupted = |answers: &[Answer]| {
        let mut r = ToolResult::error("Interrupted before the user answered.");
        r.details = Some(details(answers));
        r
    };
    let n = args.questions.len();
    for (i, q) in args.questions.iter().enumerate() {
        let title = if n > 1 {
            format!("{}\n{}", (l.count)(i + 1, n), q.question)
        } else {
            q.question.clone()
        };
        let mut options: Vec<SelectOption> = q
            .options
            .iter()
            .enumerate()
            .map(|(k, o)| SelectOption {
                value: k.to_string(),
                label: o.label.clone(),
                description: o.description.clone(),
            })
            .collect();
        options.push(if typed {
            SelectOption::new(OTHER, l.other).describe(l.other_hint)
        } else {
            SelectOption::new(OTHER, l.other_chat).describe(l.other_chat_hint)
        });
        let many = q.multi_select == Some(true) && ui.has_select_many();
        let picked: Option<Vec<String>> = if many {
            until_aborted(ui.select_many(&title, &options), signal).await.flatten()
        } else {
            until_aborted(ui.select(&title, &options), signal)
                .await
                .flatten()
                .map(|v| vec![v])
        };
        if signal.is_cancelled() {
            return interrupted(&answers);
        }
        let Some(picked) = picked.filter(|p| !(many && p.is_empty())) else {
            answers.push(Answer {
                question: q.question.clone(),
                answer: None,
                own: None,
                outcome: Outcome::Dismissed,
            });
            break;
        };
        let chosen: Vec<String> = picked
            .iter()
            .filter(|v| *v != OTHER)
            .filter_map(|v| {
                v.parse::<usize>()
                    .ok()
                    .and_then(|k| q.options.get(k))
                    .map(|o| o.label.clone())
            })
            .collect();
        if !picked.iter().any(|v| v == OTHER) {
            answers.push(Answer {
                question: q.question.clone(),
                answer: Some(chosen),
                own: None,
                outcome: Outcome::Chosen,
            });
            continue;
        }
        if !typed {
            answers.push(Answer {
                question: q.question.clone(),
                answer: Some(chosen),
                own: None,
                outcome: Outcome::OwnWords,
            });
            break;
        }
        let own = until_aborted(
            ui.input(
                &title,
                &InputOptions {
                    message: Some(l.own.to_string()),
                    ..InputOptions::default()
                },
            ),
            signal,
        )
        .await
        .flatten()
        .map(|s| js_trim(&s).to_string());
        if signal.is_cancelled() {
            return interrupted(&answers);
        }
        let Some(own) = own.filter(|s| !s.is_empty()) else {
            answers.push(Answer {
                question: q.question.clone(),
                answer: None,
                own: None,
                outcome: Outcome::Dismissed,
            });
            break;
        };
        answers.push(Answer {
            question: q.question.clone(),
            answer: Some(chosen),
            own: Some(own),
            outcome: Outcome::Chosen,
        });
    }
    ToolResult::text(report(&answers, n)).with_details(details(&answers))
}

pub fn report(answers: &[Answer], asked: usize) -> String {
    let mut lines: Vec<String> = answers
        .iter()
        .filter(|a| a.outcome == Outcome::Chosen)
        .map(|a| {
            let mut parts: Vec<String> = a.answer.clone().unwrap_or_default();
            if let Some(own) = a.own.as_ref().filter(|o| !o.is_empty()) {
                parts.push(format!("(in the user's own words) {own}"));
            }
            format!("Q: {}\nA: {}", a.question, parts.join("; "))
        })
        .collect();
    match answers.last() {
        Some(last) if last.outcome == Outcome::OwnWords => {
            let so_far = match last.answer.as_ref().filter(|a| !a.is_empty()) {
                Some(a) => format!("Chosen so far: {}. ", a.join("; ")),
                None => String::new(),
            };
            lines.push(format!(
                "Q: {}\n{so_far}The user wants to answer in their own words. End your turn now with a short message and wait for their reply; do not guess the answer.",
                last.question
            ));
        }
        Some(last) if last.outcome == Outcome::Dismissed => lines.push(format!(
            "Q: {}\nThe user dismissed the question. Do not assume an answer: say what you need and wait, or continue only with work that does not depend on it.",
            last.question
        )),
        _ => {}
    }
    if answers.len() < asked {
        lines.push(format!("{} later question(s) were not asked.", asked - answers.len()));
    }
    lines.join("\n\n")
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let a = api.clone();
        api.register_tool(
            ToolDef::new(
                "ask_user",
                "Ask the user one to four multiple-choice questions and wait for the answers. Use it when a decision is genuinely the user's (requirements, preferences, trade-offs) and cannot be settled from the request, the code or a sensible default; not for confirming routine steps or asking permission. Give 2-6 distinct options per question, recommended one first; set multiSelect when several can apply. The user can always answer in their own words instead.",
                json!({
                    "type": "object",
                    "properties": {
                        "questions": {
                            "type": "array", "minItems": 1, "maxItems": 4,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "question": { "type": "string", "minLength": 1, "maxLength": 500, "description": "The full question, ending with a question mark." },
                                    "multiSelect": { "type": "boolean", "description": "Let the user pick several options (not mutually exclusive choices)." },
                                    "options": {
                                        "type": "array", "minItems": 2, "maxItems": 6,
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "label": { "type": "string", "minLength": 1, "maxLength": 80, "description": "Short answer, 1-5 words." },
                                                "description": { "type": "string", "maxLength": 200, "description": "What choosing this means." },
                                            },
                                            "required": ["label"], "additionalProperties": false,
                                        },
                                    },
                                },
                                "required": ["question", "options"], "additionalProperties": false,
                            },
                        },
                    },
                    "required": ["questions"], "additionalProperties": false,
                }),
                ToolKind::Other,
                move |args, ctx| {
                    let a = a.clone();
                    async move {
                        let args: Args = serde_json::from_value(Value::Object(args))?;
                        Ok(ask(&ApiUi(a), &args, &ctx.signal).await)
                    }
                },
            )
            .summary(|a| {
                a.get("questions")
                    .and_then(|q| q.get(0))
                    .and_then(|q| q.get("question"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            })
            .into_ref(),
        );
        // Asking whether it may ask would defeat the tool. A user's own deny or ask rule still wins.
        api.add_permission_rules(PermissionRules {
            allow: vec!["ask_user".into()],
            ..PermissionRules::default()
        })?;
        Ok(())
    })
}
