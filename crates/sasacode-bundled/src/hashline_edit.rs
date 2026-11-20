//! hashline-edit (opt-in via `plugins.enabled`): adds short line-number+hash anchors (`42:a3|...`)
//! to `read` results and provides `hash_edit` so small models can edit lines reliably without
//! copying exact whitespace indentation. Also strips accidental `N:hh|` prefixes sent to `write`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use futures::future::BoxFuture;
use regex::Regex;
use serde_json::{Value, json};

use sasacode_ai::{AssistantContent, JsonObject};
use sasacode_plugin_api::{
    PluginApi, SystemPromptResult, ToolCallRawResult, ToolCallResult, ToolContext, ToolDef, ToolKind, ToolResult,
    ToolResultResult, UserContent, hook,
};
use sasacode_tools::util::{line_diff, resolve_path, split_lines};

const MAX_DISK_HASH_BYTES: u64 = 2_000_000;
const EDIT_CONTEXT_LINES: usize = 2;
const MAX_TRACKED_FILES: usize = 64;
const MAX_TRACKED_SUBAGENTS: usize = 16;

#[derive(Default)]
struct PluginState {
    /// Per-agent snapshot/batch state (`None` = main agent, `Some("sub-N")` = subagent).
    agents: HashMap<Option<String>, AgentState>,
    /// Monotonic counter used to evict least recently used tracked files (and idle subagents).
    access_seq: u64,
    /// In-flight `hash_edit` invocations recorded in `ToolCall` so `execute_hash_edit`
    /// (which only receives `ToolContext`) knows which agent invoked it.
    pending_tool_calls: Vec<PendingToolCall>,
}

#[derive(Default)]
struct AgentState {
    /// Per-file snapshot/anchor tracking keyed by canonical path (or resolved path fallback).
    files: HashMap<String, FileState>,
    /// Tool call IDs still open in the current assistant response for this agent. While >1 call
    /// in the same batch remains, newly returned anchors from an earlier `hash_edit` stay pending
    /// until the batch or turn ends.
    active_batch_ids: HashSet<String>,
    last_used: u64,
}

#[derive(Clone)]
struct FileState {
    content_hash: u64,
    line_hashes: Vec<u64>,
    fresh_lines: HashSet<usize>,
    pending_fresh_lines: HashSet<usize>,
    last_used: u64,
}

struct PendingToolCall {
    agent: Option<String>,
    call_id: String,
    args: JsonObject,
}

impl PluginState {
    fn next_seq(&mut self) -> u64 {
        self.access_seq += 1;
        self.access_seq
    }

    fn agent_mut(&mut self, key: &Option<String>, seq: u64) -> &mut AgentState {
        if !self.agents.contains_key(key) && key.is_some() {
            let sub_count = self.agents.keys().filter(|k| k.is_some()).count();
            if sub_count >= MAX_TRACKED_SUBAGENTS {
                let victim = self
                    .agents
                    .iter()
                    .filter(|(k, a)| k.is_some() && a.active_batch_ids.is_empty())
                    .min_by_key(|(_, a)| a.last_used)
                    .or_else(|| {
                        self.agents
                            .iter()
                            .filter(|(k, _)| k.is_some())
                            .min_by_key(|(_, a)| a.last_used)
                    })
                    .map(|(k, _)| k.clone());
                if let Some(v) = victim {
                    self.agents.remove(&v);
                }
            }
        }
        let entry = self.agents.entry(key.clone()).or_default();
        entry.last_used = seq;
        entry
    }
}

