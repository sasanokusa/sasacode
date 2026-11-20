use std::path::{Path, PathBuf};

use sasacode_protocol::git_folders::{Folders, add, file, is_known, key, load, projects, read};

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
    assert!(read(&home).unwrap().is_none());
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
    let mut theirs = read(&home).unwrap().unwrap();
    theirs.insert(key(&b), true);
    std::fs::write(file(&home), serde_json::to_string(&theirs).unwrap()).unwrap();
    let all = add(&home, &a, &[]).unwrap();
    assert!(all.contains_key(&key(&a)) && all.contains_key(&key(&b)));
}

#[test]
fn additions_from_several_threads_are_all_kept() {
    let d = Dir::new("threads");
    let home = d.0.join("home");
    let folders: Vec<PathBuf> = (0..12).map(|n| d.0.join(format!("f{n}"))).collect();
    for f in &folders {
        std::fs::create_dir_all(f).unwrap();
    }
    let threads: Vec<_> = folders
        .iter()
        .map(|f| {
            let (home, f) = (home.clone(), f.clone());
            std::thread::spawn(move || {
                add(&home, &f, &[]).unwrap();
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let kept = read(&home).unwrap().unwrap();
    for f in &folders {
        assert!(kept.contains_key(&key(f)), "{f:?} missing from {kept:?}");
    }
    assert!(!home.join(".git-folders.lock").exists());
}

#[test]
fn the_first_write_does_not_replace_a_list_another_process_wrote_meanwhile() {
    let d = Dir::new("meanwhile");
    let (home, used, theirs) = (d.0.join("home"), d.0.join("used"), d.0.join("theirs"));
    for f in [&used, &theirs] {
        std::fs::create_dir_all(f).unwrap();
    }
    conversation(&home, &used);
    // Another process wrote the list after this one saw none: what it wrote is read again.
    std::fs::write(
        file(&home),
        serde_json::to_string(&Folders::from([(key(&theirs), true)])).unwrap(),
    )
    .unwrap();
    let kept = load(&home, &[]);
    assert!(kept.contains_key(&key(&theirs)) && !kept.contains_key(&key(&used)));
    let all = add(&home, &used, &[]).unwrap();
    assert!(all.contains_key(&key(&theirs)) && all.contains_key(&key(&used)));
}

#[cfg(unix)]
#[test]
fn a_list_that_cannot_be_read_is_not_written_over() {
    use std::os::unix::fs::PermissionsExt;
    let d = Dir::new("unreadable");
    let (home, a, b) = (d.0.join("home"), d.0.join("a"), d.0.join("b"));
    for f in [&a, &b] {
        std::fs::create_dir_all(f).unwrap();
    }
    add(&home, &a, &[]).unwrap();
    let before = std::fs::read_to_string(file(&home)).unwrap();
    std::fs::set_permissions(file(&home), std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_to_string(file(&home)).is_ok() {
        return; // a user who can read anything (root): nothing to check
    }
    assert!(read(&home).is_err());
    assert!(add(&home, &b, &[]).is_err());
    assert!(load(&home, &[]).is_empty() && !is_known(&home, &a));
    std::fs::set_permissions(file(&home), std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(std::fs::read_to_string(file(&home)).unwrap(), before);
}
