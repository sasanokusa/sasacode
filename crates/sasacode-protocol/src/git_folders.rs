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

/// The file that says one process is reading and then writing the list. It holds the mark of the
/// process that took it. A process that died holding it leaves it behind: one this old is taken
/// over, by moving it to a name of its own first, so only one process takes it over.
struct Lock {
    path: PathBuf,
    mark: String,
}

impl Lock {
    const STALE: std::time::Duration = std::time::Duration::from_secs(15);
    const GIVE_UP: std::time::Duration = std::time::Duration::from_secs(20);
    const NAME: &'static str = ".git-folders.lock";

    fn take(home: &Path) -> std::io::Result<Lock> {
        Self::take_within(home, Self::STALE, Self::GIVE_UP)
    }

    fn take_within(home: &Path, stale: std::time::Duration, give_up: std::time::Duration) -> std::io::Result<Lock> {
        static TAKES: AtomicU64 = AtomicU64::new(0);
        std::fs::create_dir_all(home)?;
        let path = home.join(Self::NAME);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let n = TAKES.fetch_add(1, Ordering::Relaxed);
        let mark = format!("{}.{nanos}.{n}", std::process::id());
        let started = std::time::Instant::now();
        loop {
            if started.elapsed() > give_up {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            match options.open(&path) {
                Ok(mut file) => {
                    if let Err(e) = file.write_all(mark.as_bytes()) {
                        drop(file);
                        let _ = std::fs::remove_file(&path);
                        return Err(e);
                    }
                    return Ok(Lock { path, mark });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if is_stale(&path, stale) {
                        take_over(&path, &mark, stale);
                    }
                }
                Err(e) => return Err(e),
            }
            // A lock that is held, or cannot be moved, is waited for, never spun on.
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

/// Whether `path` is a lock file left for longer than `stale`.
fn is_stale(path: &Path, stale: std::time::Duration) -> bool {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > stale)
}

/// Clears an old lock: it is moved to a name nobody else uses, which only one process wins, and
/// then deleted. If what was moved is not old (another process took the lock over first and
/// made a new one), it is put back.
fn take_over(path: &Path, mark: &str, stale: std::time::Duration) {
    let Some(dir) = path.parent() else { return };
    let moved = dir.join(format!("{}.{mark}.old", Lock::NAME));
    if std::fs::rename(path, &moved).is_err() {
        return;
    }
    // Not old after all: put back, unless another process has made a lock there meanwhile.
    if !is_stale(&moved, stale) {
        let _ = std::fs::hard_link(&moved, path);
    }
    let _ = std::fs::remove_file(&moved);
}

impl Drop for Lock {
    /// Only a lock that still carries this process's mark is deleted: one that was taken over
    /// after this process stalled belongs to someone else.
    fn drop(&mut self) {
        if std::fs::read_to_string(&self.path).is_ok_and(|held| held == self.mark) {
            let _ = std::fs::remove_file(&self.path);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sasacode-lock-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn age(path: &Path, by: Duration) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(std::time::SystemTime::now() - by).unwrap();
    }

    #[test]
    fn a_lock_that_cannot_be_removed_is_given_up_on_without_spinning() {
        let home = home("unremovable");
        // A directory under the lock's name can be neither deleted as a file nor taken over.
        std::fs::create_dir(home.join(Lock::NAME)).unwrap();
        let started = std::time::Instant::now();
        let got = Lock::take_within(&home, Duration::ZERO, Duration::from_millis(200));
        assert_eq!(got.err().map(|e| e.kind()), Some(std::io::ErrorKind::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(home.join(Lock::NAME).is_dir());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn an_old_lock_is_taken_over_and_a_fresh_one_waited_for() {
        let home = home("stale");
        let first = Lock::take(&home).unwrap();
        let fresh = Lock::take_within(&home, Duration::from_secs(60), Duration::from_millis(100));
        assert!(fresh.is_err());
        age(&home.join(Lock::NAME), Duration::from_secs(30));
        let second = Lock::take_within(&home, Duration::from_secs(15), Duration::from_secs(5)).unwrap();
        assert_ne!(first.mark, second.mark);
        assert_eq!(std::fs::read_to_string(&second.path).unwrap(), second.mark);
        // Nothing is left beside the lock.
        let extra = std::fs::read_dir(&home).unwrap().count();
        drop(first);
        assert_eq!(std::fs::read_to_string(home.join(Lock::NAME)).unwrap(), second.mark);
        drop(second);
        assert!(!home.join(Lock::NAME).exists());
        assert_eq!(extra, 1);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn dropping_a_lock_leaves_another_process_lock_alone() {
        let home = home("foreign");
        let mine = Lock::take(&home).unwrap();
        std::fs::write(&mine.path, "someone-else").unwrap();
        drop(mine);
        assert_eq!(std::fs::read_to_string(home.join(Lock::NAME)).unwrap(), "someone-else");
        std::fs::remove_dir_all(&home).unwrap();
    }
}
