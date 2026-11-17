//! The session catalog: every session log on this machine summarized once in an index, so that
//! /resume, -c, -r <id> from another directory and /usage read one small file instead of every log
//! (`packages/host/src/sessions.ts`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use sasacode_agent::session::{SessionEntry, legacy_name, normalize_cwd, parse_json_lenient, read_entries, restore};
use sasacode_ai::json::js_f64;
use sasacode_ai::{Message, UserContent};

#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub path: PathBuf,
    pub cwd: String,
    pub created_at: String,
    /// Epoch ms, whole (a JS `Date`: the index's fractional mtime truncated).
    pub modified_at: i64,
    pub first_prompt: String,
    pub message_count: usize,
}

/// What one model response used. The same shape /usage aggregates its records into.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsage {
    /// Epoch ms.
    pub t: i64,
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    #[serde(with = "js_f64")]
    pub cost: f64,
}

/// What one session log adds up to. Kept in the index so listings cost one stat per log instead of
/// reading and rebuilding every session; a log is re-parsed only when its size or mtime changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    /// "" when the log's first line was torn away: not resumable, but its usage still counts.
    pub id: String,
    pub cwd: String,
    pub created_at: String,
    /// Size and mtime (ms, fractional like Node's `mtimeMs`) at the last parse: the staleness test
    /// for the log itself.
    pub size: u64,
    #[serde(with = "js_f64")]
    pub modified_at: f64,
    pub first_prompt: String,
    /// `restore(entries).messages.len()`: the history a resume would start from.
    pub message_count: usize,
    /// Every model response in the log. Kept per response rather than summed per day and model:
    /// /usage reports each response and the last use of each model, and a record is a few dozen bytes.
    pub usage: Vec<SessionUsage>,
}

/// The one index for all sessions on this machine: `<sessions root>/.index.json`, keyed by session
/// log path. Written by temp file + rename and rebuilt from scratch when missing or corrupt;
/// concurrent sasacode instances can lose an update, which only costs one re-parse.
const INDEX_NAME: &str = ".index.json";

/// Node's `stat.size` and `stat.mtimeMs` (seconds × 1000 + nanoseconds / 1e6, as a double: the
/// value the TS stored, so an index it wrote stays valid).
fn stat(path: &Path) -> Option<(u64, f64)> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let ms = match mtime.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as f64 * 1000.0 + f64::from(d.subsec_nanos()) / 1e6,
        Err(e) => {
            let d = e.duration();
            -(d.as_secs() as f64 * 1000.0 + f64::from(d.subsec_nanos()) / 1e6)
        }
    };
    Some((meta.len(), ms))
}

fn first_prompt(content: &[UserContent]) -> String {
    let parts: Vec<&str> = content
        .iter()
        .map(|c| match c {
            UserContent::Text(t) => t.text.as_str(),
            UserContent::Image(_) => "[image]",
        })
        .collect();
    parts.join(" ")
}

fn summarize(path: &Path) -> Option<SessionSummary> {
    let (size, modified_at) = stat(path)?;
    let entries = read_entries(path).ok()?;
    let session = match entries.first() {
        Some(SessionEntry::Session {
            id, cwd, created_at, ..
        }) => Some((id, cwd, created_at)),
        _ => None,
    };
    let first_user = entries.iter().find_map(|e| match e {
        SessionEntry::Message {
            message: Message::User(u),
        } => Some(u),
        _ => None,
    });
    let mut usage = Vec::new();
    for e in &entries {
        match e {
            // Subagents, side requests and discarded replies are logged as "usage" entries.
            SessionEntry::Usage(s) => usage.push(SessionUsage {
                t: s.t,
                model: s.model.clone(),
                input: s.usage.input,
                output: s.usage.output,
                cache_read: s.usage.cache_read,
                cache_write: s.usage.cache_write,
                cost: s.usage.cost,
            }),
            // Only "message" entries: a "replace" (compaction) lists earlier messages again.
            SessionEntry::Message {
                message: Message::Assistant(m),
            } if m.timestamp != 0 => usage.push(SessionUsage {
                t: m.timestamp,
                model: format!("{}/{}", m.provider, m.model),
                input: m.usage.input,
                output: m.usage.output,
                cache_read: m.usage.cache_read,
                cache_write: m.usage.cache_write,
                cost: m.usage.cost,
            }),
            _ => {}
        }
    }
    Some(SessionSummary {
        id: session.map(|s| s.0.clone()).unwrap_or_default(),
        cwd: session.map(|s| s.1.clone()).unwrap_or_default(),
        created_at: session.map(|s| s.2.clone()).unwrap_or_default(),
        size,
        modified_at,
        first_prompt: first_user.map(|u| first_prompt(&u.content)).unwrap_or_default(),
        message_count: restore(&entries).messages.len(),
        usage,
    })
}

