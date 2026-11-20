//! hashline-edit (opt-in via `plugins.enabled`): adds short line-number+hash anchors (`42:a3|...`)
//! to `read` results and provides `hash_edit` so small models can edit lines reliably without
//! copying exact whitespace indentation. Also strips accidental `N:hh|` prefixes sent to `write`,
//! `edit` and `hash_edit`.
//!
//! Two hex digits cannot tell one `}` from the next, so a matching hash is not enough: the model
//! must also have been shown the line where it is now. For each agent and file a [`Snapshot`]
//! holds the file as that agent last saw it and, per line, the assistant message during which its
//! anchor was shown (by `read` or by a `hash_edit` result). An edit is accepted when every line
//! from its start to its end anchor holds both:
//! - Its anchor was shown during an earlier message than the one making the call. A second call in
//!   the same message was written before the first one's result existed.
//! - It has not moved since. The plugin knows exactly what the agent's own `hash_edit` did: one
//!   that changes the line count forgets every line below it. Any other change to the file (another
//!   tool, another agent, the user) forgets every anchor of it: shifted lines can look unchanged.
//!
//! Only lines `read` showed as they are on disk get an anchor, and only after slim-profile has cut
//! the output (it loads first). A file the agent has no snapshot of (never read, too large,
//! forgotten) is checked by hash alone. When it is unclear which agent made a call, every agent
//! that has a snapshot of the file must have been shown the lines.

use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use futures::future::BoxFuture;
use regex::Regex;
use serde_json::{Value, json};

use sasacode_ai::JsonObject;
use sasacode_plugin_api::{
    PluginApi, SystemPromptResult, ToolCallRawResult, ToolContext, ToolDef, ToolKind, ToolResult, ToolResultResult,
    UserContent, hook,
};
use sasacode_tools::util::{fs_error, line_diff, resolve_path, split_lines};

const MAX_DISK_HASH_BYTES: u64 = 2_000_000;
const EDIT_CONTEXT_LINES: usize = 2;
const MAX_TRACKED_FILES: usize = 64;
const MAX_TRACKED_SUBAGENTS: usize = 16;
const MAX_IN_FLIGHT: usize = 64;

/// `None`: the main agent. `Some("sub-N")`: a subagent. Each has its own snapshots.
type AgentId = Option<String>;

/// The agent running a `hash_edit`, or `Unknown` when the arguments do not tell.
#[derive(Debug, PartialEq)]
enum Caller {
    Agent(AgentId),
    Unknown,
}

#[derive(Default)]
struct PluginState {
    /// Least recently used first.
    agents: Vec<(AgentId, AgentState)>,
    /// `hash_edit` calls between their `ToolCall` hook and their result. `ToolContext` does not
    /// say which agent is calling, so the hook notes it here and the tool finds it by its arguments.
    in_flight: Vec<InFlight>,
}

struct InFlight {
    agent: AgentId,
    call_id: String,
    args: JsonObject,
}

#[derive(Default)]
struct AgentState {
    /// By canonical path, least recently used first.
    files: Vec<(String, Snapshot)>,
    /// How many assistant messages this agent has produced so far.
    message: u64,
}

/// One file as one agent last saw it.
struct Snapshot {
    line_hashes: Vec<u32>,
    /// Per line, the assistant message during which its anchor was shown. `None`: the model holds
    /// no anchor for the line as it is now.
    shown_in: Vec<Option<u64>>,
}

/// A refused `hash_edit`: the message for the model and the lines (1-based, end exclusive) whose
/// anchors it prints.
struct Refusal {
    message: String,
    shown: Range<usize>,
}

impl From<String> for Refusal {
    fn from(message: String) -> Self {
        Refusal { message, shown: 0..0 }
    }
}

/// A `hash_edit` that passed every check, ready to be written.
struct Plan {
    after: String,
    /// The old lines (0-based, end exclusive) that give way to `added` new ones.
    replaced: Range<usize>,
    added: usize,
    /// The lines of the new file (1-based, end exclusive) whose anchors the result prints.
    shown: Range<usize>,
    summary: String,
}

fn lock(state: &Mutex<PluginState>) -> MutexGuard<'_, PluginState> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

