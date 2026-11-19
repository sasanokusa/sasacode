//! clean-retry (opt-in via `plugins.enabled`): when repeated test failures or consecutive edit
//! errors indicate a small model is stuck in a broken state, restores all files modified during the
//! current prompt back to their initial snapshots, trims the failed history back to before the task
//! started, and restarts with a short failure summary.
//!
//! settings: { maxRetries?: number (1), triggerFailures?: number (3), triggerEditErrors?: number (4), maxFileKB?: number (1024) }

use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde_json::Value;

use sasacode_ai::UserContent;
use sasacode_plugin_api::{PluginApi, ToolKind, TurnEndResult, hook};
use sasacode_tools::util::resolve_path;

use crate::checkpoints::{Change, restore_changes, snapshot};
use crate::repair::js::{num_setting, utf16_prefix};

#[derive(Default)]
struct RunState {
    original_prompt: String,
    base_len: usize,
    retries: u32,
    snapshots: IndexMap<String, Change>,
    test_failures: u32,
    consecutive_edit_errors: u32,
    failures: Vec<String>,
}

fn lock(m: &Mutex<RunState>) -> std::sync::MutexGuard<'_, RunState> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn push_failure(list: &mut Vec<String>, note: String) {
    let clean = note.replace('\n', " ");
    let clipped = utf16_prefix(clean.trim(), 160);
    if !clipped.is_empty() && list.last().is_none_or(|prev| prev != &clipped) {
        list.push(clipped);
        if list.len() > 6 {
            list.remove(0);
        }
    }
}

fn looks_like_check_command(command: &str) -> bool {
    let lower = command.to_lowercase();
    [
        "test",
        "spec",
        "check",
        "lint",
        "typecheck",
        "pytest",
        "vitest",
        "jest",
        "cargo ",
        "bun ",
        "npm ",
        "pnpm ",
        "yarn ",
        "go test",
        "make",
    ]
    .iter()
    .any(|k| lower.contains(k))
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let max_retries = num_setting(&settings, "maxRetries", 1.0).max(0.0) as u32;
        let trigger_failures = num_setting(&settings, "triggerFailures", 3.0).max(1.0) as u32;
        let trigger_edit_errors = num_setting(&settings, "triggerEditErrors", 4.0).max(1.0) as u32;
        let max_kb = num_setting(&settings, "maxFileKB", 1024.0).max(1.0);
        let cwd = api.cwd();
        let state: Arc<Mutex<RunState>> = Arc::default();

        let st = state.clone();
        let a = api.clone();
        api.on::<hook::UserPrompt, _, _>(move |ev, ctx| {
            if ctx.agent.is_none() {
                let text = ev
                    .content
                    .iter()
                    .filter_map(UserContent::as_text)
                    .collect::<Vec<_>>()
                    .join("\n");
                let base_len = a.session().messages().len();
                let mut s = lock(&st);
                *s = RunState {
                    original_prompt: text,
                    base_len,
                    ..RunState::default()
                };
            }
            async { Ok(None) }
        });

        let st = state.clone();
        let tc_cwd = cwd.clone();
        api.on::<hook::ToolCall, _, _>(move |ev, _ctx| {
            if ev.tool.kind() == ToolKind::Edit {
                let paths = ev.tool.paths(&ev.args, &tc_cwd).unwrap_or_default();
                let mut s = lock(&st);
                for raw in paths {
                    let abs = resolve_path(&raw, &tc_cwd);
                    s.snapshots.entry(abs.clone()).or_insert_with(|| snapshot(&abs, max_kb));
                }
            }
            async { Ok(None) }
        });

        let st = state.clone();
        api.on::<hook::ToolResult, _, _>(move |ev, _ctx| {
            let text: String = ev
                .result
                .content
                .iter()
                .filter_map(UserContent::as_text)
                .collect::<Vec<_>>()
                .join("\n");
            let mut s = lock(&st);
            match ev.call.name.as_str() {
                "edit" | "write" | "hash_edit" => {
                    let syntax_err = text.contains("[syntax-check] Syntax error");
                    if ev.result.is_error() || syntax_err {
                        s.consecutive_edit_errors += 1;
                        let target = ev.call.input.get("path").and_then(Value::as_str).unwrap_or("file");
                        let summary = text.lines().next().unwrap_or("edit failed");
                        push_failure(&mut s.failures, format!("{}({target}): {summary}", ev.call.name));
                    } else {
                        s.consecutive_edit_errors = 0;
                    }
                }
                "project_check" => {
                    if ev.result.is_error() {
                        s.test_failures += 1;
                        let summary = text.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("check failed");
                        push_failure(&mut s.failures, format!("project_check: {summary}"));
                    } else if ev.ran {
                        s.test_failures = 0;
                    }
                }
                "bash" => {
                    let cmd = ev.call.input.get("command").and_then(Value::as_str).unwrap_or("");
                    if looks_like_check_command(cmd) {
                        if ev.result.is_error() {
                            s.test_failures += 1;
                            let summary = text.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("command failed");
                            push_failure(&mut s.failures, format!("bash({}): {summary}", utf16_prefix(cmd, 40)));
                        } else if ev.ran {
                            s.test_failures = 0;
                        }
                    }
                }
                _ => {}
            }
            async { Ok(None) }
        });

        let st = state;
        let a = api.clone();
        api.on::<hook::TurnEnd, _, _>(move |_ev, ctx| {
            let st = st.clone();
            let a = a.clone();
            async move {
                if ctx.agent.is_some() {
                    return Ok(None);
                }
                let (should, original, base_len, changes, failures) = {
                    let mut s = lock(&st);
                    let triggered = s.test_failures >= trigger_failures
                        || s.consecutive_edit_errors >= trigger_edit_errors;
                    if !triggered
                        || s.retries >= max_retries
                        || s.original_prompt.trim().is_empty()
                        || s.snapshots.is_empty()
                    {
                        return Ok(None);
                    }
                    let restorable: Vec<Change> = s
                        .snapshots
                        .values()
                        .filter(|c| !c.skipped())
                        .cloned()
                        .collect();
                    if restorable.is_empty() {
                        return Ok(None);
                    }
                    s.retries += 1;
                    s.test_failures = 0;
                    s.consecutive_edit_errors = 0;
                    s.snapshots.clear();
                    let failures = std::mem::take(&mut s.failures);
                    (true, s.original_prompt.clone(), s.base_len, restorable, failures)
                };
                if !should {
                    return Ok(None);
                }
                let _ = restore_changes(&changes);
                let msgs = a.session().messages();
                let keep = base_len.min(msgs.len());
                a.session().replace_messages(msgs.into_iter().take(keep).collect())?;
                let bullets = if failures.is_empty() {
                    "- Previous attempts failed repeatedly.".to_string()
                } else {
                    failures.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")
                };
                let prompt = format!(
                    "{original}\n\n[clean-retry] All file changes were rolled back to a clean state after repeated failures. Previous attempt errors:\n{bullets}\nStart fresh with a clean minimal approach and verify with tests."
                );
                Ok(Some(TurnEndResult {
                    inject: Some(prompt),
                    stop: None,
                }))
            }
        });

        Ok(())
    })
}