fn upsert_file(files: &mut HashMap<String, FileState>, key: String, fs: FileState) {
    if !files.contains_key(&key)
        && files.len() >= MAX_TRACKED_FILES
        && let Some(victim) = files.iter().min_by_key(|(_, v)| v.last_used).map(|(k, _)| k.clone())
    {
        files.remove(&victim);
    }
    files.insert(key, fs);
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn line_hash_64(line: &str) -> u64 {
    let clean = line.strip_suffix('\r').unwrap_or(line);
    fnv1a_64(clean.as_bytes())
}

fn canonical_key(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

/// Compute a 2-hex-digit hash for a single source line (without trailing `\r` or `\n`).
pub fn line_hash(line: &str) -> String {
    let clean = line.strip_suffix('\r').unwrap_or(line);
    let mut h: u32 = 0x811c_9dc5;
    for &b in clean.as_bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    let folded = (h ^ (h >> 8) ^ (h >> 16) ^ (h >> 24)) & 0xff;
    format!("{folded:02x}")
}

fn read_line_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^ *(\d+)\t(.*)$").expect("valid regex"))
}

fn hashline_prefix_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\s*(\d+):([0-9a-fA-F]{2})\|(.*)$").expect("valid regex"))
}

fn transform_read_text(text: &str, disk_lines: Option<&[&str]>) -> Option<(String, Vec<usize>)> {
    let re = read_line_re();
    let mut matched = false;
    let mut seen_lines = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for line in text.split('\n') {
        if let Some(cap) = re.captures(line)
            && let Ok(n) = cap[1].parse::<usize>()
            && n >= 1
        {
            matched = true;
            seen_lines.push(n);
            let body = &cap[2];
            let raw = disk_lines.and_then(|d| d.get(n - 1).copied()).unwrap_or(body);
            let h = line_hash(raw);
            out.push(format!("{n}:{h}|{body}"));
        } else {
            out.push(line.to_string());
        }
    }
    matched.then(|| (out.join("\n"), seen_lines))
}

/// Strip sequential `N:hh|` prefixes when every non-empty line carries one (copied from
/// hashline-annotated `read` output) and line numbers advance consistently.
pub fn strip_hashline_prefixes(content: &str) -> Option<String> {
    let trimmed = content.strip_suffix('\n').unwrap_or(content);
    let rows: Vec<&str> = trimmed.split('\n').collect();
    if rows.is_empty() {
        return None;
    }
    let re = hashline_prefix_re();
    let mut expected: Option<usize> = None;
    let mut blank_streak: usize = 0;
    let mut body: Vec<&str> = Vec::with_capacity(rows.len());
    for row in rows {
        if row.trim().is_empty() {
            blank_streak += 1;
            body.push(row);
            continue;
        }
        let cap = re.captures(row)?;
        let n: usize = cap[1].parse().ok()?;
        match expected {
            None => expected = Some(n),
            Some(prev) if n == prev + 1 || (blank_streak > 0 && n == prev + 1 + blank_streak) => {
                expected = Some(n);
            }
            _ => return None,
        }
        blank_streak = 0;
        body.push(cap.get(3).map_or("", |m| m.as_str()));
    }
    expected?;
    Some(body.join("\n") + if content.ends_with('\n') { "\n" } else { "" })
}

fn parse_anchor(raw: &str) -> Result<(usize, String), String> {
    let head = raw.trim().split('|').next().unwrap_or("").trim();
    let Some((num_part, hash_part)) = head.split_once(':') else {
        return Err(format!(
            "Invalid anchor \"{raw}\": expected '<line>:<2-hex-hash>' (e.g. '42:a3')."
        ));
    };
    let line: usize = num_part
        .trim()
        .parse()
        .ok()
        .filter(|&n| n >= 1)
        .ok_or_else(|| format!("Invalid line number in anchor \"{raw}\" (must be >= 1)."))?;
    let hash = hash_part.trim().to_ascii_lowercase();
    if hash.len() != 2 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "Invalid hash in anchor \"{raw}\": expected 2 hex digits (e.g. '42:a3')."
        ));
    }
    Ok((line, hash))
}

fn format_lines_range(lines: &[String], from: usize, to: usize) -> String {
    (from..=to)
        .map(|i| format!("{}:{}|{}", i + 1, line_hash(&lines[i]), lines[i]))
        .collect::<Vec<_>>()
        .join("\n")
}

fn context_window_lines(lines: &[String], center: usize) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let idx = center.saturating_sub(1).min(lines.len() - 1);
    let from = idx.saturating_sub(3);
    let to = (idx + 3).min(lines.len() - 1);
    format_lines_range(lines, from, to)
}

