//! clean-retry (opt-in via `plugins.enabled`): when repeated test failures or consecutive edit
//! errors indicate a small model is stuck in a broken state, restores snapshotted files modified
//! during the current run, trims the failed history back to before the task started, and restarts
//! with a short failure summary.
//!
//! A run is tracked from the main agent's first request to its agent_end. The task is read from
//! the history at that first request, not from user_prompt: a prompt that another plugin handled
//! starts no run and leaves nothing behind, and a run started from queued input has its prompt too.
//!
//! settings: { maxRetries?: number (1), triggerFailures?: number (3), triggerEditErrors?: number (4), maxFileKB?: number (1024) }

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde_json::Value;
use sha2::{Digest, Sha256};

use sasacode_ai::{Message, UserContent};
use sasacode_plugin_api::{PluginApi, ToolCallEvent, ToolKind, ToolResultEvent, TurnEndResult, hook};
use sasacode_tools::util::resolve_path;

use crate::checkpoints::{Change, restore_changes, snapshot};
use crate::repair::js::{num_setting, utf16_prefix};

#[derive(Clone, Copy)]
struct Limits {
    max_retries: u32,
    trigger_failures: u32,
    trigger_edit_errors: u32,
    max_kb: f64,
}

type FileDigest = Option<Option<[u8; 32]>>; // None: too large/unreadable; Some(None): missing

