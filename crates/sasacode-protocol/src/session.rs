//! Session logs: the append-only JSONL files sasacode keeps under `$SASACODE_HOME/sessions`
//! (`packages/agent/src/session.ts`), and where a working directory's logs live.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::message::{Message, Usage, text_of};

/// The log format version this crate reads (`SESSION_VERSION`).
pub const SESSION_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEntry {
    /// The first line of every log.
    Session {
        version: u32,
        id: String,
        cwd: String,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    Message {
        message: Message,
    },
    /// The model was switched to `provider/model`.
    Model {
        model: String,
    },
    PermissionMode {
        mode: String,
    },
    /// The whole conversation was replaced (compaction).
    Replace {
        messages: Vec<Message>,
    },
    /// A call that was mended before it ran (`tool-repair`). The message holds the mended call.
    ToolRepair {
        #[serde(rename = "callId")]
        call_id: String,
        #[serde(default)]
        note: String,
    },
    /// What a response outside the conversation used (a subagent, a side request, a discarded
    /// reply). The conversation's own usage is in its assistant messages.
    Usage {
        #[serde(default)]
        usage: Usage,
        #[serde(default)]
        source: String,
    },
    /// A plugin's own entry: not part of the conversation, so not read here.
    #[serde(other)]
    Other,
}

/// Reads a log. Lines that do not parse (a torn last line after a crash) are skipped, as the
/// TypeScript reader does.
pub fn read_entries(path: &Path) -> std::io::Result<Vec<SessionEntry>> {
    let raw = fs::read_to_string(path)?;
    Ok(raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

/// The conversation and the model to continue with, rebuilt from a log's entries. Unlike the
/// TypeScript `restore`, calls without a result are left as they are: this is for showing a
/// conversation, not for sending it to a model.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Restored {
    pub messages: Vec<Message>,
    /// `provider/model`.
    pub model: Option<String>,
}

pub fn restore(entries: &[SessionEntry]) -> Restored {
    let mut out = Restored::default();
    for e in entries {
        match e {
            SessionEntry::Message { message } => {
                if let Message::Assistant(a) = message {
                    out.model = Some(format!("{}/{}", a.provider, a.model));
                }
                out.messages.push(message.clone());
            }
            SessionEntry::Replace { messages } => out.messages = messages.clone(),
            SessionEntry::Model { model } => out.model = Some(model.clone()),
            _ => {}
        }
    }
    out
}

/// Where sasacode keeps its files: `$SASACODE_HOME`, or `~/.sasacode`.
pub fn sasacode_home() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("SASACODE_HOME") {
        return Some(PathBuf::from(h));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".sasacode"))
}

/// The real path of `cwd` as Node's `realpathSync` gives it (no `\\?\` prefix on Windows), or
/// the absolute path when it cannot be resolved.
pub fn normalize_cwd(cwd: &Path) -> String {
    let real = fs::canonicalize(cwd)
        .or_else(|_| std::path::absolute(cwd))
        .unwrap_or_else(|_| cwd.to_path_buf());
    let s = real.to_string_lossy().into_owned();
    match s.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => s.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(s),
    }
}

/// The readable part of a session folder's name (`legacyName`).
pub fn legacy_name(cwd: &str) -> String {
    let mut out = String::with_capacity(cwd.len());
    let mut in_sep = false;
    for ch in cwd.chars() {
        if matches!(ch, '\\' | '/' | ':') {
            if !in_sep {
                out.push('-');
            }
            in_sep = true;
        } else {
            out.push(ch);
            in_sep = false;
        }
    }
    out.strip_prefix('-').map(str::to_owned).unwrap_or(out)
}

/// The folder that holds the logs of sessions started in `cwd` (`sessionDir`).
pub fn session_dir(home: &Path, cwd: &Path) -> PathBuf {
    session_dir_of_real_path(home, &normalize_cwd(cwd))
}

/// [`session_dir`] for a path that is already real (resolved, absolute).
pub fn session_dir_of_real_path(home: &Path, real: &str) -> PathBuf {
    let hash = hex8(&Sha256::digest(real.as_bytes()));
    let name = legacy_name(real);
    // JavaScript's slice(-80) counts UTF-16 units.
    let units: Vec<u16> = name.encode_utf16().collect();
    let tail = String::from_utf16_lossy(&units[units.len().saturating_sub(80)..]);
    home.join("sessions").join(format!("{tail}-{hash}"))
}

fn hex8(digest: &[u8]) -> String {
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// One log, summarized for a list of sessions.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    pub path: PathBuf,
    pub cwd: String,
    pub created_at: String,
    pub modified: SystemTime,
    /// The first line of the first prompt.
    pub first_prompt: String,
    pub message_count: usize,
}

/// The sessions started in `cwd`, newest first. Logs that cannot be read are left out.
pub fn list_sessions(home: &Path, cwd: &Path) -> Vec<SessionSummary> {
    let Ok(dir) = fs::read_dir(session_dir(home, cwd)) else {
        return Vec::new();
    };
    let mut out: Vec<SessionSummary> = dir
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|p| summarize(&p))
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.modified));
    out
}

pub fn summarize(path: &Path) -> Option<SessionSummary> {
    let entries = read_entries(path).ok()?;
    let SessionEntry::Session {
        id, cwd, created_at, ..
    } = entries.first()?
    else {
        return None;
    };
    let restored = restore(&entries);
    let first_prompt = restored
        .messages
        .iter()
        .find_map(|m| match m {
            Message::User { content, .. } => Some(text_of(content)),
            _ => None,
        })
        .unwrap_or_default();
    Some(SessionSummary {
        id: id.clone(),
        path: path.to_path_buf(),
        cwd: cwd.clone(),
        created_at: created_at.clone(),
        modified: fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH),
        first_prompt: first_prompt.lines().next().unwrap_or("").to_owned(),
        message_count: restored.messages.len(),
    })
}
