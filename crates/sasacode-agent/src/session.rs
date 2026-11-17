//! Append-only JSONL session log (`packages/agent/src/session.ts`).
//!
//! Every line is one [`SessionEntry`], written the way `JSON.stringify` wrote it (rust/DECISIONS.md
//! ADR-003): logs written by the TypeScript version resume here, and the other way round.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use sasacode_ai::{AssistantContent, Message, ToolResultMessage, UserContent};
use sasacode_plugin_api::Spend;

pub const SESSION_VERSION: u32 = 1;

/// One line of a session log. Serialized with the tag first and the fields in the order the
/// TypeScript object literals had them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum SessionEntry {
    Session {
        version: u32,
        id: String,
        cwd: String,
        created_at: String,
    },
    Message {
        message: Message,
    },
    Model {
        model: String,
    },
    PermissionMode {
        mode: String,
    },
    Replace {
        messages: Vec<Message>,
    },
    /// What the model actually produced for a call that a tool_call_raw hook repaired.
    ToolRepair {
        call_id: String,
        original: RepairOriginal,
        repaired: Repaired,
        note: String,
    },
    /// What a response outside the conversation used (subagent, side request, discarded reply).
    /// Flattened: `{"type":"usage","t":…,"model":…,"usage":…,"source":…}`.
    Usage(Spend),
    Custom {
        plugin: String,
        kind: String,
        /// A line the TS wrote with `data: undefined` (no key) reads as `null`.
        #[serde(default)]
        data: Value,
    },
}

/// The call as the model produced it (`tool_repair.original`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairOriginal {
    pub name: String,
    #[serde(default)]
    pub input: Value,
    /// The streamed arguments when they did not parse as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<String>,
}

/// The call after the repair (`tool_repair.repaired`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Repaired {
    pub name: String,
    #[serde(default)]
    pub input: Value,
}

/// `realpathSync(resolve(cwd))`, or the resolved path when it does not exist (yet).
pub fn normalize_cwd(cwd: &str) -> String {
    let resolved = resolve(Path::new(cwd));
    let path = match fs::canonicalize(&resolved) {
        Ok(real) => strip_verbatim(real),
        Err(_) => resolved,
    };
    path.to_string_lossy().into_owned()
}

/// Node's `path.resolve` for one argument: absolute against the process cwd, `.` and `..` and
/// repeated or trailing separators removed lexically (before any symlink is looked at).
fn resolve(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            // `pop` never goes above the root, as resolve("/..") is "/".
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `fs::canonicalize` on Windows returns `\\?\C:\…`; Node's realpath does not (dunce semantics).
fn strip_verbatim(p: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return p;
    }
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p
    }
}

/// The folder name used before 0.8: different paths could map to the same one (/a-b/c, /a/b-c).
pub fn legacy_name(cwd: &str) -> String {
    // cwd.replace(/[\\/:]+/g, "-").replace(/^-/, "")
    let mut out = String::with_capacity(cwd.len());
    let mut in_run = false;
    for ch in cwd.chars() {
        if matches!(ch, '\\' | '/' | ':') {
            if !in_run {
                out.push('-');
            }
            in_run = true;
        } else {
            out.push(ch);
            in_run = false;
        }
    }
    match out.strip_prefix('-') {
        Some(rest) => rest.to_string(),
        None => out,
    }
}

/// Readable name plus a hash of the real path, so two directories never share a folder.
pub fn session_dir(root: &Path, cwd: &str) -> PathBuf {
    let real = normalize_cwd(cwd);
    let digest = Sha256::digest(real.as_bytes());
    let hash: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    root.join("sessions")
        .join(format!("{}-{hash}", js_slice_last(&legacy_name(&real), 80)))
}

/// JS `s.slice(-n)`: the last `n` UTF-16 code units. When the cut falls inside a surrogate pair,
/// JS keeps a lone low surrogate, which Node and Bun write to the file system as U+FFFD; the folder
/// name is reproduced byte for byte, so logs written by the TypeScript version stay where they were.
fn js_slice_last(s: &str, n: usize) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    String::from_utf16_lossy(&units[units.len().saturating_sub(n)..])
}

/// JS `s.length`: UTF-16 code units.
fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Append-only JSONL log. Every entry is flushed immediately so a crash loses nothing that was
/// complete. Shared by the agent and the plugin host (`Arc<SessionFile>`): appends from several
/// threads are serialized, each line is written whole.
pub struct SessionFile {
    path: PathBuf,
    id: String,
    state: Mutex<State>,
}

/// Path and id only: the secrets never reach a debug print.
impl std::fmt::Debug for SessionFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionFile")
            .field("path", &self.path)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

