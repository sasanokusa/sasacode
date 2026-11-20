//! clean-retry (opt-in via `plugins.enabled`): when repeated test failures or consecutive edit
//! errors indicate a small model is stuck in a broken state, restores snapshotted files modified
//! during the current run, trims the failed history back to before the task started, and restarts
//! with a short failure summary.
//!
//! settings: { maxRetries?: number (1), triggerFailures?: number (3), triggerEditErrors?: number (4), maxFileKB?: number (1024) }

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde_json::Value;
use sha2::{Digest, Sha256};

use sasacode_ai::UserContent;
use sasacode_plugin_api::{PluginApi, ToolCallEvent, ToolKind, ToolResultEvent, TurnEndResult, hook};
use sasacode_tools::util::resolve_path;

use crate::checkpoints::{Change, restore_changes, snapshot};
use crate::repair::js::{num_setting, utf16_prefix};

type FileDigest = Option<Option<[u8; 32]>>; // None: too large/unreadable; Some(None): missing

fn digest_of(path: &str, max_kb: f64) -> FileDigest {
    let p = Path::new(path);
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() && (m.len() as f64) <= max_kb * 1024.0 => {
            std::fs::read(p).ok().map(|b| Some(Sha256::digest(&b).into()))
        }
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(None),
        Err(_) => None,
    }
}

/// A file an edit is about to touch, and its snapshot when it existed: (path, (content, digest)).
type PendingSnapshot = (String, Option<(Change, FileDigest)>);

#[derive(Default)]
struct RunState {
    active: bool,
    original_prompt: String,
    base_len: usize,
    retries: u32,
    files: IndexMap<String, (Change, FileDigest, FileDigest)>, // (snapshot, init, last)
    unrestorable: IndexMap<String, String>,
    pending: HashMap<(String, String), Vec<PendingSnapshot>>,
    saw_non_check_bash: bool,
    test_failures: u32,
    consecutive_edit_errors: u32,
    failures: Vec<String>,
}

