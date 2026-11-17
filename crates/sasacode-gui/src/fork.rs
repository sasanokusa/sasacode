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
    // Shown, a message is as it was typed; the log has the image plugin's labels in it.
    let same = |ex: &&crate::transcript::Exchange| crate::transcript::sent_as(prompt, &ex.prompt);
    let exchange = transcript
        .exchanges
        .get(at)
        .filter(same)
        .or_else(|| transcript.exchanges.iter().rev().find(same))
        .ok_or("the message is not in the log")?;
    let entry = exchange.entry.ok_or("the message was compacted away")?;
    branch(log, &lines, parsed[entry].0)
}

/// A copy of the whole log at `log` under a new id: the conversation branched off after its last
/// reply.
pub fn copy_whole(log: &Path) -> Result<(String, PathBuf), String> {
    let raw = std::fs::read_to_string(log).map_err(|e| e.to_string())?;
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    branch(log, &lines, lines.len())
}

/// Writes `lines` of the log at `log` up to line `cut` beside it, under a new id and time.
fn branch(log: &Path, lines: &[&str], cut: usize) -> Result<(String, PathBuf), String> {
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
            user("[Image 1: /w/shot.png] これを見て", 3),
            reply("見ました"),
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
        // A message with an image is shown as it was typed; the log has the image plugin's label.
        let (_, path) = fork(&log, 2, "@shot.png これを見て").unwrap();
        let t = Transcript::from_entries(&sasacode_protocol::read_entries(&path).unwrap());
        assert_eq!(t.exchanges.len(), 2);
        // Before the first message: a conversation with nothing in it yet.
        let (_, empty) = fork(&log, 0, "一つ目").unwrap();
        assert!(
            Transcript::from_entries(&sasacode_protocol::read_entries(&empty).unwrap())
                .exchanges
                .is_empty()
        );
        // After the last reply: all of it, under a new id.
        let (whole_id, whole) = copy_whole(&log).unwrap();
        let entries = sasacode_protocol::read_entries(&whole).unwrap();
        assert_eq!(Transcript::from_entries(&entries).exchanges.len(), 3);
        assert!(matches!(&entries[0], SessionEntry::Session { id, .. } if *id == whole_id));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The real CLI against a fake model: `SASACODE_FAKE` runs it, in the project
    /// `SASACODE_FAKE_PROJECT`. A branched log is continued with `-r` and holds what came before
    /// it; `--no-session` (a side question) leaves the log as it was; the session line says the
    /// context window.
    #[test]
    #[ignore = "needs a fake model server and a CLI pointed at it"]
    fn the_cli_continues_a_branch_and_keeps_a_side_question_out_of_the_log() {
        let (Ok(cli), Ok(project)) = (std::env::var("SASACODE_FAKE"), std::env::var("SASACODE_FAKE_PROJECT")) else {
            panic!("set SASACODE_FAKE and SASACODE_FAKE_PROJECT");
        };
        let run = |args: &[&str]| -> serde_json::Value {
            let out = std::process::Command::new(&cli)
                .args(args)
                .args(["--output", "jsonl"])
                .current_dir(&project)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let text = String::from_utf8(out.stdout).unwrap();
            serde_json::from_str(text.lines().next().unwrap()).unwrap()
        };
        let first = run(&["-p", "一つ目"]);
        assert_eq!(first["type"], "session");
        assert!(first["contextWindow"].as_u64().is_some_and(|n| n > 0), "{first}");
        let id = first["id"].as_str().unwrap().to_owned();
        let log = PathBuf::from(first["path"].as_str().unwrap());
        run(&["-r", &id, "-p", "二つ目"]);
        let prompts = |path: &Path| -> Vec<String> {
            Transcript::from_entries(&sasacode_protocol::read_entries(path).unwrap())
                .exchanges
                .iter()
                .map(|e| e.prompt.clone())
                .collect()
        };
        assert_eq!(prompts(&log), ["一つ目", "二つ目"]);

        // A side question reads the conversation and writes nothing.
        let before = std::fs::read(&log).unwrap();
        let side = run(&["-r", &id, "--no-session", "-p", "脇の質問"]);
        assert_eq!(side["type"], "session");
        assert_eq!(std::fs::read(&log).unwrap(), before);

        // The second message, edited and sent again on a branch.
        let (branch, path) = fork(&log, 1, "二つ目").unwrap();
        let again = run(&["-r", &branch, "-p", "直した二つ目"]);
        assert_eq!(again["id"].as_str(), Some(branch.as_str()));
        assert_eq!(PathBuf::from(again["path"].as_str().unwrap()), path);
        assert_eq!(prompts(&path), ["一つ目", "直した二つ目"]);
        assert_eq!(std::fs::read(&log).unwrap(), before);
    }
}