struct State {
    /// What this log redacts: a log that takes over its conversation (a fork) redacts them too (#103).
    secrets: Vec<String>,
    /// The header is written with the first real entry, so sessions nobody used leave no file.
    header: Option<SessionEntry>,
}

impl SessionFile {
    fn new(path: PathBuf, id: String, secrets: Vec<String>) -> Self {
        let secrets = secrets.into_iter().filter(|s| js_len(s) >= 8).collect();
        SessionFile {
            path,
            id,
            state: Mutex::new(State { secrets, header: None }),
        }
    }

    /// A new log in `dir` for a session started in `cwd`. Only the folder is created now; the file
    /// appears with the first [`append`](Self::append).
    pub fn create(dir: &Path, cwd: &str, secrets: Vec<String>) -> anyhow::Result<Arc<SessionFile>> {
        // Conversations hold file contents and pasted text: readable by the user only.
        create_private_dir(dir)?;
        let id: String = uuid::Uuid::new_v4().to_string().chars().take(8).collect();
        let created_at = iso_now();
        let path = dir.join(format!("{}_{id}.jsonl", created_at.replace([':', '.'], "-")));
        let f = SessionFile::new(path, id.clone(), secrets);
        f.lock().header = Some(SessionEntry::Session {
            version: SESSION_VERSION,
            id,
            cwd: cwd.to_string(),
            created_at,
        });
        Ok(Arc::new(f))
    }

    /// An existing log, its torn last line set aside first, and the entries it holds.
    pub fn open(path: &Path, secrets: Vec<String>) -> anyhow::Result<(Arc<SessionFile>, Vec<SessionEntry>)> {
        repair_tail(path)?;
        let entries = read_entries(path)?;
        let Some(SessionEntry::Session { id, .. }) = entries.first() else {
            anyhow::bail!("{} is not a session file", path.display());
        };
        let file = SessionFile::new(path.to_path_buf(), id.clone(), secrets);
        Ok((Arc::new(file), entries))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The strings this log redacts (handed to the log that takes over the conversation).
    pub fn secrets(&self) -> Vec<String> {
        self.lock().secrets.clone()
    }

    /// Redact `secret` from every later line. None or shorter than 8 characters: ignored (it
    /// would redact ordinary words).
    pub fn add_secret(&self, secret: Option<&str>) {
        let Some(secret) = secret else { return };
        let mut st = self.lock();
        if js_len(secret) >= 8 && !st.secrets.iter().any(|s| s == secret) {
            st.secrets.push(secret.to_string());
        }
    }

    /// Write `entry` (after the header, on the first call) as one line. Errs when the file cannot
    /// be written (the TS appendFileSync threw).
    pub fn append(&self, entry: &SessionEntry) -> anyhow::Result<()> {
        let mut st = self.lock();
        let header = st.header.take();
        let mut text = String::new();
        if let Some(h) = &header {
            text.push_str(&redacted_line(h, &st.secrets)?);
        }
        text.push_str(&redacted_line(entry, &st.secrets)?);
        let written = append_private(&self.path, text.as_bytes());
        if written.is_err() {
            // Unlike the TS (which dropped it), the header goes with the next entry that is written.
            st.header = header;
        }
        Ok(written?)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// `JSON.stringify(entry)` with every secret replaced, plus the newline.
fn redacted_line(entry: &SessionEntry, secrets: &[String]) -> io::Result<String> {
    let mut line = serde_json::to_string(entry).map_err(io::Error::other)?;
    // Never persist API keys, even if a tool echoed one.
    for s in secrets {
        line = line.replace(s.as_str(), "[REDACTED]");
    }
    line.push('\n');
    Ok(line)
}

/// `new Date().toISOString()`.
fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

/// `appendFileSync(path, data, { mode: 0o600 })`: the mode applies when the file is created.
fn append_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut o = OpenOptions::new();
    o.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)?.write_all(data)
}

/// A crash can leave the last line half written. Appending after it would glue the next entry onto
/// it and lose that too: finish a complete line, or move a torn one to `<file>.torn`.
fn repair_tail(path: &Path) -> io::Result<()> {
    let raw = fs::read(path)?;
    if raw.is_empty() || raw.ends_with(b"\n") {
        return Ok(());
    }
    let cut = raw.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let tail = &raw[cut..];
    if parse_json_lenient::<Value>(&String::from_utf8_lossy(tail)).is_ok() {
        return append_private(path, b"\n");
    }
    let mut torn = path.as_os_str().to_owned();
    torn.push(".torn");
    // The torn bytes as they were (the TS wrote them decoded, invalid UTF-8 as U+FFFD).
    let mut data = tail.to_vec();
    data.push(b'\n');
    append_private(Path::new(&torn), &data)?;
    // A byte offset: the TS measured the decoded text, which is off when the log holds invalid UTF-8.
    OpenOptions::new().write(true).open(path)?.set_len(cut as u64)
}

/// The entries of a log. Lines that do not parse are skipped (a torn last line from a crash).
/// Unlike the TS, which kept any JSON value, a line must also have the shape of an entry.
pub fn read_entries(path: &Path) -> io::Result<Vec<SessionEntry>> {
    // readFileSync(path, "utf8") never fails on bad bytes: they become U+FFFD.
    let raw = fs::read(path)?;
    let text = String::from_utf8_lossy(&raw);
    Ok(text
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| parse_json_lenient(line).ok())
        .collect())
}