impl PluginState {
    /// The state of `id`, created on first use, now the most recently used. Room is made by
    /// forgetting the least recently used subagent, never the main agent.
    fn agent_mut(&mut self, id: &AgentId) -> &mut AgentState {
        let state = match self.agents.iter().position(|(agent, _)| agent == id) {
            Some(found) => self.agents.remove(found).1,
            None => {
                let subagents = self.agents.iter().filter(|(agent, _)| agent.is_some()).count();
                let lru = self.agents.iter().position(|(agent, _)| agent.is_some());
                if let Some(lru) = lru
                    && id.is_some()
                    && subagents >= MAX_TRACKED_SUBAGENTS
                {
                    self.agents.remove(lru);
                }
                AgentState::default()
            }
        };
        self.agents.push((id.clone(), state));
        &mut self.agents.last_mut().expect("just pushed").1
    }

    /// The `ToolCall` hook saw `agent` make call `call_id`. Seen again (the user edited the
    /// arguments at approval), the newer arguments replace the older ones.
    fn note_call(&mut self, agent: AgentId, call_id: String, args: JsonObject) {
        self.in_flight.retain(|c| c.agent != agent || c.call_id != call_id);
        if self.in_flight.len() >= MAX_IN_FLIGHT {
            self.in_flight.remove(0);
        }
        self.in_flight.push(InFlight { agent, call_id, args });
    }

    /// The agent of the `hash_edit` with these arguments: the only agent with a call in flight
    /// with exactly them. None or several such agents: `Unknown`.
    fn caller_of(&self, args: &JsonObject) -> Caller {
        let mut agents = self.in_flight.iter().filter(|c| c.args == *args).map(|c| &c.agent);
        match agents.next() {
            Some(first) if agents.all(|agent| agent == first) => Caller::Agent(first.clone()),
            _ => Caller::Unknown,
        }
    }

    /// Per line of `text`, the file on disk now: was `caller` shown its anchor, where the line is
    /// now, before writing its current message? An unknown caller counts as shown only what every
    /// agent with a snapshot was shown. `None`: no snapshot to go by.
    fn lines_shown(&mut self, caller: &Caller, key: &str, text: &str) -> Option<Vec<bool>> {
        match caller {
            Caller::Agent(id) => self.agent_mut(id).shown_before_now(key, text),
            Caller::Unknown => self
                .agents
                .iter_mut()
                .filter_map(|(_, agent)| agent.shown_before_now(key, text))
                .reduce(|all, one| all.iter().zip(one).map(|(a, b)| *a && b).collect()),
        }
    }
}

impl AgentState {
    /// Takes out the snapshot of `key`, brought up to `text`, the file on disk now.
    fn take(&mut self, key: &str, text: &str) -> Option<Snapshot> {
        let found = self.files.iter().position(|(path, _)| path == key)?;
        let mut snapshot = self.files.remove(found).1;
        snapshot.follow_disk(text);
        Some(snapshot)
    }

    /// Per line of `text`: was the anchor shown before the current message? See [`PluginState::lines_shown`].
    fn shown_before_now(&mut self, key: &str, text: &str) -> Option<Vec<bool>> {
        let snapshot = self.take(key, text)?;
        let now = self.message;
        let shown = snapshot.shown_in.iter().map(|m| m.is_some_and(|m| m < now)).collect();
        self.store(key.to_string(), snapshot);
        Some(shown)
    }

    /// Keeps a snapshot that was taken out, or a new one, as the most recently used file.
    fn store(&mut self, key: String, snapshot: Snapshot) {
        if self.files.len() >= MAX_TRACKED_FILES {
            self.files.remove(0);
        }
        self.files.push((key, snapshot));
    }
}

impl Snapshot {
    /// `text` with no anchor shown yet.
    fn of(text: &str) -> Self {
        let line_hashes: Vec<u32> = split_lines(text).into_iter().map(fnv32).collect();
        Snapshot {
            shown_in: vec![None; line_hashes.len()],
            line_hashes,
        }
    }

    /// Brings the snapshot up to `text`, the file on disk now. If something other than this
    /// agent's `hash_edit` changed it, any line may have moved, even one whose hash and position
    /// look the same (an inserted copy of the lines below it), so every anchor is forgotten.
    fn follow_disk(&mut self, text: &str) {
        let disk = Snapshot::of(text);
        if disk.line_hashes != self.line_hashes {
            *self = disk;
        }
    }

