//! action-nudge: rescues small models that declare an intent to run a tool ("Let me check...",
//! "確認します。") and stop the turn without emitting any tool call.
//!
//! settings: { maxNudges?: number (default 2), maxChars?: number (default 1200) }

use std::sync::{Arc, Mutex, OnceLock};

use futures::future::BoxFuture;
use regex::Regex;

use sasacode_ai::StopReason;
use sasacode_plugin_api::{PluginApi, TurnEndResult, hook};

use crate::repair::js::num_setting;

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

const NUDGE_MSG: &str = "[action-nudge] You stated an action or tool input without actually calling a tool. Call the appropriate tool right now instead of describing it.";

fn looks_like_question(text: &str) -> bool {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .is_some_and(|l| l.ends_with('?') || l.ends_with('？'))
}

fn looks_like_completion(text: &str) -> bool {
    static DONE_JA: OnceLock<Regex> = OnceLock::new();
    static DONE_EN: OnceLock<Regex> = OnceLock::new();
    re(
        &DONE_JA,
        r"(?:修正しました|直しました|変更しました|完了しました|解決しました|テスト(?:は|が)(?:すべて)?通|問題(?:は|が)ありません)",
    )
    .is_match(text)
        || re(
            &DONE_EN,
            r"(?i)\b(?:all tests pass(?:ed)?|tests (?:now )?pass|i have (?:fixed|updated|resolved|completed)|has been (?:fixed|resolved|updated)|fix(?:ed)? and verified)\b",
        )
        .is_match(text)
}

fn looks_like_raw_tool_json(text: &str) -> bool {
    static RAW: OnceLock<Regex> = OnceLock::new();
    re(
        &RAW,
        r#"\{\s*"(?:path|command|old_string|new_string|query|pattern|start|questions)"\s*:"#,
    )
    .is_match(text)
}

fn last_sentence(text: &str) -> &str {
    let trimmed = text.trim();
    trimmed
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(trimmed)
}

pub(crate) fn looks_like_intent(text: &str, had_tool_calls: bool, last_tool_errored: bool) -> bool {
    static JA_END: OnceLock<Regex> = OnceLock::new();
    static JA_ANY: OnceLock<Regex> = OnceLock::new();
    static EN_INTENT: OnceLock<Regex> = OnceLock::new();

    let target = if had_tool_calls && !last_tool_errored {
        if text.chars().count() > 400 {
            return false;
        }
        last_sentence(text)
    } else {
        text.trim()
    };

    let ja_end = re(
        &JA_END,
        r"(?:確認|調査|調べ|実行|試行|試|読み|読|開|見|探|修正|直し|編集|更新|チェック|検査)(?:し|いたし|てみ|ていき)ます[。.!！…:\s]*$",
    );
    let ja_any = re(
        &JA_ANY,
        r"(?:まず|最初に|これから|次に)[^\n。]{0,40}(?:確認|調べ|調査|実行|読み|開|見てみ|修正|探し)(?:し|てみ)ます",
    );
    let en_intent = re(
        &EN_INTENT,
        r"(?im)(?:^|[.!?]\s+|\n)\s*(?:(?:ok(?:ay)?|sure|alright)[,.]?\s*)?(?:first,?\s*|next,?\s*)?(?:let me|let's|i(?:'ll| will| am going to|'m going to))\s+(?:now\s+|first\s+|start by\s+)?(?:check|read|inspect|run|execute|look(?:\s+at)?|search|find|open|examine|investigate|view|list|grep|edit|fix|modify|update|write|test|verify|check(?:ing)?)\b[^?\n]{0,120}[.…:\s]*$",
    );

    ja_end.is_match(target) || (!had_tool_calls && ja_any.is_match(target)) || en_intent.is_match(target)
}

#[derive(Default)]
struct RunState {
    nudges: u32,
    had_tool_calls: bool,
    last_tool_errored: bool,
    stopped_timestamps: Vec<i64>,
}

fn lock(m: &Mutex<RunState>) -> std::sync::MutexGuard<'_, RunState> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let max_nudges = num_setting(&settings, "maxNudges", 2.0).max(0.0) as u32;
        let max_chars = num_setting(&settings, "maxChars", 1200.0).max(1.0) as usize;
        let state: Arc<Mutex<RunState>> = Arc::default();

        let st = state.clone();
        api.on::<hook::UserPrompt, _, _>(move |_e, ctx| {
            if ctx.agent.is_none() {
                *lock(&st) = RunState::default();
            }
            async { Ok(None) }
        });

        let st = state.clone();
        api.on::<hook::AssistantMessage, _, _>(move |ev, ctx| {
            if ctx.agent.is_none() && ev.stopped.is_some() {
                let mut s = lock(&st);
                s.stopped_timestamps.push(ev.message.timestamp);
                if s.stopped_timestamps.len() > 16 {
                    s.stopped_timestamps.remove(0);
                }
            }
            async { Ok(None) }
        });

        let st = state.clone();
        api.on::<hook::ToolResult, _, _>(move |ev, ctx| {
            if ctx.agent.is_none() {
                let mut s = lock(&st);
                s.had_tool_calls = true;
                s.last_tool_errored = ev.result.is_error();
            }
            async { Ok(None) }
        });

        api.on::<hook::TurnEnd, _, _>(move |ev, ctx| {
            let st = state.clone();
            async move {
                if ctx.agent.is_some()
                    || ev.message.tool_calls().next().is_some()
                    || ev.message.stop_reason != StopReason::Stop
                {
                    return Ok(None);
                }
                let text = ev.message.text();
                let trimmed = text.trim();
                if trimmed.is_empty() || trimmed.chars().count() > max_chars || looks_like_question(trimmed) {
                    return Ok(None);
                }
                let mut s = lock(&st);
                if s.nudges >= max_nudges || s.stopped_timestamps.contains(&ev.message.timestamp) {
                    return Ok(None);
                }
                if s.had_tool_calls && !s.last_tool_errored && looks_like_completion(trimmed) {
                    return Ok(None);
                }
                let raw_json = (!s.had_tool_calls || s.last_tool_errored) && looks_like_raw_tool_json(trimmed);
                let intent = looks_like_intent(trimmed, s.had_tool_calls, s.last_tool_errored);
                if !raw_json && !intent {
                    return Ok(None);
                }
                s.nudges += 1;
                Ok(Some(TurnEndResult {
                    inject: Some(NUDGE_MSG.to_string()),
                    stop: None,
                }))
            }
        });

        Ok(())
    })
}
