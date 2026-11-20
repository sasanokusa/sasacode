use std::path::{Path, PathBuf};

use sasacode_protocol::git_folders::{add, file, is_known, key, load, projects, read};

/// A folder of its own under the system's temporary folder, removed when dropped.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("sasacode-git-folders-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Dir(std::fs::canonicalize(&p).unwrap())
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A conversation kept for `cwd`, as the CLI keeps one.
fn conversation(home: &Path, cwd: &Path) {
    let dir = sasacode_protocol::session_dir(home, cwd);
    std::fs::create_dir_all(&dir).unwrap();
    let head = serde_json::json!({ "type": "session", "cwd": cwd.to_string_lossy() });
    std::fs::write(dir.join("a.jsonl"), format!("{head}\n")).unwrap();
}

#[test]
fn the_first_time_the_folders_used_before_are_known_and_then_only_those_added() {
    let d = Dir::new("seed");
    let (home, used, new, more) = (d.0.join("home"), d.0.join("used"), d.0.join("new"), d.0.join("more"));
    for f in [&used, &new, &more] {
        std::fs::create_dir_all(f).unwrap();
    }
    conversation(&home, &used);
    assert_eq!(projects(&home), std::slice::from_ref(&used));
    assert!(read(&home).is_none());
    let kept = load(&home, std::slice::from_ref(&more));
    assert!(kept.contains_key(&key(&used)) && kept.contains_key(&key(&more)) && !kept.contains_key(&key(&new)));
    assert!(file(&home).is_file());
    // Once only: a conversation kept afterwards does not make its folder known.
    conversation(&home, &new);
    assert!(is_known(&home, &used) && !is_known(&home, &new));
    add(&home, &new, &[]).unwrap();
    assert!(is_known(&home, &new));
    // Kept by its real path, however it is spelled.
    assert!(is_known(&home, &new.join("..").join("new")));
    // No file of a write left behind.
    let names: Vec<_> = std::fs::read_dir(&home)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    assert!(
        !names.iter().any(|n| n.to_string_lossy().ends_with(".tmp")),
        "{names:?}"
    );
}

#[test]
fn a_file_that_cannot_be_read_is_an_empty_list_not_a_first_time() {
    let d = Dir::new("bad");
    let (home, used) = (d.0.join("home"), d.0.join("used"));
    std::fs::create_dir_all(&used).unwrap();
    conversation(&home, &used);
    std::fs::write(file(&home), "not json").unwrap();
    assert!(!is_known(&home, &used));
    add(&home, &used, &[]).unwrap();
    assert!(is_known(&home, &used));
}

#[test]
fn an_addition_keeps_what_another_process_added() {
    let d = Dir::new("both");
    let (home, a, b) = (d.0.join("home"), d.0.join("a"), d.0.join("b"));
    for f in [&a, &b] {
        std::fs::create_dir_all(f).unwrap();
    }
    add(&home, &a, &[]).unwrap();
    // Another process's addition, then this one's: both kept.
    let mut theirs = read(&home).unwrap();
    theirs.insert(key(&b), true);
    std::fs::write(file(&home), serde_json::to_string(&theirs).unwrap()).unwrap();
    let all = add(&home, &a, &[]).unwrap();
    assert!(all.contains_key(&key(&a)) && all.contains_key(&key(&b)));
}
