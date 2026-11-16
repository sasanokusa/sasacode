//! A conversation branched off before one of its messages: a new log that holds the old one up to
//! that message, under a new id, which `sasacode -r` then continues. The old log is left as it is.
//! The new one is written as the CLI writes its logs: its first line the old one's, with the new id
//! and time, the rest the old lines as they were (redacted as they were).

use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sasacode_protocol::SessionEntry;

use crate::transcript::Transcript;

/// Writes the branch of the log at `log` before exchange `at` of it, whose message is `prompt`
/// (found by its message when the two disagree, as one sent here may count differently). Its id
/// and where it is.
pub fn fork(log: &Path, at: usize, prompt: &str) -> Result<(String, PathBuf), String> {
    let raw = std::fs::read_to_string(log).map_err(|e| e.to_string())?;
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    // Entries as `read_entries` reads them, each with its line.
    let parsed: Vec<(usize, SessionEntry)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| Some((i, serde_json::from_str(l).ok()?)))
        .collect();
    let entries: Vec<SessionEntry> = parsed.iter().map(|(_, e)| e.clone()).collect();
    let transcript = Transcript::from_entries(&entries);
    let exchange = transcript
        .exchanges
        .get(at)
        .filter(|ex| ex.prompt == prompt)
        .or_else(|| transcript.exchanges.iter().rev().find(|ex| ex.prompt == prompt))
        .ok_or("the message is not in the log")?;
    let entry = exchange.entry.ok_or("the message was compacted away")?;
    let cut = parsed[entry].0;

    let mut head: serde_json::Value =
        serde_json::from_str(lines.first().ok_or("an empty log")?).map_err(|e| e.to_string())?;
    if head["type"] != "session" {
        return Err("not a session log".into());
    }
    let id = new_id();
    let now = SystemTime::now();
    let created = crate::system::iso(now);
    head["id"] = id.clone().into();
    head["createdAt"] = created.clone().into();
    let mut out = format!("{head}\n");
    for line in &lines[1..cut] {
        out.push_str(line);
        out.push('\n');
    }
    let dir = log.parent().ok_or("no folder")?;
    let path = dir.join(format!("{}_{id}.jsonl", created.replace([':', '.'], "-")));
    write_private(&path, &out).map_err(|e| e.to_string())?;
    Ok((id, path))
}

/// Eight hex digits, as the CLI's ids are.
fn new_id() -> String {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    h.write_u32(std::process::id());
    format!("{:08x}", h.finish() as u32)
}

/// A conversation holds file contents and pasted text: readable by its owner only.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_holds_the_log_up_to_the_message() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-fork-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("old.jsonl");
        let user = |text: &str, ts: u64| serde_json::json!({"type":"message","message":{"role":"user","content":[{"type":"text","text":text}],"timestamp":ts}});
        let reply = |text: &str| serde_json::json!({"type":"message","message":{"role":"assistant","content":[{"type":"text","text":text}],"provider":"p","model":"m","usage":{"input":1,"output":1},"stopReason":"stop","timestamp":0}});
        let lines = [
            serde_json::json!({"type":"session","version":3,"id":"aaaa0000","cwd":"/w","createdAt":"2026-01-01T00:00:00.000Z"}),
            user("一つ目", 1),
            reply("はい"),
            user("二つ目", 2),
            reply("どうぞ"),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&log, &text).unwrap();

        let (id, path) = fork(&log, 1, "二つ目").unwrap();
        assert_eq!(id.len(), 8);
        let entries = sasacode_protocol::read_entries(&path).unwrap();
        let t = Transcript::from_entries(&entries);
        assert_eq!(t.exchanges.len(), 1);
        assert_eq!(t.exchanges[0].prompt, "一つ目");
        let SessionEntry::Session { id: written, cwd, .. } = &entries[0] else {
            panic!("no header");
        };
        assert_eq!((written.as_str(), cwd.as_str()), (id.as_str(), "/w"));
        // The old log is as it was; a message that is not there branches nothing.
        assert_eq!(std::fs::read_to_string(&log).unwrap(), text);
        assert!(fork(&log, 0, "ない").is_err());
        // Before the first message: a conversation with nothing in it yet.
        let (_, empty) = fork(&log, 0, "一つ目").unwrap();
        assert!(
            Transcript::from_entries(&sasacode_protocol::read_entries(&empty).unwrap())
                .exchanges
                .is_empty()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