fn is_prefix_unchanged(old: &[u64], new: &[u64], end_line: usize) -> bool {
    let prefix_end = end_line + 1;
    old.len() >= prefix_end && new.len() >= prefix_end && old[..prefix_end] == new[..prefix_end]
}

/// Reject edits that target stale or potentially shifted anchors.
///
/// Rule:
/// - If no prior `read`/`hash_edit` snapshot is recorded for this file, allow once line hashes match
///   (so standalone tool invocations still work while hash-mismatch errors avoid leaking unread files).
/// - If the disk content hash matches the recorded snapshot, both `start_line` and `end_line` must
///   belong to `fresh_lines` (lines returned by `read`, lines strictly before an earlier edit,
///   unshifted lines after a zero-delta replace, or lines from a completed `hash_edit` context snippet).
/// - If the file changed externally on disk since the last `read`/`hash_edit`, both endpoints must be
///   in `fresh_lines` AND every line through `end_line + 1` must be unchanged (the external edit
///   occurred strictly below the target). Otherwise require a re-read.
fn check_anchor_freshness(
    recorded: Option<&FileState>,
    path: &str,
    current_hash: u64,
    current_line_hashes: &[u64],
    start_line: usize,
    end_line: usize,
) -> Result<(), String> {
    let Some(fs) = recorded else {
        return Ok(());
    };
    if !fs.fresh_lines.contains(&start_line) || !fs.fresh_lines.contains(&end_line) {
        return Err(format!(
            "Stale anchor at {path} lines {start_line}-{end_line}: lines shifted since your last read or edit. Re-read {path} before editing."
        ));
    }
    if current_hash == fs.content_hash {
        return Ok(());
    }
    if is_prefix_unchanged(&fs.line_hashes, current_line_hashes, end_line) {
        return Ok(());
    }
    Err(format!(
        "File {path} changed since your last read and anchors at lines {start_line}-{end_line} may have shifted. Re-read {path} before editing."
    ))
}

fn record_read(
    state: &Arc<Mutex<PluginState>>,
    agent_key: &Option<String>,
    key: String,
    disk_text: &str,
    seen_lines: &[usize],
) {
    let content_hash = fnv1a_64(disk_text.as_bytes());
    let line_hashes: Vec<u64> = split_lines(disk_text).into_iter().map(line_hash_64).collect();
    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
    let seq = st.next_seq();
    let agent = st.agent_mut(agent_key, seq);
    match agent.files.get_mut(&key) {
        Some(fs) if fs.content_hash == content_hash => {
            fs.fresh_lines.extend(seen_lines.iter().copied());
            fs.last_used = seq;
        }
        _ => {
            upsert_file(
                &mut agent.files,
                key,
                FileState {
                    content_hash,
                    line_hashes,
                    fresh_lines: seen_lines.iter().copied().collect(),
                    pending_fresh_lines: HashSet::new(),
                    last_used: seq,
                },
            );
        }
    }
}

fn promote_pending_in(agent: &mut AgentState) {
    for fs in agent.files.values_mut() {
        if !fs.pending_fresh_lines.is_empty() {
            let pending = std::mem::take(&mut fs.pending_fresh_lines);
            fs.fresh_lines.extend(pending);
        }
    }
}

fn take_pending_agent(state: &Arc<Mutex<PluginState>>, args: &JsonObject) -> Option<String> {
    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pos) = st.pending_tool_calls.iter().position(|p| p.args == *args) {
        return st.pending_tool_calls.remove(pos).agent;
    }
    let raw_path = args.get("path");
    if raw_path.is_some()
        && let Some(pos) = st
            .pending_tool_calls
            .iter()
            .position(|p| p.args.get("path") == raw_path)
    {
        return st.pending_tool_calls.remove(pos).agent;
    }
    None
}