    /// The snapshot once `plan` is written, given the one from before it (already following the
    /// disk). Lines above the edit stay where they were; so do the lines below it when the line
    /// count is unchanged, otherwise they moved and their anchors are forgotten.
    fn after_edit(old: Option<Snapshot>, plan: &Plan, now: u64) -> Self {
        let mut next = Snapshot::of(&plan.after);
        if let Some(old) = old {
            let same_count = plan.added == plan.replaced.len();
            let below = if same_count { plan.replaced.end } else { usize::MAX };
            for (i, (slot, was)) in next.shown_in.iter_mut().zip(old.shown_in).enumerate() {
                if i < plan.replaced.start || i >= below {
                    *slot = was;
                }
            }
        }
        next.show(plan.shown.clone(), now);
        next
    }

    /// Notes that the anchors of `lines` (1-based) were shown during message `now`. A line whose
    /// anchor the model already holds keeps its earlier message: seeing it again takes nothing away.
    fn show(&mut self, lines: impl IntoIterator<Item = usize>, now: u64) {
        for line in lines {
            if let Some(slot) = line.checked_sub(1).and_then(|i| self.shown_in.get_mut(i)) {
                slot.get_or_insert(now);
            }
        }
    }
}

fn without_cr(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

/// 32-bit FNV-1a of a line without its line ending.
fn fnv32(line: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in without_cr(line).as_bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Compute a 2-hex-digit hash for a single source line (without trailing `\r` or `\n`).
pub fn line_hash(line: &str) -> String {
    let h = fnv32(line);
    format!("{:02x}", (h ^ (h >> 8) ^ (h >> 16) ^ (h >> 24)) & 0xff)
}

fn canonical_key(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

fn str_arg<'a>(args: &'a JsonObject, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

static READ_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ *(\d+)\t(.*)$").expect("valid regex"));
static HASHLINE_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\d+):([0-9a-fA-F]{2})\|(.*)$").expect("valid regex"));

/// Whether `read` showed `body` for the disk line `on_disk`: as it is, or shortened to a prefix.
fn shows(body: &str, on_disk: &str) -> bool {
    let (body, on_disk) = (without_cr(body), without_cr(on_disk));
    let shortened = body.rsplit_once("… [line truncated, ");
    body == on_disk || shortened.is_some_and(|(head, tail)| tail.ends_with(" chars]") && on_disk.starts_with(head))
}

/// `read` output with the `<n>\t` of each numbered line turned into `<n>:<hash>|`. The hash is
/// that of the line on disk; the numbers are added to `shown`. `None` when a line differs from the
/// one on disk (the file changed while it was read): the anchors would not match what was shown.
fn annotate_read(text: &str, disk_lines: Option<&[&str]>, shown: &mut Vec<usize>) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    for row in text.split('\n') {
        if let Some(cap) = READ_LINE.captures(row)
            && let Ok(n) = cap[1].parse::<usize>()
            && n >= 1
        {
            let body = &cap[2];
            let on_disk = match disk_lines {
                Some(lines) => *lines.get(n - 1).filter(|line| shows(body, line))?,
                None => body,
            };
            out.push(format!("{n}:{}|{body}", line_hash(on_disk)));
            shown.push(n);
        } else {
            out.push(row.to_string());
        }
    }
    Some(out.join("\n"))
}

/// Strip sequential `N:hh|` prefixes when every non-empty line carries one (copied from
/// hashline-annotated `read` output) and line numbers advance consistently.
pub fn strip_hashline_prefixes(content: &str) -> Option<String> {
    let trimmed = content.strip_suffix('\n').unwrap_or(content);
    let mut expected: Option<usize> = None;
    let mut blank_streak: usize = 0;
    let mut body: Vec<&str> = Vec::new();
    for row in trimmed.split('\n') {
        if row.trim().is_empty() {
            blank_streak += 1;
            body.push(row);
            continue;
        }
        let cap = HASHLINE_PREFIX.captures(row)?;
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

/// `42:a3` (anything from a `|` on is ignored) as the line number and the lowercase hash.
fn parse_anchor(raw: &str) -> Result<(usize, String), String> {
    let head = raw.split('|').next().unwrap_or("");
    let parsed = head.split_once(':').and_then(|(line, hash)| {
        let line = line.trim().parse::<usize>().ok().filter(|&n| n >= 1)?;
        let hash = hash.trim().to_ascii_lowercase();
        let two_hex = hash.len() == 2 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        two_hex.then_some((line, hash))
    });
    parsed.ok_or_else(|| format!("Invalid anchor \"{raw}\": expected '<line>:<2-hex-hash>' (e.g. '42:a3')."))
}

/// The given lines (0-based, end exclusive, cut at the end of the file) as `read` shows them.
fn anchored(lines: &[&str], range: Range<usize>) -> String {
    let rows = lines.iter().enumerate().skip(range.start).take(range.len());
    let rows = rows.map(|(i, line)| format!("{}:{}|{line}", i + 1, line_hash(line)));
    rows.collect::<Vec<_>>().join("\n")
}

/// The text of a file `read` named, unless it is too large to track.
async fn tracked_text(path: &str) -> Option<String> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    if !meta.is_file() || meta.len() > MAX_DISK_HASH_BYTES {
        return None;
    }
    let bytes = tokio::fs::read(path).await.ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// `agent` was shown the anchors of lines `shown` (1-based) of `text`, the file on disk now.
/// `None`: it was shown lines that may not be the file's, so it forgets every anchor of it.
fn record_read(state: &Mutex<PluginState>, agent: &AgentId, key: String, text: &str, shown: Option<&[usize]>) {
    let mut st = lock(state);
    let agent = st.agent_mut(agent);
    let mut snapshot = agent.take(&key, text).unwrap_or_else(|| Snapshot::of(text));
    match shown {
        Some(lines) => snapshot.show(lines.iter().copied(), agent.message),
        None => snapshot.shown_in.fill(None),
    }
    agent.store(key, snapshot);
}

/// `hash_edit` turned `before` into `plan.after` for `agent`: its snapshot now describes that.
fn record_edit(state: &Mutex<PluginState>, agent: &AgentId, key: String, before: &str, plan: &Plan) {
    let mut st = lock(state);
    let agent = st.agent_mut(agent);
    let old = agent.take(&key, before);
    if plan.after.len() as u64 <= MAX_DISK_HASH_BYTES {
        agent.store(key, Snapshot::after_edit(old, plan, agent.message));
    }
}

/// Checks one `hash_edit` against `before`, the file on disk, and works out what to write.
/// `shown` is [`PluginState::lines_shown`] for it. Without `anchors` (the caller is unknown) the
/// messages print none: no agent would be recorded as shown them.
fn plan_edit(
    args: &JsonObject,
    path: &str,
    before: &str,
    shown: Option<&[bool]>,
    anchors: bool,
) -> Result<Plan, Refusal> {
    if before.is_empty() {
        return Err(format!("{path} is empty. Use write to populate it.").into());
    }
    let (start, start_hash) = parse_anchor(str_arg(args, "start"))?;
    let (end, end_hash) = match str_arg(args, "end").trim() {
        "" => (start, start_hash.clone()),
        raw => parse_anchor(raw)?,
    };
    let mode = args.get("mode").and_then(Value::as_str).unwrap_or("replace");
    let content = str_arg(args, "content");
    let lines: Vec<&str> = split_lines(before).into_iter().map(without_cr).collect();
    let total = lines.len();

    // An error must not print lines of a file the agent has not read (`shown` is `None`).
    if end < start || end > total {
        return Err(
            format!("Lines {start}-{end} are not a range in {path} ({total} lines). Re-read {path} first.").into(),
        );
    }
    // Every line in the range, not just its ends: they are what gets replaced. A shown line has the
    // hash it had then, as any change but the agent's own `hash_edit` forgets every anchor.
    if shown.is_some_and(|shown| !shown[start - 1..end].iter().all(|&s| s)) {
        return Err(format!(
            "Stale anchor at {path} lines {start}-{end}: the file changed since your last read of these lines, they moved, or you have not read all of them. Re-read {path} before editing."
        )
        .into());
    }
    for (line, expected) in [(start, &start_hash), (end, &end_hash)] {
        let found = line_hash(lines[line - 1]);
        if found == *expected {
            continue;
        }
        if shown.is_none() || !anchors {
            return Err(format!("Hash mismatch at {path} line {line}. Read {path} first.").into());
        }
        let nearby = line.saturating_sub(4)..(line + 3).min(total);
        return Err(Refusal {
            message: format!(
                "Hash mismatch at {path} line {line}: expected {expected}, found {found}. Current lines:\n{}",
                anchored(&lines, nearby.clone())
            ),
            shown: nearby.start + 1..nearby.end + 1,
        });
    }
    if mode != "replace" && content.is_empty() {
        return Err(format!("content must not be empty for mode \"{mode}\".").into());
    }
    let replaced = match mode {
        "replace" => start - 1..end,
        "insert_after" => end..end,
        "insert_before" => start - 1..start - 1,
        other => return Err(format!("Unknown mode \"{other}\"").into()),
    };

    let body = content.strip_suffix('\n').unwrap_or(content);
    let new_lines: Vec<&str> = if content.is_empty() {
        Vec::new()
    } else {
        body.split('\n').map(without_cr).collect()
    };
    let mut next: Vec<&str> = lines[..replaced.start].to_vec();
    next.extend(&new_lines);
    next.extend(&lines[replaced.end..]);
    let newline = if before.contains("\r\n") { "\r\n" } else { "\n" };
    let mut after = next.join(newline);
    if before.ends_with('\n') && !next.is_empty() {
        after.push_str(newline);
    }
    if after == before {
        return Err("No changes made (replacement matches existing lines)."
            .to_string()
            .into());
    }

    // The result prints the new lines (after a deletion, the line now in their place) with some
    // context, so the model can go on editing there without another read.
    let first = replaced.start.min(next.len().saturating_sub(1));
    let last = (replaced.start + new_lines.len()).saturating_sub(1).max(first);
    let from = first.saturating_sub(EDIT_CONTEXT_LINES);
    let to = (last + EDIT_CONTEXT_LINES + 1).min(next.len());
    let region = if next.is_empty() {
        "File is now empty.".to_string()
    } else if !anchors {
        format!("Re-read {path} before editing it again.")
    } else {
        format!("Updated lines {}-{to}:\n{}", from + 1, anchored(&next, from..to))
    };
    Ok(Plan {
        after,
        replaced,
        added: new_lines.len(),
        shown: from + 1..to + 1,
        summary: format!("Edited {path} (lines {start}-{end}, {mode}). {region}"),
    })
}

async fn execute_hash_edit(
    args: JsonObject,
    ctx: ToolContext,
    state: Arc<Mutex<PluginState>>,
) -> anyhow::Result<ToolResult> {
    let caller = lock(&state).caller_of(&args);
    let path = resolve_path(str_arg(&args, "path"), &ctx.cwd);
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
    let shown = lock(&state).lines_shown(&caller, &key, &before);
    let anchors = caller != Caller::Unknown;
    let plan = match plan_edit(&args, &path, &before, shown.as_deref(), anchors) {
        Ok(plan) => plan,
        Err(refusal) => {
            if let Caller::Agent(agent) = &caller
                && !refusal.shown.is_empty()
            {
                let printed: Vec<usize> = refusal.shown.collect();
                record_read(&state, agent, key, &before, Some(&printed));
            }
            return Ok(ToolResult::error(refusal.message));
        }
    };
    if ctx.signal.is_cancelled() {
        return Ok(ToolResult::error("Interrupted by the user; the file was not changed."));
    }
    tokio::fs::write(&path, &plan.after)
        .await
        .map_err(|e| fs_error(&e, "open", &path))?;
    // After an unknown caller's edit no snapshot follows it, so every agent must read the file again.
    if let Caller::Agent(agent) = &caller {
        record_edit(&state, agent, key, &before, &plan);
    }

    let mut details = JsonObject::new();
    details.insert("path".into(), json!(path));
    details.insert("changed".into(), json!(true));
    details.insert("diff".into(), json!(line_diff(&before, &plan.after)));
    Ok(ToolResult::text(plan.summary).with_details(details))
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

        let st = state.clone();
        api.on::<hook::SessionStart, _, _>(move |_ev, _ctx| {
            let st = st.clone();
            async move {
                *lock(&st) = PluginState::default();
                Ok(None)
            }
        });

        // Anchors shown from here on were not in front of the model when it wrote this message.
        let st = state.clone();
        api.on::<hook::AssistantMessage, _, _>(move |_ev, ctx| {
            let st = st.clone();
            async move {
                lock(&st).agent_mut(&ctx.agent).message += 1;
                Ok(None)
            }
        });

        api.on::<hook::ToolCallRaw, _, _>(|ev, _ctx| async move {
            let fields: &[&str] = match ev.name.as_str() {
                "write" | "hash_edit" => &["content"],
                "edit" => &["old_string", "new_string"],
                _ => return Ok(None),
            };
            let mut input = ev.input;
            let mut stripped = false;
            for &field in fields {
                let text = input.get(field).and_then(Value::as_str);
                if let Some(plain) = text.and_then(strip_hashline_prefixes) {
                    input.insert(field.into(), Value::String(plain));
                    stripped = true;
                }
            }
            Ok(stripped.then(|| ToolCallRawResult {
                name: None,
                input: Some(input),
                note: Some("removed hashline prefixes (N:hh|): this tool takes the file's plain text".into()),
            }))
        });

        let st = state.clone();
        api.on::<hook::ToolCall, _, _>(move |ev, ctx| {
            let st = st.clone();
            async move {
                if ev.tool.name() == "hash_edit" {
                    lock(&st).note_call(ctx.agent, ev.call.id, ev.args);
                }
                Ok(None)
            }
        });

        let st = state.clone();
        api.on::<hook::ToolResult, _, _>(move |ev, ctx| {
            let (cwd, st) = (cwd.clone(), st.clone());
            async move {
                // A `hash_edit` that was refused never ran, so its note is still there.
                lock(&st)
                    .in_flight
                    .retain(|c| c.agent != ctx.agent || c.call_id != ev.call.id);
                if !ev.ran || ev.result.is_error() || ev.call.name != "read" {
                    return Ok(None);
                }
                let path = resolve_path(str_arg(&ev.call.input, "path"), &cwd);
                let disk = tracked_text(&path).await;
                let disk_lines = disk.as_deref().map(split_lines);

                let mut result = ev.result;
                let mut shown = Vec::new();
                let mut parts: Vec<&mut String> = result
                    .content
                    .iter_mut()
                    .filter_map(|item| match item {
                        UserContent::Text(part) => Some(&mut part.text),
                        _ => None,
                    })
                    .collect();
                let annotated: Option<Vec<String>> = parts
                    .iter()
                    .map(|text| annotate_read(text, disk_lines.as_deref(), &mut shown))
                    .collect();
                // Shown text that is not the file's (it changed while being read): forget every anchor.
                if let Some(text) = &disk {
                    let lines = annotated.as_ref().map(|_| &shown[..]);
                    record_read(&st, &ctx.agent, canonical_key(&path), text, lines);
                }
                let Some(annotated) = annotated.filter(|_| !shown.is_empty()) else {
                    return Ok(None);
                };
                for (part, text) in parts.iter_mut().zip(annotated) {
                    **part = text;
                }
                Ok(Some(ToolResultResult { result: Some(result) }))
            }
        });

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
                move |args, ctx| execute_hash_edit(args, ctx, state.clone()),
            )
            .always_load(true)
            .permissions_as("edit")
            .paths(|args, cwd| vec![resolve_path(str_arg(args, "path"), cwd)])
            .summary(|args| format!("{}@{}", str_arg(args, "path"), str_arg(args, "start")))
            .into_ref(),
        );

        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(path: &str) -> JsonObject {
        json!({"path": path, "start": "1:00", "content": "x"})
            .as_object()
            .unwrap()
            .clone()
    }

    #[test]
    fn subagents_beyond_the_cap_push_out_the_least_recently_used_subagent_never_the_main_agent() {
        let mut st = PluginState::default();
        st.agent_mut(&None).message = 7;
        for i in 1..=MAX_TRACKED_SUBAGENTS {
            st.agent_mut(&Some(format!("sub-{i}")));
        }
        // sub-1 is the oldest but was just used: sub-2 is the one to go.
        st.agent_mut(&Some("sub-1".into()));
        st.agent_mut(&Some("sub-new".into()));
        let has = |st: &PluginState, name: &str| st.agents.iter().any(|(a, _)| a.as_deref() == Some(name));
        assert_eq!(st.agents.len(), 1 + MAX_TRACKED_SUBAGENTS);
        assert!(has(&st, "sub-1") && !has(&st, "sub-2") && has(&st, "sub-new"));
        assert_eq!(st.agent_mut(&None).message, 7);
    }

    #[test]
    fn a_call_is_attributed_only_when_exactly_one_agent_has_it_in_flight() {
        let mut st = PluginState::default();
        let sub = Some("sub-1".to_string());
        assert_eq!(st.caller_of(&args("a")), Caller::Unknown);
        st.note_call(None, "1".into(), args("a"));
        st.note_call(None, "2".into(), args("a"));
        assert_eq!(st.caller_of(&args("a")), Caller::Agent(None));
        st.note_call(sub.clone(), "1".into(), args("a"));
        assert_eq!(
            st.caller_of(&args("a")),
            Caller::Unknown,
            "main agent and subagent alike"
        );
        // The same call seen again with arguments the user edited replaces its first note.
        st.note_call(sub.clone(), "1".into(), args("b"));
        assert_eq!(st.caller_of(&args("a")), Caller::Agent(None));
        assert_eq!(st.caller_of(&args("b")), Caller::Agent(sub));
        assert_eq!(st.in_flight.len(), 3);
        for i in 0..MAX_IN_FLIGHT + 5 {
            st.note_call(None, format!("x{i}"), args("c"));
        }
        assert_eq!(st.in_flight.len(), MAX_IN_FLIGHT);
    }

    #[test]
    fn an_unknown_caller_needs_every_agent_with_a_snapshot_to_have_been_shown_the_lines() {
        let mut st = PluginState::default();
        let text = "a\nb\nc\n";
        let sub = Some("sub-1".to_string());
        assert_eq!(st.lines_shown(&Caller::Unknown, "f", text), None);
        for (agent, lines) in [(&None, vec![1, 2]), (&sub, vec![2, 3])] {
            let state = st.agent_mut(agent);
            let mut snapshot = Snapshot::of(text);
            snapshot.show(lines, state.message);
            state.message += 1;
            state.store("f".into(), snapshot);
        }
        assert_eq!(
            st.lines_shown(&Caller::Agent(None), "f", text),
            Some(vec![true, true, false])
        );
        assert_eq!(
            st.lines_shown(&Caller::Unknown, "f", text),
            Some(vec![false, true, false])
        );
    }

    #[test]
    fn read_output_that_differs_from_the_disk_gets_no_anchors() {
        let disk = ["one", "two", "x".repeat(30).leak()];
        let read = "     1\tone\n     2\ttwo\n     3\txxxxx… [line truncated, 30 chars]\n[note]";
        let mut shown = Vec::new();
        let out = annotate_read(read, Some(&disk), &mut shown).expect("same as disk");
        assert!(out.starts_with(&format!("1:{}|one\n", line_hash("one"))), "{out}");
        assert_eq!(shown, vec![1, 2, 3]);
        // The file changed between `read` and the hook: line 2 is no longer what was shown.
        let changed = ["one", "TWO", "x"];
        assert_eq!(annotate_read(read, Some(&changed), &mut Vec::new()), None);
        assert_eq!(annotate_read("     4\tfour", Some(&disk), &mut Vec::new()), None);
    }

    #[test]
    fn any_change_but_the_agents_own_hash_edit_forgets_every_anchor() {
        let mut snapshot = Snapshot::of("[\n  {\n  },\n]\n");
        snapshot.show(1..=4, 0);
        snapshot.follow_disk("[\n  {\n  },\n]\n");
        assert!(snapshot.shown_in.iter().all(Option::is_some));
        snapshot.follow_disk("[\n  {\n  },\n  {\n  },\n]\n");
        assert_eq!(snapshot.shown_in, vec![None; 6]);
    }
}