/// `JSON.parse` that also takes what JS accepts and serde_json does not: lone UTF-16 surrogates in
/// `\u` escapes (JSON.stringify writes them for strings that hold one). They become U+FFFD, as
/// `toWellFormed()` would make them.
pub fn parse_json_lenient<T: DeserializeOwned>(text: &str) -> serde_json::Result<T> {
    match serde_json::from_str(text) {
        Ok(v) => Ok(v),
        Err(e) => match replace_lone_surrogates(text) {
            Some(fixed) => serde_json::from_str(&fixed),
            None => Err(e),
        },
    }
}

/// `text` with every `\u` escape of a lone surrogate replaced by `\ufffd`; None when it has none.
/// Escapes are read in order, so the `\\` of an escaped backslash is never taken for the start of
/// one (`"\\ud800"` is the text `\ud800`, not a surrogate).
fn replace_lone_surrogates(text: &str) -> Option<String> {
    fn hex4(b: &[u8]) -> Option<u16> {
        let s = std::str::from_utf8(b.get(..4)?).ok()?;
        if !s.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        u16::from_str_radix(s, 16).ok()
    }
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut changed = false;
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        if b.get(i + 1) != Some(&b'u') {
            // Another escape: copy it whole, so its second character is not read as a new one.
            out.extend_from_slice(&b[i..(i + 2).min(b.len())]);
            i += 2;
            continue;
        }
        let Some(unit) = hex4(&b[i + 2..]) else {
            out.extend_from_slice(&b[i..i + 2]);
            i += 2;
            continue;
        };
        let low_follows = || {
            b.get(i + 6) == Some(&b'\\')
                && b.get(i + 7) == Some(&b'u')
                && hex4(b.get(i + 8..).unwrap_or_default()).is_some_and(|u| (0xDC00..=0xDFFF).contains(&u))
        };
        if (0xD800..=0xDBFF).contains(&unit) && low_follows() {
            out.extend_from_slice(&b[i..i + 12]);
            i += 12;
        } else if (0xD800..=0xDFFF).contains(&unit) {
            out.extend_from_slice(b"\\ufffd");
            changed = true;
            i += 6;
        } else {
            out.extend_from_slice(&b[i..i + 6]);
            i += 6;
        }
    }
    if !changed {
        return None;
    }
    // Only ASCII escapes were replaced, at ASCII positions: still UTF-8.
    String::from_utf8(out).ok()
}

/// The conversation and the model to continue with, rebuilt from entries.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Restored {
    pub messages: Vec<Message>,
    /// `provider/model`.
    pub model: Option<String>,
    /// Calls without a result were settled: the caller persists `messages` (a replace entry), so
    /// the next restore sees the same history.
    pub repaired: bool,
}

/// The conversation and the model to continue with, rebuilt from entries. Tool calls that never got
/// a result (a crash mid-turn) get one saying so; `repaired` tells the caller to persist that (a
/// replace entry), so the next restore sees the same history. `permission_mode` entries are a record
/// of what the user chose, not restored: resuming never starts in a looser mode than the one asked for now.
pub fn restore(entries: &[SessionEntry]) -> Restored {
    let mut messages: Vec<Message> = Vec::new();
    let mut model: Option<String> = None;
    for e in entries {
        match e {
            SessionEntry::Message { message } => {
                messages.push(message.clone());
                // The model that answered last is the one to continue with.
                if let Message::Assistant(a) = message {
                    model = Some(format!("{}/{}", a.provider, a.model));
                }
            }
            SessionEntry::Replace { messages: m } => messages = m.clone(),
            SessionEntry::Model { model: m } => model = Some(m.clone()),
            _ => {}
        }
    }
    let before = messages.len();
    let settled = settle_tool_calls(messages);
    let repaired = settled.len() != before;
    Restored {
        messages: settled,
        model,
        repaired,
    }
}

/// No result was saved, which does not mean the call did nothing: it may have run just before a crash.
pub const NO_RESULT: &str = "[Result unknown: the session ended before this call's result was saved. It may or may not have run. If it changes anything, check the current state before running it again.]";

