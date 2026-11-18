//! loop-guard (`packages/bundled/src/loop-guard.ts`): catches the loops a text-repetition check
//! cannot see, and ends runs that stopped making progress:
//! - the same tool calls with the same results over and over (one call, or a short cycle like
//!   read A → read B → read A …): the model is told on the `noteAt`-th repeat; from the
//!   `blockAt`-th, every call of that loop is refused until a file changes or the user writes again;
//! - the same error again and again (calls rejected before running count too);
//! - the same call twice in one response with nothing in between that could change its result;
//! - `noProgressTurns` turns in a row in which no tool call ran: the run is stopped (turn_end stop).
//!   The core leaves such decisions to plugins (principle A6).
//!
//! settings: { noteAt?: number (default 3), blockAt?: number (default 5), noProgressTurns?: number (default 5, 0 = off) }

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use futures::future::BoxFuture;
use serde_json::Value;

use sasacode_ai::UserContent;
use sasacode_plugin_api::{
    Decision, PluginApi, ToolCallResult, ToolKind, ToolResult, ToolResultResult, TurnEndResult, hook,
};

use crate::repair::js::num_setting;

#[derive(Debug, Clone)]
struct Step {
    call: String,
    result: String,
    is_error: bool,
    /// The call changed files, so repeating earlier calls afterwards is legitimate progress.
    changes: bool,
}

