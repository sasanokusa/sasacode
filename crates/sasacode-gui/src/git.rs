//! The folder's git repository as the inspector's git tab shows it: the branch, what is changed
//! and not committed, each file's diff, and what can be done about it there (stage, unstage,
//! discard, commit, push, open a pull request with `gh`). Each is a run of `git` or `gh` in the
//! folder, away from the window.

use std::path::Path;
use std::process::Command;

use crate::diff::{Diff, Sign};

/// A file bigger than this that git does not know yet is not read to be shown.
const UNTRACKED_MAX: u64 = 256 * 1024;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// The folder is in a git repository.
    pub repo: bool,
    /// The repository's top folder, which the files' paths are relative to.
    pub top: std::path::PathBuf,
    /// The branch checked out; none when HEAD is detached.
    pub branch: Option<String>,
    /// The branch has an upstream to push to.
    pub upstream: bool,
    /// Commits not pushed yet, when it has.
    pub ahead: usize,
    pub files: Vec<File>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct File {
    /// Relative to the repository's top.
    pub path: String,
    /// `git status --porcelain`'s two letters: the index's, and the working tree's.
    pub index: char,
    pub tree: char,
    pub diff: Diff,
}

impl File {
    pub fn untracked(&self) -> bool {
        self.index == '?'
    }

    /// Something of it is staged.
    pub fn staged(&self) -> bool {
        !matches!(self.index, ' ' | '?')
    }
}

/// What can be done from the git tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Stage(String),
    Unstage(String),
    /// Back to what is committed; a file git does not know is removed.
    Discard(String),
    /// What is staged, or everything when nothing is.
    Commit(String),
    Push,
    /// A pull request of the branch, its title and description from its commits.
    Pr,
}

fn run(cwd: &Path, program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_owned();
    if out.status.success() {
        Ok(text(&out.stdout))
    } else {
        let err = text(&out.stderr);
        Err(if err.is_empty() { text(&out.stdout) } else { err })
    }
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    run(cwd, "git", args)
}

/// The repository's top folder, where the paths git gives are relative to.
fn top(cwd: &Path) -> Option<std::path::PathBuf> {
    git(cwd, &["rev-parse", "--show-toplevel"]).ok().map(Into::into)
}

/// The repository as it is now.
pub fn status(cwd: &Path) -> Status {
    let Some(top) = top(cwd) else {
        return Status::default();
    };
    let mut status = Status {
        repo: true,
        top: top.clone(),
        ..Status::default()
    };
    let Ok(out) = git(
        &top,
        &["status", "--porcelain=v1", "-z", "--branch", "--untracked-files=all"],
    ) else {
        return status;
    };
    let mut entries = out.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        if let Some(head) = entry.strip_prefix("## ") {
            let (branch, rest) = head.split_once("...").unwrap_or((head, ""));
            status.branch = (!branch.starts_with("HEAD") && !branch.starts_with("No commits"))
                .then(|| branch.trim().to_owned())
                .or_else(|| branch.strip_prefix("No commits yet on ").map(str::to_owned));
            status.upstream = !rest.is_empty();
            status.ahead = rest
                .split_once("ahead ")
                .and_then(|(_, n)| n.trim_end_matches(']').split(',').next()?.trim().parse().ok())
                .unwrap_or(0);
            continue;
        }
        let mut letters = entry.chars();
        let (Some(index), Some(tree)) = (letters.next(), letters.next()) else {
            continue;
        };
        let path = entry.get(3..).unwrap_or_default().to_owned();
        // A rename names where it came from next; the diff is of where it is.
        if index == 'R' || index == 'C' {
            entries.next();
        }
        let diff = file_diff(&top, &path, index == '?');
        status.files.push(File {
            path,
            index,
            tree,
            diff,
        });
    }
    status
}