/// `readdirSync` as Node gives it: names sorted by bytes (libuv's scandir sorts them).
fn read_dir_sorted(dir: &Path) -> Vec<String> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn key(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Summaries of every session log under the sessions root `root`, keyed by log path. Logs the index
/// already knows are served from it; only logs whose size or mtime moved are re-parsed, and the
/// index is rewritten when anything changed.
pub fn load_session_index(root: &Path) -> IndexMap<String, SessionSummary> {
    // The layout is one folder per working directory (see session_dir): one level of readdir.
    let mut paths: Vec<PathBuf> = Vec::new();
    for d in read_dir_sorted(root) {
        let dir = root.join(&d);
        for f in read_dir_sorted(&dir) {
            if f.ends_with(".jsonl") {
                paths.push(dir.join(f));
            }
        }
    }
    let index_path = root.join(INDEX_NAME);
    // Entries are checked one by one: a malformed one is re-parsed, not the whole index.
    let mut stored: serde_json::Map<String, Value> = serde_json::Map::new();
    if let Ok(text) = fs::read_to_string(&index_path)
        && let Ok(Value::Object(mut index)) = parse_json_lenient::<Value>(&text)
        && index.get("version").and_then(Value::as_f64) == Some(1.0)
        && let Some(Value::Object(sessions)) = index.remove("sessions")
    {
        stored = sessions;
    }
    let mut out: IndexMap<String, SessionSummary> = IndexMap::new();
    let mut dirty = false;
    for path in &paths {
        let k = key(path);
        let cached = stored.get(&k).and_then(|v| SessionSummary::deserialize(v).ok());
        let fresh = match (cached, stat(path)) {
            (Some(c), Some((size, ms))) if c.size == size && c.modified_at == ms => Some(c),
            _ => None,
        };
        let summary = match fresh {
            Some(s) => Some(s),
            None => {
                let s = summarize(path);
                dirty |= s.is_some();
                s
            }
        };
        if let Some(s) = summary {
            out.insert(k, s);
        }
    }
    // Logs that vanished drop out too, so the index cannot grow without bound.
    if !dirty {
        dirty = stored.keys().any(|k| !out.contains_key(k));
    }
    if dirty {
        let tmp = PathBuf::from(format!("{}.{}.tmp", index_path.display(), std::process::id()));
        let body = serde_json::json!({ "version": 1, "sessions": &out });
        let written = fs::write(&tmp, body.to_string()).and_then(|()| fs::rename(&tmp, &index_path));
        // Best effort: a read-only or vanished root must not break a listing; the next call re-parses.
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }
    out
}

fn to_info(path: &str, s: &SessionSummary) -> SessionInfo {
    SessionInfo {
        id: s.id.clone(),
        path: PathBuf::from(path),
        cwd: s.cwd.clone(),
        created_at: s.created_at.clone(),
        // new Date(ms): whole milliseconds, toward zero.
        modified_at: s.modified_at.trunc() as i64,
        first_prompt: s.first_prompt.clone(),
        message_count: s.message_count,
    }
}

fn parent(dir: &Path) -> PathBuf {
    dir.parent().map(Path::to_path_buf).unwrap_or_else(|| dir.to_path_buf())
}

/// Sessions in `dir`, newest first. With `cwd`, only that directory's sessions, including ones
/// saved under the pre-0.8 folder name (which other directories may share).
pub fn list_sessions(dir: &Path, cwd: Option<&str>) -> Vec<SessionInfo> {
    let root = parent(dir);
    let dirs = match cwd {
        Some(c) => vec![
            dir.to_path_buf(),
            root.join(legacy_name(&normalize_cwd(c))),
            root.join(legacy_name(c)),
        ],
        None => vec![dir.to_path_buf()],
    };
    let mut paths: Vec<String> = Vec::new();
    for d in &dirs {
        for f in read_dir_sorted(d) {
            let p = key(&d.join(&f));
            if f.ends_with(".jsonl") && !paths.contains(&p) {
                paths.push(p);
            }
        }
    }
    let index = load_session_index(&root);
    let want = cwd.map(normalize_cwd);
    let mut out: Vec<SessionInfo> = Vec::new();
    for path in &paths {
        let Some(s) = index.get(path) else { continue };
        // No id: not a session log, or its header was torn away (its usage still counts).
        if s.id.is_empty() {
            continue;
        }
        if want.as_ref().is_some_and(|w| normalize_cwd(&s.cwd) != *w) {
            continue;
        }
        out.push(to_info(path, s));
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.modified_at));
    out
}

/// The session matching `reference` in any session folder under `root`, for resuming by id from
/// another directory. Matching is what a resume uses locally: exact path, exact id, or an id prefix;
/// the newest wins when a prefix matches several.
pub fn find_session(root: &Path, reference: &str) -> Option<SessionInfo> {
    let mut best: Option<SessionInfo> = None;
    for (path, s) in &load_session_index(root) {
        if s.id.is_empty() || !(s.id == reference || path == reference || s.id.starts_with(reference)) {
            continue;
        }
        let info = to_info(path, s);
        if best.as_ref().is_none_or(|b| info.modified_at > b.modified_at) {
            best = Some(info);
        }
    }
    best
}