/// Stable text for comparing calls and results (key order does not matter).
fn stable(v: &Value) -> String {
    match v {
        Value::Array(a) => format!("[{}]", a.iter().map(stable).collect::<Vec<_>>().join(",")),
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            // JS sort(): UTF-16 code unit order.
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            let parts: Vec<String> = keys
                .iter()
                .map(|k| format!("{}:{}", sasacode_ai::json::stringify(k), stable(&o[*k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        _ => sasacode_ai::json::stringify(v),
    }
}

fn result_text(r: &ToolResult) -> String {
    r.content
        .iter()
        .map(|c| match c {
            UserContent::Text(t) => t.text.clone(),
            UserContent::Image(i) => format!("[{}]", i.media_type),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Longest cycle looked for: A A A …, A B A B …, A B C A B C ….
const MAX_PERIOD: usize = 3;

/// How many times the last `period` steps occur back to back at the end of `history` (same calls,
/// same results, no file changed in between). 1 means no repeat.
fn repeats(history: &[Step], period: usize) -> usize {
    let n = history.len();
    if n < period {
        return 0;
    }
    let mut same = 0;
    let mut i = n - 1;
    while i >= period {
        let (a, b) = (&history[i], &history[i - period]);
        if a.changes || b.changes || a.call != b.call || a.result != b.result {
            break;
        }
        same += 1;
        i -= 1;
    }
    1 + same / period
}

/// Consecutive identical errors at the end of `history`, stopping at a file change.
fn error_streak(history: &[Step], result: &str) -> usize {
    let mut n = 0;
    for s in history.iter().rev() {
        if s.is_error && s.result == result {
            n += 1;
        } else if s.changes {
            break;
        }
    }
    n
}

/// Per agent (ctx.agent): parallel subagents share these hooks but not their conversations.
#[derive(Default)]
struct AgentState {
    history: Vec<Step>,
    /// Calls in the current response since the last one that could change state.
    this_turn: HashSet<String>,
    /// The calls of a loop that was stopped: refused until something changes.
    locked: HashSet<String>,
}

#[derive(Default)]
struct State {
    states: HashMap<String, AgentState>,
    /// Calls this plugin refused: their results say so and must not break a detected cycle.
    refused: HashSet<String>,
    /// For the no-progress check: calls answered this turn, how many of them ran, turns without any.
    calls: u32,
    ran: u32,
    idle: u32,
}

fn lock(s: &Mutex<State>) -> MutexGuard<'_, State> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

fn deny(refused: &mut HashSet<String>, id: &str, reason: String) -> Option<ToolCallResult> {
    refused.insert(id.to_string());
    Some(ToolCallResult {
        args: None,
        decision: Some(Decision::Deny),
        reason: Some(format!("loop-guard: {reason}")),
    })
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let note_at = num_setting(&settings, "noteAt", 3.0);
        let block_at = num_setting(&settings, "blockAt", 5.0);
        let no_progress_turns = num_setting(&settings, "noProgressTurns", 5.0);
        let state: Arc<Mutex<State>> = Arc::default();

        let st = state.clone();
        api.on::<hook::UserPrompt, _, _>(move |_e, _c| {
            let mut s = lock(&st);
            s.states.clear();
            s.idle = 0;
            async { Ok(None) }
        });
        // Fires for every response (subagents' too, unlike turn_end), before its calls are checked.
        let st = state.clone();
        api.on::<hook::AssistantMessage, _, _>(move |_e, ctx| {
            lock(&st)
                .states
                .entry(ctx.agent.unwrap_or_default())
                .or_default()
                .this_turn = HashSet::new();
            async { Ok(None) }
        });

        let st = state.clone();
        api.on::<hook::ToolCall, _, _>(move |ev, ctx| {
            let out = {
                let mut guard = lock(&st);
                let State { states, refused, .. } = &mut *guard;
                let s = states.entry(ctx.agent.unwrap_or_default()).or_default();
                let name = &ev.call.name;
                let sig = format!("{name}:{}", stable(&Value::Object(ev.args.clone())));
                if s.locked.contains(&sig) {
                    deny(
                        refused,
                        &ev.call.id,
                        format!("this {name} call belongs to a loop that was stopped, and no file has changed since; change the approach"),
                    )
                } else if s.this_turn.contains(&sig) {
                    deny(
                        refused,
                        &ev.call.id,
                        format!("the same {name} call appears earlier in this response with nothing in between that could change its result; it is not run twice"),
                    )
                } else {
                    // Arbitrary effects remain unknown; edit tools reset checks only after a successful change.
                    if matches!(ev.tool.kind(), ToolKind::Exec | ToolKind::Other) {
                        s.this_turn = HashSet::new();
                    }
                    s.this_turn.insert(sig.clone());
                    let history = &s.history;
                    let mut out = None;
                    for p in 1..=MAX_PERIOD.min(history.len()) {
                        if history[history.len() - p].call != sig {
                            continue;
                        }
                        let n = repeats(history, p);
                        if ((n + 1) as f64) < block_at {
                            continue;
                        }
                        s.locked = history[history.len() - p..].iter().map(|x| x.call.clone()).collect();
                        out = deny(
                            refused,
                            &ev.call.id,
                            if p == 1 {
                                format!("the same {name} call returned the same result {n} times in a row; change the approach")
                            } else {
                                format!("the same {p} calls have repeated {n} times with the same results; change the approach")
                            },
                        );
                        break;
                    }
                    out
                }
            };
            async move { Ok(out) }
        });

        let st = state.clone();
        let a = api.clone();
        api.on::<hook::ToolResult, _, _>(move |ev, ctx| {
            let tool_kind = a.agent().tools().iter().find(|t| t.name() == ev.call.name).map(|t| t.kind());
            let out = {
                let mut guard = lock(&st);
                // turn_end (the no-progress check) is the main agent's only.
                if ctx.agent.is_none() {
                    guard.calls += 1;
                    if ev.ran {
                        guard.ran += 1;
                    }
                }
                let key = ctx.agent.clone().unwrap_or_default();
                guard.states.entry(key.clone()).or_default();
                if guard.refused.remove(&ev.call.id) {
                    None
                } else {
                    let result = &ev.result;
                    let changed = match result.details.as_ref().and_then(|d| d.get("changed")) {
                        Some(Value::Bool(b)) => *b,
                        _ => tool_kind == Some(ToolKind::Edit),
                    };
                    let step = Step {
                        call: format!("{}:{}", ev.call.name, stable(&Value::Object(ev.call.input.clone()))),
                        result: result_text(result),
                        is_error: result.is_error(),
                        // Built-ins report actual changes; preserve the edit-kind contract for older plugins.
                        changes: ev.ran && !result.is_error() && changed,
                    };
                    // A file changed (by any agent): stopped loops may make progress again.
                    if step.changes {
                        for other in guard.states.values_mut() {
                            *other = AgentState::default();
                        }
                    }
                    let s = guard.states.entry(key).or_default();
                    if step.changes {
                        s.this_turn.insert(step.call.clone());
                    }
                    let same_error = if step.is_error { error_streak(&s.history, &step.result) + 1 } else { 0 };
                    s.history.push(step);
                    if s.history.len() > 50 {
                        let cut = s.history.len() - 50;
                        s.history.drain(..cut);
                    }
                    let mut cycle = 0;
                    let mut period = 1;
                    for p in 1..=MAX_PERIOD {
                        if cycle != 0 {
                            break;
                        }
                        let n = repeats(&s.history, p);
                        if n as f64 >= note_at {
                            (cycle, period) = (n, p);
                        }
                    }
                    let name = &ev.call.name;
                    let note = if cycle != 0 {
                        Some(if period == 1 {
                            format!("[harness] This is call {cycle} of {name} with the same arguments and the same result, with no file changed in between. Repeating it will not help; try a different approach.")
                        } else {
                            format!("[harness] The same {period} tool calls have now repeated {cycle} times with the same results, with no file changed in between. Repeating them will not help; try a different approach.")
                        })
                    } else if same_error as f64 >= note_at {
                        Some(format!("[harness] The same error has now occurred {same_error} times in a row. Try a different approach instead of repeating it."))
                    } else {
                        None
                    };
                    note.map(|note| {
                        let mut result = ev.result.clone();
                        result.content.push(UserContent::text(note));
                        ToolResultResult { result: Some(result) }
                    })
                }
            };
            async move { Ok(out) }
        });

        let st = state;
        api.on::<hook::TurnEnd, _, _>(move |_e, _c| {
            let mut s = lock(&st);
            if s.calls != 0 {
                s.idle = if s.ran != 0 { 0 } else { s.idle + 1 };
            }
            s.calls = 0;
            s.ran = 0;
            let out = (no_progress_turns != 0.0 && f64::from(s.idle) >= no_progress_turns).then(|| TurnEndResult {
                inject: None,
                stop: Some(format!("{} turns in a row in which no tool call could run", s.idle)),
            });
            async move { Ok(out) }
        });
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stable_sorts_keys_at_every_level() {
        assert_eq!(
            stable(&json!({"b": 1, "a": [{"d": 2.5, "c": null}]})),
            r#"{"a":[{"c":null,"d":2.5}],"b":1}"#
        );
    }
}
