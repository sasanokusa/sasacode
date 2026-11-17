//! progress-notes (`packages/bundled/src/progress-notes.ts`): the text a model writes beside its tool
//! calls is all the user sees of its work, and most models write none unless told (in the session
//! logs, Qwen3.8 Flash Next wrote text with 3% of 740 tool-call replies, DeepSeek V4.1 Flash with
//! 15%). The system prompt asks for short progress lines, and after `remindAfter` tool-call replies
//! in a row without text a reminder follows the tool results. It stays in the history: a note on one
//! request only made the next request differ from it, so a local server read everything after it
//! again (and a chat template that keeps reasoning only after the last user turn, as Qwen's do,
//! changed the history before it too).
//!
//! settings: { remindAfter?: number (default 5, 0 = no reminder) }

use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;

use sasacode_ai::{AssistantContent, UserContent};
use sasacode_host::harness_note;
use sasacode_plugin_api::{Lang, PluginApi, SystemPromptResult, TurnEndResult, hook};

use crate::repair::js::{js_trim, num_setting};

/// The language of a prompt, named when its script tells it. Asked for "the user's language" in an
/// English system prompt, DeepSeek V4.1 Flash wrote its progress lines in English to a Japanese
/// user, and its final answer drifted into Chinese.
pub fn prompt_language(text: &str) -> Option<&'static str> {
    let any = |lo: char, hi: char| text.chars().any(|c| (lo..=hi).contains(&c));
    if any('\u{3040}', '\u{30ff}') {
        Some("Japanese")
    } else if any('\u{ac00}', '\u{d7af}') {
        Some("Korean")
    } else if any('\u{4e00}', '\u{9fff}') {
        Some("Chinese")
    } else {
        None
    }
}

fn guidance(language: &str) -> String {
    [
        "Keep the user informed while you work. They see the text you write, not your reasoning and not every tool's output:".to_string(),
        "- Before your first tool call, say in one sentence what you are going to do.".to_string(),
        "- Then, beside your tool calls, write a short line (one or two sentences) when you find something that matters, finish a step, or change direction.".to_string(),
        format!("- Keep these lines brief and write them, like your final answer, in {language}. Do not describe every call."),
    ]
    .join("\n")
}

#[derive(Default)]
struct State {
    /// Replies of the main agent in a row that called tools and said nothing.
    silent: f64,
    /// From the user's latest prompt that tells it; a short "ok" keeps the one before.
    language: Option<&'static str>,
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let remind_after = num_setting(&api.settings(), "remindAfter", 5.0);
        let state: Arc<Mutex<State>> = Arc::default();
        let lock = |s: &Arc<Mutex<State>>| -> State {
            let g = s.lock().unwrap_or_else(|e| e.into_inner());
            State {
                silent: g.silent,
                language: g.language,
            }
        };

        // Subagents report to the model that started them, not to the user.
        let st = state.clone();
        api.on::<hook::SystemPrompt, _, _>(move |ev, ctx| {
            let out = ctx.agent.is_none().then(|| {
                let named = match lock(&st).language {
                    Some(l) => format!("{l}, the language the user writes in"),
                    None => "the language the user writes in".to_string(),
                };
                SystemPromptResult {
                    prompt: Some(format!("{}\n\n{}", ev.prompt, guidance(&named))),
                }
            });
            async move { Ok(out) }
        });

        let st = state.clone();
        let a = api.clone();
        api.on::<hook::UserPrompt, _, _>(move |ev, _c| {
            let text: String = ev.content.iter().filter_map(UserContent::as_text).collect();
            let said = prompt_language(&text);
            let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
            s.silent = 0.0;
            // Kanji alone (続行, 了解) is Japanese as much as Chinese: it keeps the language already
            // known, and is Japanese in a Japanese interface (#91).
            s.language = if said == Some("Chinese") {
                s.language.or(Some(if a.ui().lang() == Lang::Ja {
                    "Japanese"
                } else {
                    "Chinese"
                }))
            } else {
                said.or(s.language)
            };
            async { Ok(None) }
        });

        let st = state;
        api.on::<hook::TurnEnd, _, _>(move |ev, ctx| {
            let out = if ctx.agent.is_some() {
                None
            } else {
                let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
                let content = &ev.message.content;
                let said = content.iter().any(|b| matches!(b, AssistantContent::Text(t) if !js_trim(&t.text).is_empty()));
                let calls = content.iter().any(|b| matches!(b, AssistantContent::ToolCall(_)));
                s.silent = if said || !calls { 0.0 } else { s.silent + 1.0 };
                if remind_after == 0.0 || s.silent < remind_after {
                    None
                } else {
                    let replies = s.silent;
                    s.silent = 0.0;
                    Some(TurnEndResult {
                        inject: Some(harness_note(&format!(
                            "Your last {replies} replies were tool calls with no text, so the user cannot tell what you are doing. Keep working, and with your next tool call write one or two sentences in {}: what you have found so far and what you are doing next.",
                            s.language.unwrap_or("the user's language")
                        ))),
                        stop: None,
                    })
                }
            };
            async move { Ok(out) }
        });
        Ok(())
    })
}