async fn execute_hash_edit(
    args: JsonObject,
    ctx: ToolContext,
    state: Arc<Mutex<PluginState>>,
) -> anyhow::Result<ToolResult> {
    let agent_key = take_pending_agent(&state, &args);
    let raw_path = args.get("path").and_then(Value::as_str).unwrap_or("");
    let path = resolve_path(raw_path, &ctx.cwd);
    let key = canonical_key(&path);
    let Ok(bytes) = tokio::fs::read(&path).await else {
        return Ok(ToolResult::error(format!(
            "File not found: {path}. Use write to create it."
        )));
    };
    let Ok(before) = String::from_utf8(bytes) else {
        return Ok(ToolResult::error(format!(
            "{path} is not valid UTF-8; edit it with bash instead."
        )));
    };
    if before.is_empty() {
        return Ok(ToolResult::error(format!("{path} is empty. Use write to populate it.")));
    }
    let start_raw = args.get("start").and_then(Value::as_str).unwrap_or("");
    let (start_line, start_hash) = match parse_anchor(start_raw) {
        Ok(v) => v,
        Err(msg) => return Ok(ToolResult::error(msg)),
    };
    let (end_line, end_hash) = match args.get("end").and_then(Value::as_str) {
        Some(end_raw) if !end_raw.trim().is_empty() => match parse_anchor(end_raw) {
            Ok(v) => v,
            Err(msg) => return Ok(ToolResult::error(msg)),
        },
        _ => (start_line, start_hash.clone()),
    };
    let mode = args.get("mode").and_then(Value::as_str).unwrap_or("replace");
    let content = args.get("content").and_then(Value::as_str).unwrap_or("");

    let has_crlf = before.contains("\r\n");
    let trailing_nl = before.ends_with('\n');
    let lines: Vec<String> = split_lines(&before)
        .into_iter()
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect();
    let recorded = {
        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
        let seq = st.next_seq();
        st.agents.get_mut(&agent_key).and_then(|a| {
            a.last_used = seq;
            a.files.get_mut(&key).map(|fs| {
                fs.last_used = seq;
                fs.clone()
            })
        })
    };
    let has_recorded = recorded.is_some();

    if start_line > lines.len() || end_line > lines.len() {
        let detail = if has_recorded {
            format!(" Near end:\n{}", context_window_lines(&lines, lines.len()))
        } else {
            format!(" Read {path} first.")
        };
        return Ok(ToolResult::error(format!(
            "Line out of range in {path} ({} lines total; got {start_line}..={end_line}).{detail}",
            lines.len(),
        )));
    }
    if end_line < start_line {
        return Ok(ToolResult::error(format!(
            "end line ({end_line}) must be >= start line ({start_line})."
        )));
    }

    let current_hash = fnv1a_64(before.as_bytes());
    let current_line_hashes: Vec<u64> = lines.iter().map(|l| line_hash_64(l)).collect();
    if let Err(msg) = check_anchor_freshness(
        recorded.as_ref(),
        &path,
        current_hash,
        &current_line_hashes,
        start_line,
        end_line,
    ) {
        return Ok(ToolResult::error(msg));
    }

    let actual_start = line_hash(&lines[start_line - 1]);
    if actual_start != start_hash {
        let msg = if has_recorded {
            format!(
                "Hash mismatch at {path} line {start_line}: expected {start_hash}, found {actual_start}. Current lines:\n{}",
                context_window_lines(&lines, start_line)
            )
        } else {
            format!("Hash mismatch at {path} line {start_line}. Read {path} first.")
        };
        return Ok(ToolResult::error(msg));
    }
    let actual_end = line_hash(&lines[end_line - 1]);
    if actual_end != end_hash {
        let msg = if has_recorded {
            format!(
                "Hash mismatch at {path} line {end_line}: expected {end_hash}, found {actual_end}. Current lines:\n{}",
                context_window_lines(&lines, end_line)
            )
        } else {
            format!("Hash mismatch at {path} line {end_line}. Read {path} first.")
        };
        return Ok(ToolResult::error(msg));
    }
    if mode != "replace" && content.is_empty() {
        return Ok(ToolResult::error(format!(
            "content must not be empty for mode \"{mode}\"."
        )));
    }

    let stripped = content.strip_suffix('\n').unwrap_or(content);
    let insert_lines: Vec<String> = if content.is_empty() {
        Vec::new()
    } else {
        stripped
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect()
    };

    let mut next_lines = Vec::with_capacity(lines.len() + insert_lines.len());
    match mode {
        "replace" => {
            next_lines.extend_from_slice(&lines[..start_line - 1]);
            next_lines.extend(insert_lines.iter().cloned());
            next_lines.extend_from_slice(&lines[end_line..]);
        }
        "insert_after" => {
            next_lines.extend_from_slice(&lines[..end_line]);
            next_lines.extend(insert_lines.iter().cloned());
            next_lines.extend_from_slice(&lines[end_line..]);
        }
        "insert_before" => {
            next_lines.extend_from_slice(&lines[..start_line - 1]);
            next_lines.extend(insert_lines.iter().cloned());
            next_lines.extend_from_slice(&lines[start_line - 1..]);
        }
        other => return Ok(ToolResult::error(format!("Unknown mode \"{other}\""))),
    }

    let sep = if has_crlf { "\r\n" } else { "\n" };
    let mut after = next_lines.join(sep);
    if trailing_nl && !next_lines.is_empty() {
        after.push_str(sep);
    }
    if after == before {
        return Ok(ToolResult::error(
            "No changes made (replacement matches existing lines).",
        ));
    }
    if ctx.signal.is_cancelled() {
        return Ok(ToolResult::error("Interrupted by the user; the file was not changed."));
    }
    tokio::fs::write(&path, &after)
        .await
        .map_err(|e| sasacode_tools::util::fs_error(&e, "open", &path))?;

    let (shown_range, region_msg) = if next_lines.is_empty() {
        (None, "File is now empty.".to_string())
    } else {
        let (first_idx, last_idx) = match mode {
            "insert_after" => (end_line, end_line + insert_lines.len() - 1),
            _ => {
                let first = start_line - 1;
                if insert_lines.is_empty() {
                    let clamped = first.min(next_lines.len() - 1);
                    (clamped, clamped)
                } else {
                    (first, first + insert_lines.len() - 1)
                }
            }
        };
        let from = first_idx.saturating_sub(EDIT_CONTEXT_LINES);
        let to = (last_idx + EDIT_CONTEXT_LINES).min(next_lines.len() - 1);
        let snippet = format_lines_range(&next_lines, from, to);
        (
            Some((from + 1, to + 1)),
            format!("Updated lines {}-{}:\n{snippet}", from + 1, to + 1),
        )
    };

    if after.len() as u64 <= MAX_DISK_HASH_BYTES {
        let delta = next_lines.len() as isize - lines.len() as isize;
        let unchanged_before_end = if mode == "insert_after" {
            end_line + 1
        } else {
            start_line
        };
        let new_content_hash = fnv1a_64(after.as_bytes());
        let new_line_hashes: Vec<u64> = next_lines.iter().map(|l| line_hash_64(l)).collect();
        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
        let seq = st.next_seq();
        let agent = st.agent_mut(&agent_key, seq);
        let mut next_fresh = HashSet::new();
        let mut next_pending = HashSet::new();
        if let Some(prev) = agent.files.get(&key) {
            let exact = prev.content_hash == current_hash;
            let prefix_ok = !exact && is_prefix_unchanged(&prev.line_hashes, &current_line_hashes, end_line);
            if exact || prefix_ok {
                let keep = |&n: &usize| n < unchanged_before_end || (exact && delta == 0 && n > end_line);
                next_fresh.extend(prev.fresh_lines.iter().copied().filter(keep));
                next_pending.extend(prev.pending_fresh_lines.iter().copied().filter(keep));
            }
        }
        let batch_pending = agent.active_batch_ids.len() > 1;
        if let Some((shown_from, shown_to)) = shown_range {
            if batch_pending {
                next_pending.extend(shown_from..=shown_to);
            } else {
                next_fresh.extend(shown_from..=shown_to);
            }
        }
        upsert_file(
            &mut agent.files,
            key,
            FileState {
                content_hash: new_content_hash,
                line_hashes: new_line_hashes,
                fresh_lines: next_fresh,
                pending_fresh_lines: next_pending,
                last_used: seq,
            },
        );
    } else {
        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(agent) = st.agents.get_mut(&agent_key) {
            agent.files.remove(&key);
        }
    }

    let diff = line_diff(&before, &after);
    let mut details = JsonObject::new();
    details.insert("path".into(), json!(path));
    details.insert("changed".into(), json!(true));
    details.insert("diff".into(), json!(diff));
    Ok(ToolResult::text(format!(
        "Edited {path} (lines {start_line}-{end_line}, {mode}). {region_msg}"
    ))
    .with_details(details))
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let cwd = api.cwd();
        let state: Arc<Mutex<PluginState>> = Arc::default();

        api.on::<hook::SystemPrompt, _, _>(|ev, _ctx| async move {
            let prompt = format!(
                "{}\n\nNote: `read` prefixes each line with `<line>:<hash>|`. You can use `hash_edit` with `start`/`end` anchors (e.g. `42:a3`) instead of copying `old_string` verbatim.",
                ev.prompt
            );
            Ok(Some(SystemPromptResult { prompt: Some(prompt) }))
        });

        let st_ss = state.clone();
        api.on::<hook::SessionStart, _, _>(move |_ev, _ctx| {
            let st = st_ss.clone();
            async move {
                *st.lock().unwrap_or_else(|e| e.into_inner()) = PluginState::default();
                Ok(None)
            }
        });

        let st_am = state.clone();
        api.on::<hook::AssistantMessage, _, _>(move |ev, ctx| {
            let st = st_am.clone();
            async move {
                let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
                let seq = s.next_seq();
                let agent = s.agent_mut(&ctx.agent, seq);
                promote_pending_in(agent);
                agent.active_batch_ids.clear();
                for block in &ev.message.content {
                    if let AssistantContent::ToolCall(tc) = block {
                        agent.active_batch_ids.insert(tc.id.clone());
                    }
                }
                Ok(None)
            }
        });

        let st_te = state.clone();
        api.on::<hook::TurnEnd, _, _>(move |_ev, ctx| {
            let st = st_te.clone();
            async move {
                let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(agent) = s.agents.get_mut(&ctx.agent) {
                    agent.active_batch_ids.clear();
                    promote_pending_in(agent);
                }
                Ok(None)
            }
        });

        api.on::<hook::ToolCallRaw, _, _>(|ev, _ctx| async move {
            if ev.name != "write" {
                return Ok(None);
            }
            let Some(Value::String(content)) = ev.input.get("content") else {
                return Ok(None);
            };
            let Some(plain) = strip_hashline_prefixes(content) else {
                return Ok(None);
            };
            let mut input = ev.input.clone();
            input.insert("content".into(), Value::String(plain));
            Ok(Some(ToolCallRawResult {
                name: None,
                input: Some(input),
                note: Some("removed hashline prefixes (N:hh|) from content; write takes the file's plain text".into()),
            }))
        });

        let st_tc = state.clone();
        api.on::<hook::ToolCall, _, _>(move |ev, ctx| {
            let st = st_tc.clone();
            async move {
                let mut args = ev.args.clone();
                let mut changed = false;
                let strip_field = |args: &mut JsonObject, key: &str, changed: &mut bool| {
                    if let Some(Value::String(s)) = args.get(key)
                        && let Some(plain) = strip_hashline_prefixes(s)
                    {
                        args.insert(key.into(), Value::String(plain));
                        *changed = true;
                    }
                };
                match ev.tool.name() {
                    "hash_edit" => {
                        strip_field(&mut args, "content", &mut changed);
                        let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
                        s.pending_tool_calls
                            .retain(|p| !(p.agent == ctx.agent && p.call_id == ev.call.id));
                        s.pending_tool_calls.push(PendingToolCall {
                            agent: ctx.agent,
                            call_id: ev.call.id,
                            args: args.clone(),
                        });
                    }
                    "edit" => {
                        strip_field(&mut args, "old_string", &mut changed);
                        strip_field(&mut args, "new_string", &mut changed);
                    }
                    _ => return Ok(None),
                }
                if !changed {
                    return Ok(None);
                }
                Ok(Some(ToolCallResult {
                    args: Some(args),
                    decision: None,
                    reason: None,
                }))
            }
        });

        let cwd_res = cwd.clone();
        let st_res = state.clone();
        api.on::<hook::ToolResult, _, _>(move |ev, ctx| {
            let cwd = cwd_res.clone();
            let st = st_res.clone();
            async move {
                let mut out: Option<ToolResultResult> = None;
                if ev.ran && !ev.result.is_error() && ev.call.name == "read" {
                    let disk: Option<(String, String)> = match ev.call.input.get("path").and_then(Value::as_str) {
                        Some(raw) => {
                            let resolved = resolve_path(raw, &cwd);
                            let meta = tokio::fs::metadata(&resolved).await.ok();
                            if meta.is_some_and(|m| m.is_file() && m.len() <= MAX_DISK_HASH_BYTES) {
                                tokio::fs::read(&resolved)
                                    .await
                                    .ok()
                                    .map(|b| (canonical_key(&resolved), String::from_utf8_lossy(&b).into_owned()))
                            } else {
                                None
                            }
                        }
                        None => None,
                    };
                    let disk_lines: Option<Vec<&str>> = disk.as_ref().map(|(_, text)| split_lines(text));

                    let mut changed = false;
                    let mut all_seen = Vec::new();
                    let mut next_content = Vec::with_capacity(ev.result.content.len());
                    for item in &ev.result.content {
                        if let UserContent::Text(tc) = item
                            && let Some((updated, seen)) = transform_read_text(&tc.text, disk_lines.as_deref())
                        {
                            changed = true;
                            all_seen.extend(seen);
                            next_content.push(UserContent::text(updated));
                        } else {
                            next_content.push(item.clone());
                        }
                    }
                    if let Some((key, disk_text)) = disk
                        && !all_seen.is_empty()
                    {
                        record_read(&st, &ctx.agent, key, &disk_text, &all_seen);
                    }
                    if changed {
                        let mut result = ev.result;
                        result.content = next_content;
                        out = Some(ToolResultResult { result: Some(result) });
                    }
                }

                {
                    let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
                    s.pending_tool_calls
                        .retain(|p| !(p.agent == ctx.agent && p.call_id == ev.call.id));
                    if let Some(agent) = s.agents.get_mut(&ctx.agent) {
                        agent.active_batch_ids.remove(&ev.call.id);
                        if agent.active_batch_ids.is_empty() {
                            promote_pending_in(agent);
                        }
                    }
                }

                Ok(out)
            }
        });

        let st_tool = state;
        api.register_tool(
            ToolDef::new(
                "hash_edit",
                "Edit a file by line:hash anchors shown by read (e.g. start='42:a3', end='45:f1'). Avoids whitespace mismatch errors.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path, absolute or relative to working directory" },
                        "start": { "type": "string", "description": "Start line anchor '<line>:<hash>' (e.g. '42:a3')" },
                        "end": { "type": "string", "description": "Optional inclusive end anchor '<line>:<hash>' (defaults to start)" },
                        "content": { "type": "string", "description": "Replacement text (or empty string to delete)" },
                        "mode": { "type": "string", "enum": ["replace", "insert_after", "insert_before"], "description": "Edit mode (default replace)" }
                    },
                    "required": ["path", "start", "content"],
                    "additionalProperties": false
                }),
                ToolKind::Edit,
                move |args, ctx| {
                    let st = st_tool.clone();
                    async move { execute_hash_edit(args, ctx, st).await }
                },
            )
            .always_load(true)
            .permissions_as("edit")
            .paths(|args, cwd| vec![resolve_path(args.get("path").and_then(Value::as_str).unwrap_or(""), cwd)])
            .summary(|args| {
                let path = args.get("path").and_then(Value::as_str).unwrap_or("");
                let start = args.get("start").and_then(Value::as_str).unwrap_or("");
                format!("{path}@{start}")
            })
            .into_ref(),
        );

        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_hashline_prefixes_requires_every_non_empty_line_to_match() {
        assert_eq!(
            strip_hashline_prefixes("12:ab|let a = 1;\n13:cd|let b = 2;\n"),
            Some("let a = 1;\nlet b = 2;\n".into())
        );
        assert_eq!(strip_hashline_prefixes("12:ab|single"), Some("single".into()));
        assert_eq!(
            strip_hashline_prefixes("12:ab|first\n\n14:cd|third"),
            Some("first\n\nthird".into())
        );
        assert_eq!(strip_hashline_prefixes("12:ab|first\nplain second"), None);
        assert_eq!(strip_hashline_prefixes("12:ab|first\n15:cd|jumped"), None);
        assert_eq!(strip_hashline_prefixes(""), None);
    }

    #[test]
    fn check_anchor_freshness_rejects_equal_length_external_change_and_allows_prefix_unchanged() {
        let old_lines = ["a", "b", "c", "", "}", "", "tail"];
        let old_hashes: Vec<u64> = old_lines.iter().map(|l| line_hash_64(l)).collect();
        let fs = FileState {
            content_hash: fnv1a_64(old_lines.join("\n").as_bytes()),
            line_hashes: old_hashes,
            fresh_lines: (1..=old_lines.len()).collect(),
            pending_fresh_lines: HashSet::new(),
            last_used: 1,
        };

        // Equal line count, identical [4, 5, 6] context ("", "}", ""), but line 2 changed -> rejected.
        let equal_len_changed = ["a", "MODIFIED", "c", "", "}", "", "tail"];
        let eq_hashes: Vec<u64> = equal_len_changed.iter().map(|l| line_hash_64(l)).collect();
        let eq_hash = fnv1a_64(equal_len_changed.join("\n").as_bytes());
        assert!(check_anchor_freshness(Some(&fs), "f.rs", eq_hash, &eq_hashes, 5, 5).is_err());

        // External edit strictly below end_line + 1 (line 7 changed, lines 1..=6 identical) -> allowed.
        let suffix_changed = ["a", "b", "c", "", "}", "", "CHANGED_TAIL"];
        let suf_hashes: Vec<u64> = suffix_changed.iter().map(|l| line_hash_64(l)).collect();
        let suf_hash = fnv1a_64(suffix_changed.join("\n").as_bytes());
        assert!(check_anchor_freshness(Some(&fs), "f.rs", suf_hash, &suf_hashes, 5, 5).is_ok());
    }

    #[test]
    fn record_read_caps_files_with_lru_and_subagent_map_keeps_main() {
        let state: Arc<Mutex<PluginState>> = Arc::default();
        for i in 0..MAX_TRACKED_FILES {
            record_read(&state, &None, format!("/tmp/f{i}"), "line1\n", &[1]);
        }
        // Re-read f0 so f1 becomes least recently used.
        record_read(&state, &None, "/tmp/f0".into(), "line1\n", &[1]);
        record_read(&state, &None, "/tmp/f_extra".into(), "line1\n", &[1]);

        let mut st = state.lock().unwrap();
        let main_files = &st.agents.get(&None).unwrap().files;
        assert_eq!(main_files.len(), MAX_TRACKED_FILES);
        assert!(main_files.contains_key("/tmp/f0"));
        assert!(!main_files.contains_key("/tmp/f1"));
        assert!(main_files.contains_key("/tmp/f_extra"));

        for i in 1..=(MAX_TRACKED_SUBAGENTS + 5) {
            let seq = st.next_seq();
            st.agent_mut(&Some(format!("sub-{i}")), seq);
        }
        assert!(st.agents.contains_key(&None));
        assert_eq!(st.agents.keys().filter(|k| k.is_some()).count(), MAX_TRACKED_SUBAGENTS);
    }
}
