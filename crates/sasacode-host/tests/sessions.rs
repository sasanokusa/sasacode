// packages/host/test/sessions.test.ts: the session index: listings cost one stat per log instead of
// re-reading every session, changed logs alone are re-parsed, and an id finds its session from any
// directory. Each test has its own sasacode home (the TS ones shared one and ran in order).
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sasacode_agent::session::{
    SessionEntry, SessionFile, legacy_name, normalize_cwd, read_entries, restore, session_dir,
};
use sasacode_ai::{Message, UserContent};
use sasacode_host::{SessionInfo, SessionUsage, find_session, list_sessions, load_session_index};
use serde_json::{Value, json};

fn header(id: &str, cwd: &str) -> String {
    json!({ "type": "session", "version": 1, "id": id, "cwd": cwd, "createdAt": "2026-09-24T00:00:00.000Z" })
        .to_string()
}
fn user(text: &str, t: i64) -> String {
    json!({ "type": "message", "message": { "role": "user", "content": [{ "type": "text", "text": text }], "timestamp": t } })
        .to_string()
}
fn assistant(t: i64, model: &str) -> String {
    json!({ "type": "message", "message": { "role": "assistant", "provider": "p", "model": model, "content": [], "api": "chat",
        "stopReason": "stop", "timestamp": t, "usage": { "input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0, "cost": 0.01 } } })
    .to_string()
}

fn write_log(root: &Path, name: &str, lines: &[String]) -> PathBuf {
    // Joined part by part, as Node's join writes `a/one.jsonl` on Windows (`a\one.jsonl`): the
    // index is keyed by the path the listing finds.
    let path = name.split('/').fold(root.to_path_buf(), |p, part| p.join(part));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    path
}

fn key(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Whole-second stamps: mtimes survive set_modified exactly, so the staleness test is deterministic.
fn stamp(offset_secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(1_758_000_000_000) + Duration::from_secs(offset_secs)
}

fn utimes(path: &Path, t: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

fn read_index(root: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(root.join(".index.json")).unwrap()).unwrap()
}

#[test]
fn the_index_summarizes_a_log_id_cwd_first_prompt_message_count_and_usage() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("sessions");
    // A subagent's response is logged as a "usage" entry: counted, not a message.
    let sub = json!({ "type": "usage", "t": 4, "model": "p/s", "source": "subagent",
        "usage": { "input": 7, "output": 3, "cacheRead": 100, "cacheWrite": 20, "cost": 0.5 } })
    .to_string();
    let path = write_log(
        &root,
        "a/one.jsonl",
        &[
            header("aaaa1111", "/work/a"),
            user("first prompt", 1),
            assistant(2, "m"),
            user("more", 3),
            sub,
        ],
    );
    let index = load_session_index(&root);
    let s = &index[&key(&path)];
    assert_eq!(
        (
            s.id.as_str(),
            s.cwd.as_str(),
            s.created_at.as_str(),
            s.first_prompt.as_str(),
            s.message_count
        ),
        ("aaaa1111", "/work/a", "2026-09-24T00:00:00.000Z", "first prompt", 3)
    );
    let usage = |t, model: &str, input, output, cache_read, cache_write, cost| SessionUsage {
        t,
        model: model.into(),
        input,
        output,
        cache_read,
        cache_write,
        cost,
    };
    assert_eq!(
        s.usage,
        [usage(2, "p/m", 10, 5, 0, 0, 0.01), usage(4, "p/s", 7, 3, 100, 20, 0.5)]
    );
    // The written index is what later listings read.
    let file = read_index(&root);
    assert_eq!(file["version"], 1);
    assert_eq!(file["sessions"][key(&path)]["id"], "aaaa1111");
    // In the TS shape: keys in its order, integers without a fraction.
    let written = fs::read_to_string(root.join(".index.json")).unwrap();
    let keys: Vec<&String> = file["sessions"][key(&path)].as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "id",
            "cwd",
            "createdAt",
            "size",
            "modifiedAt",
            "firstPrompt",
            "messageCount",
            "usage"
        ]
    );
    assert!(written.starts_with(r#"{"version":1,"sessions":{"#));
    assert!(
        written.contains(r#"{"t":2,"model":"p/m","input":10,"output":5,"cacheRead":0,"cacheWrite":0,"cost":0.01}"#)
    );
}

#[test]
fn unchanged_logs_are_served_from_the_index_only_changed_logs_are_re_parsed() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("sessions");
    let a = write_log(
        &root,
        "inc/a.jsonl",
        &[header("aaaa2222", "/work/b"), user("prompt a", 1), assistant(2, "m")],
    );
    let b = write_log(
        &root,
        "inc/b.jsonl",
        &[header("bbbb2222", "/work/b"), user("prompt b", 1), assistant(2, "m")],
    );
    for f in [&a, &b] {
        utimes(f, stamp(0));
    }
    assert_eq!(load_session_index(&root)[&key(&a)].message_count, 2);
    // b now reads differently on disk, but its size and mtime are unchanged, so a re-parse would
    // only waste time (logs are append-only in practice): the summary keeps its values.
    fs::write(
        &b,
        [header("bbbb2222", "/work/b"), user("PROMPT b", 1), assistant(2, "m")].join("\n") + "\n",
    )
    .unwrap();
    utimes(&b, stamp(0));
    // a really changed (appended to), so it alone is re-read.
    fs::write(
        &a,
        [
            header("aaaa2222", "/work/b"),
            user("prompt a", 1),
            assistant(2, "m"),
            user("appended", 3),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    let index = load_session_index(&root);
    assert_eq!(index[&key(&b)].first_prompt, "prompt b");
    assert_eq!(index[&key(&a)].first_prompt, "prompt a");
    assert_eq!(index[&key(&a)].message_count, 3);
}

#[test]
fn a_corrupt_index_is_rebuilt_from_the_logs() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("sessions");
    let path = write_log(
        &root,
        "a/one.jsonl",
        &[header("aaaa1111", "/work/a"), user("first prompt", 1)],
    );
    load_session_index(&root);
    fs::write(root.join(".index.json"), "{ not json").unwrap();
    assert_eq!(load_session_index(&root)[&key(&path)].id, "aaaa1111");
    let file = read_index(&root);
    assert_eq!(file["version"], 1);
    assert_eq!(file["sessions"][key(&path)]["firstPrompt"], "first prompt");
}

#[test]
fn an_index_written_by_the_typescript_version_is_reused() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("sessions");
    let path = write_log(
        &root,
        "a/one.jsonl",
        &[header("aaaa1111", "/work/a"), user("first prompt", 1)],
    );
    // An mtime with a fraction that serde_json reads one ulp off without float_roundtrip (whole
    // 100 ns, so NTFS keeps it exactly too).
    let t = UNIX_EPOCH + Duration::new(1_793_929_471, 550_001_400);
    utimes(&path, t);
    let size = fs::metadata(&path).unwrap().len();
    // What the TS wrote for it (Node's mtimeMs, printed by JS as 1793929471550.0015), with a summary
    // the log does not say: served from the index.
    let summary = json!({ "id": "from-index", "cwd": "/work/a", "createdAt": "", "size": size,
        "modifiedAt": 1_793_929_471_550.001_5, "firstPrompt": "", "messageCount": 0, "usage": [] });
    let index = json!({ "version": 1, "sessions": { key(&path): summary } }).to_string();
    assert!(index.contains(r#""modifiedAt":1793929471550.0015,"#));
    fs::write(root.join(".index.json"), &index).unwrap();
    assert_eq!(load_session_index(&root)[&key(&path)].id, "from-index");
    // Nothing changed: the index is not rewritten.
    assert_eq!(fs::read_to_string(root.join(".index.json")).unwrap(), index);
}

/// Node's `statSync(path).mtime`: whole milliseconds.
fn mtime_ms(path: &Path) -> i64 {
    fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// The pre-index algorithm, kept as the reference for "list_sessions output unchanged".
fn old_list_sessions(dir: &Path, cwd: Option<&str>) -> Vec<SessionInfo> {
    let root = dir.parent().unwrap();
    let dirs = match cwd {
        Some(c) => vec![
            dir.to_path_buf(),
            root.join(legacy_name(&normalize_cwd(c))),
            root.join(legacy_name(c)),
        ],
        None => vec![dir.to_path_buf()],
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    for d in dirs {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        let mut names: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for f in names {
            let p = d.join(&f);
            if f.ends_with(".jsonl") && !paths.contains(&p) {
                paths.push(p);
            }
        }
    }
    let mut out = Vec::new();
    for path in paths {
        let Ok(entries) = read_entries(&path) else { continue };
        let Some(SessionEntry::Session {
            id,
            cwd: head_cwd,
            created_at,
            ..
        }) = entries.first()
        else {
            continue;
        };
        if cwd.is_some_and(|c| normalize_cwd(head_cwd) != normalize_cwd(c)) {
            continue;
        }
        let first_prompt = entries
            .iter()
            .find_map(|e| match e {
                SessionEntry::Message {
                    message: Message::User(u),
                } => Some(
                    u.content
                        .iter()
                        .map(|c| match c {
                            UserContent::Text(t) => t.text.clone(),
                            UserContent::Image(_) => "[image]".into(),
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
                _ => None,
            })
            .unwrap_or_default();
        out.push(SessionInfo {
            id: id.clone(),
            path: path.clone(),
            cwd: head_cwd.clone(),
            created_at: created_at.clone(),
            modified_at: mtime_ms(&path),
            first_prompt,
            message_count: restore(&entries).messages.len(),
        });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.modified_at));
    out
}

#[test]
fn list_sessions_output_is_unchanged_versus_the_pre_index_semantics() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("sessions");
    // Distinct mtimes throughout: the expected order must not depend on readdir order.
    let mine = write_log(
        &root,
        "work-x/mine.jsonl",
        &[
            header("cccc3333", "/work/x"),
            user("mine prompt", 1),
            assistant(2, "other"),
        ],
    );
    let theirs = write_log(
        &root,
        "work-x/theirs.jsonl",
        &[header("dddd3333", "/work/y"), user("theirs", 1)],
    );
    let headless = write_log(
        &root,
        "work-x/headless.jsonl",
        &[user("no header", 1), assistant(2, "m")],
    );
    let torn = write_log(
        &root,
        "work-x/torn.jsonl",
        &[
            header("eeee3333", "/work/x"),
            user("before the crash", 1),
            "{ torn".to_string(),
        ],
    );
    utimes(&mine, stamp(0));
    utimes(&headless, stamp(1));
    utimes(&theirs, stamp(2));
    utimes(&torn, stamp(3));
    let dir = root.join("work-x");
    for cwd in [None, Some("/work/x"), Some("/work/y")] {
        assert_eq!(list_sessions(&dir, cwd), old_list_sessions(&dir, cwd), "{cwd:?}");
    }
    // Newest first; a log without its session header is not a session (its usage still counts).
    let ids = |l: Vec<SessionInfo>| l.into_iter().map(|s| s.id).collect::<Vec<_>>();
    assert_eq!(ids(list_sessions(&dir, None)), ["eeee3333", "dddd3333", "cccc3333"]);
    assert_eq!(ids(list_sessions(&dir, Some("/work/x"))), ["eeee3333", "cccc3333"]);
    let info = list_sessions(&dir, Some("/work/x"))
        .into_iter()
        .find(|s| s.path == mine)
        .unwrap();
    assert_eq!(
        (info.cwd.as_str(), info.first_prompt.as_str(), info.message_count),
        ("/work/x", "mine prompt", 2)
    );
    let no_header = &load_session_index(&root)[&key(&headless)];
    assert_eq!(no_header.id, "");
    assert_eq!(no_header.usage.len(), 1);
}

#[test]
fn a_session_id_is_found_from_any_directory_naming_its_working_directory() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("sessions");
    write_log(
        &root,
        "work-x/headless.jsonl",
        &[user("no header", 1), assistant(2, "m")],
    );
    let path = write_log(
        &root,
        "elsewhere/s.jsonl",
        &[
            header("ffff4444", "/work/elsewhere"),
            user("other", 1),
            assistant(2, "m"),
        ],
    );
    utimes(&path, stamp(0));
    let found = find_session(&root, "ffff").unwrap(); // id prefix
    assert_eq!(
        (found.id.as_str(), &found.path, found.cwd.as_str()),
        ("ffff4444", &path, "/work/elsewhere")
    );
    assert_eq!(find_session(&root, "ffff4444").unwrap().path, path); // exact id
    assert_eq!(find_session(&root, &key(&path)).unwrap().id, "ffff4444"); // exact path
    assert_eq!(find_session(&root, "no-such-session"), None);
    assert_eq!(find_session(&root, "headless"), None); // a log without its header is not resumable
    // The newest wins when a prefix matches several.
    let newer = write_log(
        &root,
        "other/t.jsonl",
        &[header("ffff5555", "/work/other"), user("x", 1)],
    );
    utimes(&newer, stamp(5));
    assert_eq!(find_session(&root, "ffff").unwrap().id, "ffff5555");
}

#[test]
fn sessions_in_the_shared_pre_0_8_folder_are_told_apart_by_the_directory_they_record() {
    let home = tempfile::tempdir().unwrap();
    let home2 = home.path().join("home-dirs");
    let legacy = home2.join("sessions").join("work-a-b-c");
    let mine = SessionFile::create(&legacy, "/work/a-b/c", vec![]).unwrap();
    let theirs = SessionFile::create(&legacy, "/work/a/b-c", vec![]).unwrap();
    for f in [&mine, &theirs] {
        f.append(&SessionEntry::Model { model: "t/m".into() }).unwrap();
    }
    let ids: Vec<String> = list_sessions(&session_dir(&home2, "/work/a-b/c"), Some("/work/a-b/c"))
        .into_iter()
        .map(|x| x.id)
        .collect();
    assert_eq!(ids, [mine.id()]);
}