/// What changed in `path` since the last commit, staged or not; all of it for a file git does not
/// know yet.
fn file_diff(top: &Path, path: &str, untracked: bool) -> Diff {
    if untracked {
        let file = top.join(path);
        let small = std::fs::metadata(&file).is_ok_and(|m| m.is_file() && m.len() <= UNTRACKED_MAX);
        return match small.then(|| std::fs::read_to_string(&file).ok()).flatten() {
            Some(text) => Diff::create(&text),
            None => Diff::default(),
        };
    }
    // Against HEAD, or, before the first commit, against nothing.
    let base = if git(top, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok() {
        "HEAD"
    } else {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    };
    let out = git(top, &["diff", "--no-color", "--no-ext-diff", base, "--", path]).unwrap_or_default();
    // The hunks only: the lines before the first say which files, as `---` and `+++`.
    let hunks = out.find("\n@@").map_or("", |at| &out[at + 1..]);
    Diff::parse(hunks)
}

/// The line number in the new file of each line of `diff`, where it has one.
pub fn numbers(diff: &Diff) -> Vec<Option<usize>> {
    let mut next = 0usize;
    diff.lines
        .iter()
        .map(|line| match line.sign {
            Sign::Hunk => {
                next = line
                    .text
                    .split_once('+')
                    .and_then(|(_, rest)| rest.split([',', ' ']).next()?.parse().ok())
                    .unwrap_or(0);
                None
            }
            Sign::Del => None,
            Sign::Add | Sign::Context => {
                let n = next;
                next += 1;
                Some(n)
            }
        })
        .collect()
}

/// Does `op`, and says what came of it.
pub fn apply(cwd: &Path, op: &Op, branch: Option<&str>) -> Result<String, String> {
    let top = top(cwd).ok_or_else(|| "not a git repository".to_owned())?;
    match op {
        Op::Stage(path) => git(&top, &["add", "--", path]),
        Op::Unstage(path) => git(&top, &["restore", "--staged", "--", path]),
        Op::Discard(path) => {
            let tracked = git(&top, &["ls-files", "--error-unmatch", "--", path]).is_ok();
            if tracked {
                git(
                    &top,
                    &["restore", "--staged", "--worktree", "--source=HEAD", "--", path],
                )
            } else {
                git(&top, &["clean", "-f", "-q", "--", path])
            }
        }
        Op::Commit(message) => {
            let staged = git(&top, &["diff", "--cached", "--quiet"]).is_err();
            if !staged {
                git(&top, &["add", "-A"])?;
            }
            git(&top, &["commit", "-q", "-m", message])
                .map(|_| git(&top, &["log", "-1", "--format=%h %s"]).unwrap_or_default())
        }
        Op::Push => {
            let upstream = git(&top, &["rev-parse", "--abbrev-ref", "@{upstream}"]).is_ok();
            match (upstream, branch) {
                (true, _) => git(&top, &["push", "-q"]),
                (false, Some(branch)) => git(&top, &["push", "-q", "-u", "origin", branch]),
                (false, None) => Err("no branch to push".into()),
            }
            .map(|out| if out.is_empty() { "pushed".to_owned() } else { out })
        }
        Op::Pr => run(&top, "gh", &["pr", "create", "--fill"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-git-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "test@example.invalid"],
            &["config", "user.name", "test"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git(&dir, args).unwrap();
        }
        dir
    }

    #[test]
    fn what_is_not_committed_is_listed_with_its_diff_and_can_be_committed() {
        let dir = repo("status");
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        apply(&dir, &Op::Commit("first".into()), None).unwrap();
        assert!(status(&dir).files.is_empty());

        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(dir.join("new file.txt"), "hello\n").unwrap();
        let s = status(&dir);
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert!(s.repo && !s.upstream);
        let paths: Vec<&str> = s.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "new file.txt"]);
        let a = &s.files[0];
        assert_eq!((a.diff.added, a.diff.removed), (2, 1));
        assert_eq!(numbers(&a.diff), vec![None, Some(1), None, Some(2), Some(3)]);
        assert!(s.files[1].untracked() && s.files[1].diff.added == 1);

        // Staged, then unstaged; discarded, the new file is gone.
        apply(&dir, &Op::Stage("a.txt".into()), None).unwrap();
        assert!(status(&dir).files[0].staged());
        apply(&dir, &Op::Unstage("a.txt".into()), None).unwrap();
        assert!(!status(&dir).files[0].staged());
        apply(&dir, &Op::Discard("new file.txt".into()), None).unwrap();
        assert!(!dir.join("new file.txt").exists());

        // With nothing staged, a commit takes everything.
        let said = apply(&dir, &Op::Commit("second".into()), None).unwrap();
        assert!(said.ends_with("second"));
        assert!(status(&dir).files.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_folder_outside_a_repository_has_none() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-nogit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The temp folder may itself be inside a repository on some machines; only say what is
        // certain.
        if top(&dir).is_none() {
            assert_eq!(status(&dir), Status::default());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