/// Every tool call must be followed by its result before the next user or assistant message
/// (all APIs reject history otherwise). Add an error result for each call that has none,
/// anywhere in the history.
pub fn settle_tool_calls(messages: Vec<Message>) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    let mut it = messages.into_iter().peekable();
    while let Some(m) = it.next() {
        let Message::Assistant(a) = &m else {
            out.push(m);
            continue;
        };
        let calls: Vec<(String, String)> = a
            .content
            .iter()
            .filter_map(|c| match c {
                AssistantContent::ToolCall(c) => Some((c.id.clone(), c.name.clone())),
                _ => None,
            })
            .collect();
        let timestamp = a.timestamp;
        out.push(m);
        if calls.is_empty() {
            continue;
        }
        // The results of this turn are the tool messages right after it.
        let mut answered: Vec<String> = Vec::new();
        while let Some(Message::Tool(r)) = it.peek() {
            answered.push(r.tool_call_id.clone());
            out.extend(it.next());
        }
        for (id, name) in calls {
            if !answered.contains(&id) {
                out.push(Message::Tool(ToolResultMessage {
                    tool_call_id: id,
                    tool_name: name,
                    content: vec![UserContent::text(NO_RESULT)],
                    is_error: true,
                    timestamp,
                }));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sasacode_ai::{AssistantMessage, StopReason, ToolCall, Usage, UserMessage};
    use sasacode_plugin_api::SpendSource;

    /// A fresh directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("sasacode-{tag}-{}", uuid::Uuid::new_v4().simple()));
            fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn user(text: &str) -> Message {
        Message::User(UserMessage {
            content: vec![UserContent::text(text)],
            timestamp: 0,
        })
    }
    fn asst(content: Vec<AssistantContent>, stop: StopReason) -> Message {
        Message::Assistant(AssistantMessage {
            content,
            api: "x".into(),
            provider: "t".into(),
            model: "m".into(),
            usage: Usage::default(),
            stop_reason: stop,
            timestamp: 0,
            replay_scope: None,
            error_message: None,
        })
    }
    fn call(id: &str, name: &str) -> AssistantContent {
        AssistantContent::ToolCall(ToolCall {
            id: id.into(),
            name: name.into(),
            input: Default::default(),
            raw_input: None,
        })
    }
    fn msg(m: Message) -> SessionEntry {
        SessionEntry::Message { message: m }
    }
    fn roles(ms: &[Message]) -> Vec<&'static str> {
        ms.iter().map(Message::role).collect()
    }
    fn types(entries: &[SessionEntry]) -> Vec<&'static str> {
        entries
            .iter()
            .map(|e| match e {
                SessionEntry::Session { .. } => "session",
                SessionEntry::Message { .. } => "message",
                SessionEntry::Model { .. } => "model",
                SessionEntry::PermissionMode { .. } => "permission_mode",
                SessionEntry::Replace { .. } => "replace",
                SessionEntry::ToolRepair { .. } => "tool_repair",
                SessionEntry::Usage(_) => "usage",
                SessionEntry::Custom { .. } => "custom",
            })
            .collect()
    }

    // boundaries.test.ts: "restoring twice gives the same valid history once the repair is recorded"
    #[test]
    fn restoring_twice_gives_the_same_valid_history_once_the_repair_is_recorded() {
        let root = TempDir::new("restore");
        let s = SessionFile::create(&root.0.join("sessions"), "/proj", vec![]).unwrap();
        s.append(&msg(user("go"))).unwrap();
        s.append(&msg(asst(vec![call("dangling", "bash")], StopReason::ToolUse)))
            .unwrap();
        let first = restore(&SessionFile::open(s.path(), vec![]).unwrap().1);
        assert!(first.repaired);
        s.append(&SessionEntry::Replace {
            messages: first.messages.clone(),
        })
        .unwrap(); // what setup does after a repair
        s.append(&msg(user("next"))).unwrap();
        s.append(&msg(asst(vec![AssistantContent::text("ok")], StopReason::Stop)))
            .unwrap();
        let second = restore(&SessionFile::open(s.path(), vec![]).unwrap().1);
        assert!(!second.repaired);
        assert_eq!(
            roles(&second.messages),
            ["user", "assistant", "tool", "user", "assistant"]
        );
        // Even without the recorded repair, a dangling call in the middle is settled, not left behind.
        let log = [user("a"), asst(vec![call("x", "bash")], StopReason::ToolUse), user("b")];
        let settled = restore(&log.map(msg)).messages;
        assert_eq!(roles(&settled), ["user", "assistant", "tool", "user"]);
    }

    // boundaries.test.ts: "a torn last line is set aside before the session is appended to"
    #[test]
    fn a_torn_last_line_is_set_aside_before_the_session_is_appended_to() {
        let root = TempDir::new("torn");
        let s = SessionFile::create(&root.0.join("sessions-torn"), "/proj", vec![]).unwrap();
        s.append(&msg(user("one"))).unwrap();
        append_private(s.path(), br#"{"type":"message","mess"#).unwrap();
        let (file, _) = SessionFile::open(s.path(), vec![]).unwrap();
        file.append(&msg(user("two"))).unwrap();
        let texts: Vec<String> = SessionFile::open(s.path(), vec![])
            .unwrap()
            .1
            .iter()
            .filter_map(|e| match e {
                SessionEntry::Message { message } => Some(message.text()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["one", "two"]);
        let mut torn = s.path().as_os_str().to_owned();
        torn.push(".torn");
        assert!(fs::read_to_string(&torn).unwrap().contains("\"mess"));
        // A complete entry that only lacks its newline is kept.
        append_private(s.path(), br#"{"type":"model","model":"t/m"}"#).unwrap();
        SessionFile::open(s.path(), vec![])
            .unwrap()
            .0
            .append(&SessionEntry::PermissionMode { mode: "ask".into() })
            .unwrap();
        let entries = SessionFile::open(s.path(), vec![]).unwrap().1;
        assert_eq!(
            types(&entries),
            ["session", "message", "message", "model", "permission_mode"]
        );
    }

    // boundaries.test.ts: "directories whose names flatten the same get separate session folders"
    #[test]
    fn directories_whose_names_flatten_the_same_get_separate_session_folders() {
        let home = Path::new("/nonexistent-home");
        assert_ne!(session_dir(home, "/work/a-b/c"), session_dir(home, "/work/a/b-c"));
    }

    // Expected values computed by the TS (bun, packages/agent/src/session.ts) for paths that do not exist.
    // POSIX paths: on Windows, path.resolve puts a drive in front.
    #[cfg(unix)]
    #[test]
    fn session_dir_matches_the_typescript_names() {
        let root = Path::new("/root");
        let dir = |c: &str| session_dir(root, c).to_string_lossy().into_owned();
        assert_eq!(dir("/work/a-b/c"), "/root/sessions/work-a-b-c-6b919627");
        assert_eq!(dir("/work/a/b-c"), "/root/sessions/work-a-b-c-0fce0ba0");
        let long = format!("/nonexistent/{}", "日本語のとても長いディレクトリ名".repeat(6));
        assert_eq!(
            dir(&long),
            format!(
                "/root/sessions/{}-f4be62c7",
                "日本語のとても長いディレクトリ名".repeat(5)
            )
        );
        // An emoji that ends exactly at the 80-unit cut stays whole.
        let whole = format!("/nonexistent/{}😀{}", "a".repeat(100), "b".repeat(78));
        assert_eq!(dir(&whole), format!("/root/sessions/😀{}-b0ee9352", "b".repeat(78)));
        // One the cut splits: JS keeps its low surrogate, which lands on disk as U+FFFD.
        let split = format!("/nonexistent/{}😀{}", "a".repeat(100), "b".repeat(79));
        assert_eq!(
            dir(&split),
            format!("/root/sessions/\u{FFFD}{}-3bd34cc0", "b".repeat(79))
        );
        // path.resolve normalises before hashing; legacyName joins runs of separators.
        assert_eq!(dir("/nonexistent/../x/./y//z/"), "/root/sessions/x-y-z-2230b486");
        assert_eq!(dir("/no:exist\\here//x"), "/root/sessions/no-exist-here-x-07adef76");
        assert_eq!(dir("/"), "/root/sessions/-8a5edab2");
        assert_eq!(legacy_name("/nonexistent/../x/./y//z/"), "nonexistent-..-x-.-y-z-");
        assert_eq!(legacy_name("/"), "");
    }

    // POSIX paths: on Windows, path.resolve puts a drive in front.
    #[cfg(unix)]
    #[test]
    fn normalize_cwd_resolves_symlinks_and_relative_paths() {
        let root = TempDir::new("norm");
        let real = fs::canonicalize(&root.0).unwrap();
        fs::create_dir(real.join("d")).unwrap();
        let s = |p: PathBuf| p.to_string_lossy().into_owned();
        assert_eq!(normalize_cwd(&s(root.0.join("d/../d/."))), s(real.join("d")));
        // A missing path is resolved lexically, not dropped.
        assert_eq!(normalize_cwd("/nonexistent/a/../b"), "/nonexistent/b");
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(normalize_cwd("."), s(fs::canonicalize(cwd).unwrap()));
    }

    // Lines as JSON.stringify wrote them (bun, packages/agent/src/session.ts).
    #[test]
    fn entries_serialize_like_json_stringify() {
        let lines = [
            r#"{"type":"session","version":1,"id":"abcd1234","cwd":"/work/日本","createdAt":"2026-11-18T00:00:00.000Z"}"#,
            r#"{"type":"tool_repair","callId":"c1","original":{"name":"read_file","input":{"path":"a","n":1.5}},"repaired":{"name":"read","input":{"path":"a"}},"note":"tool \"read_file\" → \"read\""}"#,
            r#"{"type":"tool_repair","callId":"c2","original":{"name":"x","input":{},"rawInput":"{bad"},"repaired":{"name":"x","input":{}},"note":"arguments repaired"}"#,
            r#"{"type":"usage","t":5,"model":"p/m","usage":{"input":1,"output":2,"cacheRead":3,"cacheWrite":4,"cost":0.5},"source":"subagent"}"#,
            r#"{"type":"custom","plugin":"goal","kind":"goal","data":null}"#,
            "{\"type\":\"custom\",\"plugin\":\"p\",\"kind\":\"k\",\"data\":{\"a\":[1,\"x\\u0001\u{2028}\"]}}",
            r#"{"type":"permission_mode","mode":"ask"}"#,
            r#"{"type":"model","model":"t/m"}"#,
        ];
        for line in lines {
            let e: SessionEntry = serde_json::from_str(line).unwrap();
            assert_eq!(serde_json::to_string(&e).unwrap(), line);
        }
        let usage: SessionEntry = serde_json::from_str(lines[3]).unwrap();
        let SessionEntry::Usage(s) = usage else {
            panic!("not a usage entry")
        };
        assert_eq!((s.t, s.source, s.usage.cache_read), (5, SpendSource::Subagent, 3));
        // data: undefined (no key) reads as null.
        let missing = r#"{"type":"custom","plugin":"p","kind":"k"}"#;
        let SessionEntry::Custom { data, .. } = serde_json::from_str(missing).unwrap() else {
            panic!("not custom")
        };
        assert_eq!(data, Value::Null);
    }

    // settleToolCalls output computed by the TS for the same input.
    #[test]
    fn settle_tool_calls_adds_the_no_result_text_after_the_answered_ones() {
        let input = r#"[{"role":"assistant","content":[{"type":"text","text":"hi"},{"type":"tool_call","id":"1","name":"bash","input":{}},{"type":"tool_call","id":"2","name":"read","input":{}}],"api":"x","provider":"t","model":"m","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"cost":0},"stopReason":"tool_use","timestamp":7},{"role":"tool","toolCallId":"1","toolName":"bash","content":[{"type":"text","text":"ok"}],"isError":false,"timestamp":8},{"role":"user","content":[{"type":"text","text":"next"}],"timestamp":9}]"#;
        let expected = r#"[{"role":"assistant","content":[{"type":"text","text":"hi"},{"type":"tool_call","id":"1","name":"bash","input":{}},{"type":"tool_call","id":"2","name":"read","input":{}}],"api":"x","provider":"t","model":"m","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"cost":0},"stopReason":"tool_use","timestamp":7},{"role":"tool","toolCallId":"1","toolName":"bash","content":[{"type":"text","text":"ok"}],"isError":false,"timestamp":8},{"role":"tool","toolCallId":"2","toolName":"read","content":[{"type":"text","text":"[Result unknown: the session ended before this call's result was saved. It may or may not have run. If it changes anything, check the current state before running it again.]"}],"isError":true,"timestamp":7},{"role":"user","content":[{"type":"text","text":"next"}],"timestamp":9}]"#;
        let messages: Vec<Message> = serde_json::from_str(input).unwrap();
        assert_eq!(serde_json::to_string(&settle_tool_calls(messages)).unwrap(), expected);
    }

    // agent.test.ts: "sessions persist every message, redact secrets, and resume to the last complete
    // turn" and "the key in use is redacted from the first message too (#34)", at the level of the log.
    #[test]
    fn secrets_are_redacted_from_every_line_the_header_included() {
        let root = TempDir::new("secrets");
        let dir = root.0.join("sessions");
        // Too short to redact: ignored, here and in add_secret.
        let s = SessionFile::create(&dir, "/proj/sk-secret-123456", vec!["short".into()]).unwrap();
        s.add_secret(Some("sk-secret-123456"));
        s.add_secret(Some("sk-secret-123456"));
        s.add_secret(Some("1234567"));
        s.add_secret(None);
        assert_eq!(s.secrets(), ["sk-secret-123456"]);
        s.append(&msg(user("first sk-secret-123456"))).unwrap();
        let raw = fs::read_to_string(s.path()).unwrap();
        assert!(!raw.contains("sk-secret-123456"));
        assert!(raw.contains("first [REDACTED]"));
        assert!(raw.contains(r#""cwd":"/proj/[REDACTED]""#));
        // A log that takes over the conversation redacts them too (#103).
        let fork = SessionFile::create(&dir, "/proj", s.secrets()).unwrap();
        fork.append(&msg(user("again sk-secret-123456"))).unwrap();
        assert!(fs::read_to_string(fork.path()).unwrap().contains("again [REDACTED]"));
        let (opened, _) = SessionFile::open(s.path(), vec!["sk-secret-123456".into(), "tiny".into()]).unwrap();
        assert_eq!(opened.secrets(), ["sk-secret-123456"]);
        assert_eq!(opened.id(), s.id());
        assert!(!format!("{opened:?}").contains("sk-secret"));
    }

    #[test]
    fn a_crash_mid_turn_resumes_with_an_unknown_result_for_the_dangling_call() {
        let root = TempDir::new("crash");
        let s = SessionFile::create(&root.0.join("sessions"), "/proj", vec![]).unwrap();
        s.append(&msg(user("print it"))).unwrap();
        let mut reply = asst(vec![call("9", "bash")], StopReason::ToolUse);
        if let Message::Assistant(a) = &mut reply {
            a.timestamp = 42;
        }
        s.append(&msg(reply)).unwrap();
        append_private(s.path(), br#"{"type":"mess"#).unwrap(); // torn line
        let again = restore(&SessionFile::open(s.path(), vec![]).unwrap().1);
        assert!(again.repaired);
        assert_eq!(again.model.as_deref(), Some("t/m"));
        let Some(Message::Tool(last)) = again.messages.last() else {
            panic!("no tool result")
        };
        assert_eq!(
            (last.tool_call_id.as_str(), last.tool_name.as_str(), last.is_error),
            ("9", "bash", true)
        );
        assert_eq!(last.timestamp, 42);
        assert_eq!(text_of(&last.content), NO_RESULT);
    }

    fn text_of(c: &[UserContent]) -> String {
        c.iter().filter_map(UserContent::as_text).collect()
    }

    #[test]
    fn the_header_is_written_with_the_first_entry_and_the_file_is_private() {
        let root = TempDir::new("header");
        let dir = root.0.join("a/b");
        let s = SessionFile::create(&dir, "/proj", vec![]).unwrap();
        assert!(dir.is_dir());
        assert!(!s.path().exists(), "no file before the first entry");
        let name = s.path().file_name().unwrap().to_string_lossy().into_owned();
        // 2026-11-18T01-02-03-456Z_abcd1234.jsonl
        assert_eq!(name.len(), "2026-11-18T01-02-03-456Z_abcd1234.jsonl".len());
        assert!(name.ends_with(&format!("_{}.jsonl", s.id())));
        assert!(!name.contains(':') && name.matches('.').count() == 1);
        assert_eq!(s.id().len(), 8);
        s.append(&SessionEntry::Model { model: "t/m".into() }).unwrap();
        s.append(&SessionEntry::Model { model: "t/n".into() }).unwrap();
        let entries = read_entries(s.path()).unwrap();
        assert_eq!(types(&entries), ["session", "model", "model"]);
        let SessionEntry::Session {
            version,
            id,
            cwd,
            created_at,
        } = &entries[0]
        else {
            panic!("no header")
        };
        assert_eq!(
            (*version, id.as_str(), cwd.as_str()),
            (SESSION_VERSION, s.id(), "/proj")
        );
        assert_eq!(created_at.replace([':', '.'], "-"), name.split('_').next().unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(s.path()).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
        assert!(SessionFile::open(&dir.join("missing.jsonl"), vec![]).is_err());
        fs::write(dir.join("plain.jsonl"), "{\"type\":\"model\",\"model\":\"x\"}\n").unwrap();
        let e = SessionFile::open(&dir.join("plain.jsonl"), vec![]).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!("{} is not a session file", dir.join("plain.jsonl").display())
        );
    }

    #[test]
    fn appends_from_many_threads_stay_whole_lines() {
        let root = TempDir::new("threads");
        let s = SessionFile::create(&root.0, "/proj", vec![]).unwrap();
        let handles: Vec<_> = (0..8)
            .map(|t| {
                let s = s.clone();
                std::thread::spawn(move || {
                    for i in 0..50 {
                        s.append(&SessionEntry::Model {
                            model: format!("{t}/{i}"),
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let entries = read_entries(s.path()).unwrap();
        assert_eq!(entries.len(), 401);
        assert_eq!(types(&entries[..1]), ["session"]);
    }

    #[test]
    fn lone_surrogates_are_read_as_replacement_characters() {
        let parse = |s: &str| parse_json_lenient::<Value>(s).map(|v| v.as_str().unwrap_or_default().to_string());
        assert_eq!(parse(r#""a\ud800b""#).unwrap(), "a\u{FFFD}b"); // lone high
        assert_eq!(parse(r#""a\udc00b""#).unwrap(), "a\u{FFFD}b"); // lone low
        assert_eq!(parse(r#""\ud83d\ude00""#).unwrap(), "😀"); // a valid pair
        assert_eq!(parse(r#""\\ud800""#).unwrap(), "\\ud800"); // an escaped backslash, then text
        assert_eq!(parse(r#""\\\ud800""#).unwrap(), "\\\u{FFFD}");
        assert_eq!(parse(r#""\ud800\u0041""#).unwrap(), "\u{FFFD}A"); // high, then not a low
        assert_eq!(parse(r#""\ud800\ud800\udc00""#).unwrap(), "\u{FFFD}𐀀");
        assert!(parse(r#""\ud800"#).is_err()); // still not JSON
        assert_eq!(replace_lone_surrogates(r#""\\ud800 \ud83d\ude00""#), None);
        let root = TempDir::new("surrogate");
        let path = root.0.join("s.jsonl");
        fs::write(
            &path,
            "{\"type\":\"session\",\"version\":1,\"id\":\"i\",\"cwd\":\"/\",\"createdAt\":\"\"}\n{\"type\":\"model\",\"model\":\"a\\udc00\"}\n",
        )
        .unwrap();
        let entries = read_entries(&path).unwrap();
        assert_eq!(
            entries[1],
            SessionEntry::Model {
                model: "a\u{FFFD}".into()
            }
        );
    }

    #[test]
    fn invalid_utf8_does_not_make_a_log_unreadable() {
        let root = TempDir::new("utf8");
        let path = root.0.join("s.jsonl");
        let mut data = b"{\"type\":\"session\",\"version\":1,\"id\":\"i\",\"cwd\":\"/\",\"createdAt\":\"\"}\n".to_vec();
        data.extend_from_slice(b"{\"type\":\"model\",\"model\":\"a\xff\"}\n{\"type\":\"model\",\"model\":\"\xe6\x97");
        fs::write(&path, &data).unwrap();
        let (_, entries) = SessionFile::open(&path, vec![]).unwrap();
        assert_eq!(
            entries[1],
            SessionEntry::Model {
                model: "a\u{FFFD}".into()
            }
        );
        assert_eq!(entries.len(), 2);
        // The torn tail (cut inside a character) is moved aside byte for byte.
        let mut torn = path.as_os_str().to_owned();
        torn.push(".torn");
        assert_eq!(fs::read(&torn).unwrap(), b"{\"type\":\"model\",\"model\":\"\xe6\x97\n");
        assert!(fs::read(&path).unwrap().ends_with(b"\"a\xff\"}\n"));
    }

    /// Every line of the real logs under ~/.sasacode/sessions (read-only: lines are only parsed,
    /// nothing is opened for writing) deserializes, and serializes back to the same JSON value.
    /// Failures name the file and line, never the content: the logs are private.
    #[test]
    fn real_session_logs_round_trip() {
        let root = sasacode_plugin_api::sasacode_home().join("sessions");
        let Ok(dirs) = fs::read_dir(&root) else { return };
        let mut files: Vec<PathBuf> = dirs
            .flatten()
            .filter_map(|d| fs::read_dir(d.path()).ok())
            .flat_map(|rd| rd.flatten().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        files.sort();
        let (mut lines, mut same, mut number_format, mut key_order) = (0, 0, 0, 0);
        let mut failures: Vec<String> = Vec::new();
        for f in &files {
            let Ok(raw) = fs::read(f) else { continue };
            let text = String::from_utf8_lossy(&raw);
            let name = f.file_name().unwrap_or_default().to_string_lossy().into_owned();
            for (no, line) in text.split('\n').enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                // Not JSON at all (a torn line): the TS skips it as well.
                let Ok(original) = parse_json_lenient::<Value>(line) else {
                    continue;
                };
                lines += 1;
                // The parse read_entries uses.
                let entry = match parse_json_lenient::<SessionEntry>(line) {
                    Ok(e) => e,
                    Err(_) => {
                        failures.push(format!("{name}:{} does not parse as an entry", no + 1));
                        continue;
                    }
                };
                let written = serde_json::to_string(&entry).unwrap();
                let back: Value = serde_json::from_str(&written).unwrap();
                if back != original {
                    failures.push(format!("{name}:{} changes in a round trip", no + 1));
                } else if written == line {
                    same += 1;
                } else if serde_json::to_string(&original).unwrap() != line {
                    // Without serde_json's float_roundtrip feature some floats parse one ulp off
                    // (a float printed with 17 significant digits can come back as a shorter one).
                    number_format += 1;
                } else {
                    // The TS wrote some objects with their keys in another order (compaction's user messages).
                    key_order += 1;
                }
            }
        }
        eprintln!(
            "real logs: {} files, {lines} entries: {same} byte-identical, {number_format} differ in a float only, {key_order} in key order only",
            files.len()
        );
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