fn lock(m: &Mutex<RunState>) -> std::sync::MutexGuard<'_, RunState> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn text_of(content: &[UserContent]) -> String {
    content
        .iter()
        .filter_map(UserContent::as_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn push_failure(list: &mut Vec<String>, note: String) {
    let clipped = utf16_prefix(note.replace('\n', " ").trim(), 160);
    if !clipped.is_empty() && list.last().is_none_or(|prev| prev != &clipped) {
        list.push(clipped);
        if list.len() > 6 {
            list.remove(0);
        }
    }
}

fn detect_external_edits(s: &mut RunState, max_kb: f64) {
    let moved: Vec<_> = s
        .files
        .iter()
        .filter(|(p, (_, _, last))| digest_of(p, max_kb) != *last)
        .map(|(p, _)| p.clone())
        .collect();
    for path in moved {
        s.files.shift_remove(&path);
        s.unrestorable
            .entry(path)
            .or_insert_with(|| "modified externally since last edit".into());
    }
}

pub fn looks_like_check_command(command: &str) -> bool {
    let is_env = |w: &str| {
        w.split_once('=').is_some_and(|(k, _)| {
            !k.is_empty()
                && !k.as_bytes()[0].is_ascii_digit()
                && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
    };
    command
        .split("&&")
        .flat_map(|s| s.split("||"))
        .flat_map(|s| s.split(';'))
        .flat_map(|s| s.split('|'))
        .flat_map(|s| s.split('\n'))
        .any(|seg| {
            let mut words = seg
                .split_whitespace()
                .skip_while(|w| is_env(w) || matches!(*w, "env" | "time" | "sudo"));
            let Some(first) = words.next() else { return false };
            let prog = first.rsplit(['/', '\\']).next().unwrap_or(first).to_ascii_lowercase();
            if prog == "cd" {
                return false;
            }
            let second = words.next().map(str::to_ascii_lowercase);
            let third = words.next().map(str::to_ascii_lowercase);
            matches!(
                (prog.as_str(), second.as_deref(), third.as_deref()),
                ("cargo", Some("test" | "check" | "build" | "clippy" | "nextest"), _)
                    | ("npm" | "pnpm" | "yarn" | "bun", Some("test"), _)
                    | (
                        "npm" | "pnpm" | "yarn" | "bun",
                        Some("run"),
                        Some("test" | "lint" | "check" | "typecheck" | "build"),
                    )
                    | (
                        "pytest" | "make" | "tsc" | "jest" | "vitest" | "ruff" | "mypy" | "eslint",
                        _,
                        _
                    )
                    | ("python" | "python3", Some("-m"), Some("pytest" | "mypy" | "unittest"))
                    | ("go", Some("test" | "vet" | "build"), _)
                    | ("deno", Some("test" | "check"), _)
            )
        })
}

fn on_tool_call(s: &mut RunState, ev: &ToolCallEvent, agent: String, cwd: &Path, max_kb: f64) {
    if ev.tool.kind() != ToolKind::Edit {
        return;
    }
    detect_external_edits(s, max_kb);
    let paths = ev.tool.paths(&ev.args, cwd).unwrap_or_default();
    let staged = paths
        .into_iter()
        .filter_map(|raw| {
            let abs = resolve_path(&raw, cwd);
            (!s.unrestorable.contains_key(&abs)).then(|| {
                let snap = (!s.files.contains_key(&abs)).then(|| (snapshot(&abs, max_kb), digest_of(&abs, max_kb)));
                (abs, snap)
            })
        })
        .collect();
    s.pending.insert((agent, ev.call.id.clone()), staged);
}

fn on_tool_result(s: &mut RunState, ev: &ToolResultEvent, agent: String, max_kb: f64) {
    let staged = s.pending.remove(&(agent, ev.call.id.clone()));
    if !ev.ran {
        return;
    }
    let text = text_of(&ev.result.content);
    let unchanged = ev.result.details.as_ref().and_then(|d| d.get("changed")) == Some(&Value::Bool(false));
    for (path, snap) in staged.unwrap_or_default() {
        if s.unrestorable.contains_key(&path) {
            continue;
        }
        match snap {
            Some((ch, _)) if ch.skipped() => {
                if !ev.result.is_error() && !unchanged {
                    s.unrestorable
                        .entry(path)
                        .or_insert_with(|| "not snapshotted (too large or unreadable)".into());
                }
            }
            Some((ch, init)) => {
                let last = digest_of(&path, max_kb);
                if last != init {
                    s.files.entry(path).or_insert((ch, init, last));
                }
            }
            None => {
                if let Some(entry) = s.files.get_mut(&path) {
                    entry.2 = digest_of(&path, max_kb);
                }
            }
        }
    }
    let mut record_check = |label: String, fallback: &str| {
        if ev.result.is_error() {
            s.test_failures += 1;
            let tail = text.lines().rfind(|l| !l.trim().is_empty()).unwrap_or(fallback);
            push_failure(&mut s.failures, format!("{label}: {tail}"));
        } else {
            s.test_failures = 0;
        }
    };
    match ev.call.name.as_str() {
        "edit" | "write" | "hash_edit" => {
            if ev.result.is_error() || text.contains("[syntax-check] Syntax error") {
                s.consecutive_edit_errors += 1;
                let target = ev.call.input.get("path").and_then(Value::as_str).unwrap_or("file");
                let head = text.lines().next().unwrap_or("edit failed");
                push_failure(&mut s.failures, format!("{}({target}): {head}", ev.call.name));
            } else {
                s.consecutive_edit_errors = 0;
            }
        }
        "project_check" => record_check("project_check".into(), "check failed"),
        "bash" => {
            let cmd = ev.call.input.get("command").and_then(Value::as_str).unwrap_or("");
            if looks_like_check_command(cmd) {
                record_check(format!("bash({})", utf16_prefix(cmd, 40)), "command failed");
            } else if !ev.result.is_error() {
                s.saw_non_check_bash = true;
            }
        }
        _ => {}
    }
}

fn perform_retry(
    s: &mut RunState,
    api: &PluginApi,
    limits: (u32, u32, u32, f64),
) -> anyhow::Result<Option<TurnEndResult>> {
    let (max_retries, trigger_failures, trigger_edit_errors, max_kb) = limits;
    let triggered = s.test_failures >= trigger_failures || s.consecutive_edit_errors >= trigger_edit_errors;
    if !triggered || s.retries >= max_retries || s.original_prompt.trim().is_empty() {
        return Ok(None);
    }
    detect_external_edits(s, max_kb);
    if s.files.is_empty()
        && s.unrestorable.is_empty()
        && !s.saw_non_check_bash
        && s.consecutive_edit_errors < trigger_edit_errors
    {
        return Ok(None);
    }
    s.retries += 1;
    s.test_failures = 0;
    s.consecutive_edit_errors = 0;
    let tracked: Vec<_> = s.files.drain(..).map(|(_, v)| v).collect();
    let (mut unrestorable, saw_bash, failures) = (
        std::mem::take(&mut s.unrestorable),
        std::mem::take(&mut s.saw_non_check_bash),
        std::mem::take(&mut s.failures),
    );
    let changes: Vec<Change> = tracked.iter().map(|(c, _, _)| c.clone()).collect();
    if !changes.is_empty() {
        let _ = restore_changes(&changes);
        for (ch, init, _) in &tracked {
            if digest_of(&ch.path, max_kb) != *init {
                unrestorable
                    .entry(ch.path.clone())
                    .or_insert_with(|| "rollback failed".into());
            }
        }
    }
    let msgs = api.session().messages();
    api.session()
        .replace_messages(msgs.into_iter().take(s.base_len).collect())?;

    let bullets = if failures.is_empty() {
        "- Previous attempts failed repeatedly.".into()
    } else {
        failures.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")
    };
    let summary = if unrestorable.is_empty() && !saw_bash {
        "All file changes were rolled back to a clean state after repeated failures.".into()
    } else {
        let mut parts =
            vec!["Snapshotted file changes were rolled back after repeated failures, except as noted below:".into()];
        if !unrestorable.is_empty() {
            let list = unrestorable
                .iter()
                .map(|(p, w)| format!("- {p} ({w})"))
                .collect::<Vec<_>>()
                .join("\n");
            parts.push(format!("Files not restored:\n{list}"));
        }
        if saw_bash {
            parts.push("Changes made through bash were not rolled back.".into());
        }
        parts.join("\n")
    };
    let prompt = format!(
        "{}\n\n[clean-retry] {summary}\nPrevious attempt errors:\n{bullets}\nStart fresh with a clean minimal approach and verify with tests.",
        s.original_prompt
    );
    Ok(Some(TurnEndResult {
        inject: Some(prompt),
        stop: None,
    }))
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let limits = (
            num_setting(&settings, "maxRetries", 1.0).max(0.0) as u32,
            num_setting(&settings, "triggerFailures", 3.0).max(1.0) as u32,
            num_setting(&settings, "triggerEditErrors", 4.0).max(1.0) as u32,
            num_setting(&settings, "maxFileKB", 1024.0).max(1.0),
        );
        let cwd = api.cwd();
        let state: Arc<Mutex<RunState>> = Arc::default();

        let (st, a) = (state.clone(), api.clone());
        api.on::<hook::UserPrompt, _, _>(move |ev, ctx| {
            if ctx.agent.is_none() && !lock(&st).active {
                let text = text_of(&ev.content);
                let base_len = a.session().messages().len();
                let mut s = lock(&st);
                if !s.active {
                    *s = RunState {
                        active: true,
                        original_prompt: text,
                        base_len,
                        ..RunState::default()
                    };
                }
            }
            async { Ok(None) }
        });
        let st = state.clone();
        api.on::<hook::SessionStart, _, _>(move |_, _| {
            *lock(&st) = RunState::default();
            async { Ok(None) }
        });
        let st = state.clone();
        api.on::<hook::BeforeRequest, _, _>(move |_, ctx| {
            if ctx.agent.is_none() {
                lock(&st).active = true;
            }
            async { Ok(None) }
        });
        let st = state.clone();
        api.on::<hook::AgentEnd, _, _>(move |_, ctx| {
            if ctx.agent.is_none() {
                *lock(&st) = RunState::default();
            }
            async { Ok(None) }
        });
        let st = state.clone();
        api.on::<hook::ToolCall, _, _>(move |ev, ctx| {
            on_tool_call(&mut lock(&st), &ev, ctx.agent.unwrap_or_default(), &cwd, limits.3);
            async { Ok(None) }
        });
        let st = state.clone();
        api.on::<hook::ToolResult, _, _>(move |ev, ctx| {
            on_tool_result(&mut lock(&st), &ev, ctx.agent.unwrap_or_default(), limits.3);
            async { Ok(None) }
        });
        let a = api.clone();
        api.on::<hook::TurnEnd, _, _>(move |_ev, ctx| {
            let res = if ctx.agent.is_some() {
                Ok(None)
            } else {
                perform_retry(&mut lock(&state), &a, limits)
            };
            async move { res }
        });
        Ok(())
    })
}
