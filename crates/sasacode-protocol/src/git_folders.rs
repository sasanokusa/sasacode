//! The folders git may be run in without asking, shared by the CLI and the desktop window:
//! `$SASACODE_HOME/git-folders.json`, a map of real paths. A repository's config can name programs
//! that git starts, so a folder is added only once the user allows it, trusts its project, or a
//! window made it (a worktree). The first time the file is needed, the folders used before are
//! written into it, so they are not asked about.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub type Folders = BTreeMap<String, bool>;

/// Where the folders are kept.
pub fn file(home: &Path) -> PathBuf {
    home.join("git-folders.json")
}

/// How a folder is kept: its real path, else as it is spelled.
pub fn key(folder: &Path) -> String {
    std::fs::canonicalize(folder)
        .unwrap_or_else(|_| folder.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// The folders kept, or none when there is no file yet. A file that is not a map counts as an
/// empty one; a file that cannot be read at all (anything but "not there") is an error, so what
/// is in it is not written over.
pub fn read(home: &Path) -> std::io::Result<Option<Folders>> {
    match std::fs::read_to_string(file(home)) {
        Ok(text) => Ok(Some(serde_json::from_str(&text).unwrap_or_default())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The folders kept, written first from those used before (the projects sasacode keeps
/// conversations for, and `more`) when there is no file yet. A file that cannot be read, or the
/// lock not got, is no folder at all: nothing is written and nothing runs.
pub fn load(home: &Path, more: &[PathBuf]) -> Folders {
    match read(home) {
        Ok(Some(kept)) => return kept,
        Ok(None) => {}
        Err(_) => return Folders::new(),
    }
    let Ok(_lock) = Lock::take(home) else {
        return Folders::new();
    };
    // Another process may have written it while this one waited.
    match read(home) {
        Ok(Some(kept)) => kept,
        Ok(None) => {
            let seen = used_before(home, more);
            let _ = write(home, &seen);
            seen
        }
        Err(_) => Folders::new(),
    }
}

/// Whether `folder` is kept, the file first written as [`load`] does.
pub fn is_known(home: &Path, folder: &Path) -> bool {
    load(home, &[]).contains_key(&key(folder))
}

/// `folder` kept from now on: read from disk under a lock (written first as [`load`] does),
/// added, written whole to a new file that then takes the old one's name. What is kept afterwards
/// is returned. A file that cannot be read is an error, and is left as it is.
pub fn add(home: &Path, folder: &Path, more: &[PathBuf]) -> std::io::Result<Folders> {
    let _lock = Lock::take(home)?;
    let (mut all, first) = match read(home)? {
        Some(kept) => (kept, false),
        None => (used_before(home, more), true),
    };
    if all.insert(key(folder), true).is_none() || first {
        write(home, &all)?;
    }
    Ok(all)
}

fn used_before(home: &Path, more: &[PathBuf]) -> Folders {
    projects(home).iter().chain(more).map(|p| (key(p), true)).collect()
}

/// The file that says one process is reading and then writing the list. A process that died
/// holding it leaves it behind: one this old is taken over.
struct Lock(PathBuf);

impl Lock {
    const STALE: std::time::Duration = std::time::Duration::from_secs(15);
    const GIVE_UP: std::time::Duration = std::time::Duration::from_secs(20);

    fn take(home: &Path) -> std::io::Result<Lock> {
        std::fs::create_dir_all(home)?;
        let path = home.join(".git-folders.lock");
        let started = std::time::Instant::now();
        loop {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(Lock(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let age = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok());
                    if age.is_some_and(|a| a > Self::STALE) {
                        let _ = std::fs::remove_file(&path);
                    } else if started.elapsed() > Self::GIVE_UP {
                        return Err(std::io::ErrorKind::TimedOut.into());
                    } else {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn write(home: &Path, all: &Folders) -> std::io::Result<()> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(home)?;
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(all).unwrap_or_else(|_| "{}".into())
    );
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = WRITES.fetch_add(1, Ordering::Relaxed);
    let temp = home.join(format!(".git-folders.{}.{nanos}.{n}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options
        .open(&temp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&temp, file(home)));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// The folders sasacode has kept conversations for, the most recently used first: the first
/// entry of the newest log in each of the home's session folders says where it ran.
pub fn projects(home: &Path) -> Vec<PathBuf> {
    let Ok(dirs) = std::fs::read_dir(home.join("sessions")) else {
        return Vec::new();
    };
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = dirs
        .filter_map(Result::ok)
        .filter_map(|dir| {
            let newest = std::fs::read_dir(dir.path())
                .ok()?
                .filter_map(Result::ok)
                .filter(|f| f.path().extension().is_some_and(|x| x == "jsonl"))
                .filter_map(|f| Some((f.metadata().ok()?.modified().ok()?, f.path())))
                .max_by_key(|(when, _)| *when)?;
            let first = std::fs::read_to_string(&newest.1).ok()?;
            let head: serde_json::Value = serde_json::from_str(first.lines().next()?).ok()?;
            let cwd = PathBuf::from(head["cwd"].as_str()?);
            cwd.is_dir().then_some((newest.0, cwd))
        })
        .collect();
    found.sort_by_key(|a| std::cmp::Reverse(a.0));
    // One folder reached through a link (`/var` on macOS is `/private/var`) is listed once. The
    // real path only tells them apart: it is not shown or opened, as on Windows it is `\\?\C:\…`.
    let mut seen = std::collections::HashSet::new();
    found
        .into_iter()
        .map(|(_, cwd)| cwd)
        .filter(|cwd| seen.insert(std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.clone())))
        .collect()
}