fn digest_of(path: &str, max_kb: f64) -> FileDigest {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() && (m.len() as f64) <= max_kb * 1024.0 => {
            std::fs::read(path).ok().map(|b| Some(Sha256::digest(&b).into()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(None),
        _ => None,
    }
}

/// A file an edit is about to touch, and its snapshot when it existed: (path, (content, digest)).
type PendingSnapshot = (String, Option<(Change, FileDigest)>);

#[derive(Default)]
struct RunState {
    /// A main-agent run is in progress: its first request was seen, its agent_end was not.
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

/// The state of a run about to send its first request. Its task is the user messages at the end
/// of the history: the prompt that started it (several when they queued up while the previous run
/// was ending), or none when the run continues without new input.
fn begin_run(history: &[Message]) -> RunState {
    let mut prompts = Vec::new();
    for message in history.iter().rev() {
        let Message::User(user) = message else { break };
        prompts.insert(0, text_of(&user.content));
    }
    RunState {
        active: true,
        base_len: history.len() - prompts.len(),
        original_prompt: prompts.join("\n\n"),
        ..RunState::default()
    }
}

fn lock(m: &Mutex<RunState>) -> std::sync::MutexGuard<'_, RunState> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn text_of(content: &[UserContent]) -> String {
    let texts: Vec<&str> = content.iter().filter_map(UserContent::as_text).collect();
    texts.join("\n")
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

/// The first reason a file cannot be rolled back is the one reported.
fn mark_unrestorable(s: &mut RunState, path: String, why: &str) {
    s.unrestorable.entry(path).or_insert_with(|| why.to_string());
}

/// A tracked file that no longer is as the last edit left it was changed by someone else: leave it alone.
fn detect_external_edits(s: &mut RunState, max_kb: f64) {
    let paths: Vec<String> = s.files.keys().cloned().collect();
    for path in paths {
        if digest_of(&path, max_kb) != s.files[&path].2 {
            s.files.shift_remove(&path);
            mark_unrestorable(s, path, "modified externally since last edit");
        }
    }
}

// ── bash commands: which ones are checks ─────────────────────────────

/// What one part of a bash command (between `&&`, `||`, `;`, `|` or newlines) is. `Ignored`: empty,
/// `cd`, or only env assignments. `Check`: tests, a build, a linter or a type checker.
#[derive(Clone, Copy, PartialEq)]
enum Segment {
    Ignored,
    Check,
    Other,
}

fn is_env_assignment(word: &str) -> bool {
    let name = word.split_once('=').map_or("", |(name, _)| name);
    let starts_like_a_name = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_');
    starts_like_a_name && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A package script that tests, lints, type-checks or builds, by its name (`test:unit`, not `lint:fix`).
fn is_check_script(name: &str) -> bool {
    let known = matches!(name, "test" | "lint" | "check" | "typecheck" | "type-check" | "build")
        || name.starts_with("test:")
        || name.starts_with("lint:");
    known && !name.ends_with(":fix")
}

/// `make` with no target, or only with targets that check. The value of a flag (`-C dir`) reads as
/// a target: such a command is not taken for a check.
fn make_only_checks(args: &[&str]) -> bool {
    let is_target = |arg: &&str| !arg.starts_with('-') && !arg.contains('>') && !is_env_assignment(arg);
    let mut targets = args.iter().copied().filter(is_target);
    targets.all(|target| matches!(target, "test" | "check" | "lint" | "build" | "all"))
}

/// Whether `program args` only tests, builds, lints or type-checks. A command asked to fix files is not a check.
fn is_check(program: &str, args: &[&str]) -> bool {
    // `--fix`, `--fix=…`, `--fix-only`; not pytest's `--fixtures`.
    let asks_to_fix = |arg: &&str| *arg == "--fix" || arg.starts_with("--fix=") || arg.starts_with("--fix-");
    if args.iter().any(asks_to_fix) {
        return false;
    }
    // The subcommand: the first argument that is not a flag or a cargo toolchain (`+nightly`).
    let mut positional = args.iter().filter(|arg| !arg.starts_with(['-', '+']));
    let sub = positional.next().copied().unwrap_or("");
    match program {
        "cargo" => matches!(sub, "test" | "check" | "build" | "clippy" | "nextest"),
        "npm" | "bun" | "pnpm" | "yarn" => match args {
            ["run", script, ..] => is_check_script(script),
            // pnpm and yarn run a script without `run`.
            [script, ..] => *script == "test" || (matches!(program, "pnpm" | "yarn") && is_check_script(script)),
            [] => false,
        },
        "make" => make_only_checks(args),
        "ruff" => sub == "check",
        "go" => matches!(sub, "test" | "vet" | "build"),
        "deno" => matches!(sub, "test" | "check"),
        "pytest" | "tsc" | "jest" | "vitest" | "mypy" | "eslint" => true,
        // `python`, `python3`, `python3.12`.
        _ => program.starts_with("python") && matches!(args, ["-m", "pytest" | "mypy" | "unittest", ..]),
    }
}

fn classify_segment(segment: &str) -> Segment {
    let lower = segment.to_ascii_lowercase();
    let all_words: Vec<&str> = lower.split_whitespace().collect();
    // Drop what stands before the program: env assignments, wrappers and runners, and their flags.
    let mut words = all_words.as_slice();
    loop {
        words = match words {
            [first, rest @ ..] if is_env_assignment(first) || first.starts_with('-') => rest,
            ["env" | "time" | "sudo" | "npx", rest @ ..] => rest,
            ["pnpm", "exec", rest @ ..] | ["uv" | "poetry", "run", rest @ ..] => rest,
            _ => break,
        };
    }
    let program = words
        .first()
        .map_or("", |path| path.rsplit(['/', '\\']).next().unwrap_or(path));
    match program {
        "" | "cd" | "export" => Segment::Ignored,
        _ if is_check(program, &words[1..]) => Segment::Check,
        _ => Segment::Other,
    }
}

/// A command a pipe feeds to only read it (`cargo test 2>&1 | tail -n 20`): it changes no file.
fn only_reads_its_input(segment: &str) -> bool {
    let program = segment.split_whitespace().next().unwrap_or("");
    let reader = matches!(
        program,
        "tail" | "head" | "grep" | "rg" | "wc" | "sort" | "uniq" | "cut" | "cat" | "less"
    );
    reader && !segment.contains('>')
}

/// The parts of a command, classified. Quotes are not parsed: a separator inside a string splits too.
fn segments(command: &str) -> Vec<Segment> {
    let separated = command.replace("&&", ";").replace("||", ";");
    let pipelines = separated.split([';', '\n']);
    pipelines
        .flat_map(|pipeline| {
            pipeline.split('|').enumerate().map(|(i, segment)| match i {
                0 => classify_segment(segment),
                _ if only_reads_its_input(segment) => Segment::Ignored,
                _ => classify_segment(segment),
            })
        })
        .collect()
}

/// Some part of the command runs tests, a build, a linter or a type checker.
pub fn looks_like_check_command(command: &str) -> bool {
    segments(command).contains(&Segment::Check)
}

/// Some part of the command is not a check (nor `cd` or an env assignment), so it may have changed files.
pub fn runs_other_commands(command: &str) -> bool {
    segments(command).contains(&Segment::Other)
}

// ── tool calls and results ───────────────────────────────────────────

fn call_key(agent: Option<&str>, call_id: &str) -> (String, String) {
    (agent.unwrap_or_default().to_string(), call_id.to_string())
}

/// Before an edit runs: snapshot the files it names. A subagent's edits are part of the run too.
fn on_tool_call(s: &mut RunState, ev: &ToolCallEvent, agent: Option<&str>, cwd: &Path, max_kb: f64) {
    if !s.active || ev.tool.kind() != ToolKind::Edit {
        return;
    }
    detect_external_edits(s, max_kb);
    let mut staged = Vec::new();
    for raw in ev.tool.paths(&ev.args, cwd).unwrap_or_default() {
        let abs = resolve_path(&raw, cwd);
        if s.unrestorable.contains_key(&abs) {
            continue;
        }
        let snap = (!s.files.contains_key(&abs)).then(|| (snapshot(&abs, max_kb), digest_of(&abs, max_kb)));
        staged.push((abs, snap));
    }
    s.pending.insert(call_key(agent, &ev.call.id), staged);
}

/// After an edit ran: keep the snapshots of the files it changed.
fn track_edited_files(s: &mut RunState, staged: Vec<PendingSnapshot>, ev: &ToolResultEvent, max_kb: f64) {
    let unchanged = ev.result.details.as_ref().and_then(|d| d.get("changed")) == Some(&Value::Bool(false));
    for (path, snap) in staged {
        if s.unrestorable.contains_key(&path) {
            continue;
        }
        match snap {
            Some((ch, _)) if ch.skipped() => {
                if !ev.result.is_error() && !unchanged {
                    mark_unrestorable(s, path, "not snapshotted (too large or unreadable)");
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
}

fn record_check(s: &mut RunState, label: &str, failed: bool, output: &str) {
    s.test_failures = if failed { s.test_failures + 1 } else { 0 };
    if failed {
        let tail = output.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("failed");
        push_failure(&mut s.failures, format!("{label}: {tail}"));
    }
}

fn on_tool_result(s: &mut RunState, ev: &ToolResultEvent, agent: Option<&str>, max_kb: f64) {
    let staged = s.pending.remove(&call_key(agent, &ev.call.id));
    // Not counted: results that arrive after the run ended, and calls that never ran (denied).
    if !s.active || !ev.ran {
        return;
    }
    track_edited_files(s, staged.unwrap_or_default(), ev, max_kb);
    let failed = ev.result.is_error();
    let text = text_of(&ev.result.content);
    let name = ev.call.name.as_str();
    let command = ev.call.input.get("command").and_then(Value::as_str).unwrap_or("");
    let bash_check = name == "bash" && looks_like_check_command(command);
    // A check chained to something else ran that something even when it failed (`sed -i … && cargo test`).
    if name == "bash" && runs_other_commands(command) && (bash_check || !failed) {
        s.saw_non_check_bash = true;
    }
    // A subagent's failures are its own: they do not count toward retrying the main run.
    if agent.is_some() {
        return;
    }
    match name {
        "edit" | "write" | "hash_edit" => {
            if failed || text.contains("[syntax-check] Syntax error") {
                s.consecutive_edit_errors += 1;
                let target = ev.call.input.get("path").and_then(Value::as_str).unwrap_or("file");
                let head = text.lines().next().unwrap_or("edit failed");
                push_failure(&mut s.failures, format!("{name}({target}): {head}"));
            } else {
                s.consecutive_edit_errors = 0;
            }
        }
        "project_check" => record_check(s, "project_check", failed, &text),
        "bash" if bash_check => record_check(s, &format!("bash({})", utf16_prefix(command, 40)), failed, &text),
        _ => {}
    }
}

// ── the retry ────────────────────────────────────────────────────────

/// The prompt that restarts the task: the original one, what was not rolled back, and what failed.
fn retry_prompt(s: &RunState) -> String {
    let mut summary = if s.unrestorable.is_empty() && !s.saw_non_check_bash {
        "All file changes were rolled back to a clean state after repeated failures.".to_string()
    } else {
        "Snapshotted file changes were rolled back after repeated failures, except as noted below:".to_string()
    };
    if !s.unrestorable.is_empty() {
        summary.push_str("\nFiles not restored:");
        for (path, why) in &s.unrestorable {
            summary.push_str(&format!("\n- {path} ({why})"));
        }
    }
    if s.saw_non_check_bash {
        summary.push_str("\nChanges made through bash were not rolled back.");
    }
    let mut bullets: Vec<String> = s.failures.iter().map(|f| format!("- {f}")).collect();
    if bullets.is_empty() {
        bullets.push("- Previous attempts failed repeatedly.".into());
    }
    format!(
        "{}\n\n[clean-retry] {summary}\nPrevious attempt errors:\n{}\nStart fresh with a clean minimal approach and verify with tests.",
        s.original_prompt,
        bullets.join("\n")
    )
}

fn perform_retry(s: &mut RunState, api: &PluginApi, limits: Limits) -> anyhow::Result<Option<TurnEndResult>> {
    let edits_stuck = s.consecutive_edit_errors >= limits.trigger_edit_errors;
    let triggered = s.test_failures >= limits.trigger_failures || edits_stuck;
    if !s.active || !triggered || s.retries >= limits.max_retries || s.original_prompt.trim().is_empty() {
        return Ok(None);
    }
    detect_external_edits(s, limits.max_kb);
    let nothing_changed = s.files.is_empty() && s.unrestorable.is_empty() && !s.saw_non_check_bash;
    if nothing_changed && !edits_stuck {
        return Ok(None);
    }
    s.retries += 1;
    let tracked: Vec<_> = s.files.drain(..).map(|(_, v)| v).collect();
    let changes: Vec<Change> = tracked.iter().map(|(change, _, _)| change.clone()).collect();
    let _ = restore_changes(&changes);
    for (change, init, _) in tracked {
        if digest_of(&change.path, limits.max_kb) != init {
            mark_unrestorable(s, change.path, "rollback failed");
        }
    }
    let mut history = api.session().messages();
    history.truncate(s.base_len);
    api.session().replace_messages(history)?;

    let inject = Some(retry_prompt(s));
    // The next attempt starts with nothing held against it.
    s.test_failures = 0;
    s.consecutive_edit_errors = 0;
    s.saw_non_check_bash = false;
    s.unrestorable.clear();
    s.failures.clear();
    Ok(Some(TurnEndResult { inject, stop: None }))
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let limits = Limits {
            max_retries: num_setting(&settings, "maxRetries", 1.0).max(0.0) as u32,
            trigger_failures: num_setting(&settings, "triggerFailures", 3.0).max(1.0) as u32,
            trigger_edit_errors: num_setting(&settings, "triggerEditErrors", 4.0).max(1.0) as u32,
            max_kb: num_setting(&settings, "maxFileKB", 1024.0).max(1.0),
        };
        let cwd = api.cwd();
        let state: Arc<Mutex<RunState>> = Arc::default();

        // `ctx.agent` is set for a subagent. Its requests start no run of their own; its tool calls
        // and results belong to the main run that is in progress.
        let st = state.clone();
        api.on_sync::<hook::SessionStart, _>(move |_, _| {
            *lock(&st) = RunState::default();
            Ok(None)
        });
        let (st, a) = (state.clone(), api.clone());
        api.on_sync::<hook::BeforeRequest, _>(move |_, ctx| {
            let mut s = lock(&st);
            if ctx.agent.is_none() && !s.active {
                *s = begin_run(&a.session().messages());
            }
            Ok(None)
        });
        let st = state.clone();
        api.on_sync::<hook::AgentEnd, _>(move |_, ctx| {
            if ctx.agent.is_none() {
                *lock(&st) = RunState::default();
            }
            Ok(None)
        });
        let st = state.clone();
        api.on_sync::<hook::ToolCall, _>(move |ev, ctx| {
            on_tool_call(&mut lock(&st), ev, ctx.agent.as_deref(), &cwd, limits.max_kb);
            Ok(None)
        });
        let st = state.clone();
        api.on_sync::<hook::ToolResult, _>(move |ev, ctx| {
            on_tool_result(&mut lock(&st), ev, ctx.agent.as_deref(), limits.max_kb);
            Ok(None)
        });
        let a = api.clone();
        api.on_sync::<hook::TurnEnd, _>(move |_, ctx| match ctx.agent {
            None => perform_retry(&mut lock(&state), &a, limits),
            Some(_) => Ok(None),
        });
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_piped_into_a_reader_is_still_only_a_check() {
        for command in [
            "cargo test 2>&1 | tail -n 20",
            "cargo test 2>&1 | grep FAILED | head",
            "pytest -q | tail -5",
        ] {
            assert!(looks_like_check_command(command), "{command}");
            assert!(!runs_other_commands(command), "{command}");
        }
        // A reader first, or one that writes a file, may change something.
        for command in [
            "cargo test | tail -n 20 > out.txt",
            "cat a.txt | cargo test",
            "cargo test | tee log.txt",
        ] {
            assert!(runs_other_commands(command), "{command}");
        }
    }
}
