//! The git worktrees the window makes for a conversation of its own: from the branch checked out
//! or one named (`/worktree main`), readied by the folder's own command (`npm install`) before
//! their window opens, and listed to clean up. Only those it made, under sasacode's home, are
//! listed, and one is removed as git removes it: never with what was changed in it and not
//! committed, and its branch kept.

use std::path::{Path, PathBuf};
use std::process::Command;

use iced::Task;

use crate::app::{App, Msg};
use crate::overlay::Entry;

/// A worktree made here: its folder and the branch checked out in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    pub path: PathBuf,
    pub branch: Option<String>,
}

fn git(cwd: &Path, args: &[&std::ffi::OsStr]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Where the window keeps the worktrees it makes.
pub fn root(home: &Path) -> PathBuf {
    home.join("worktrees")
}

/// The worktrees of `cwd`'s repository that were made here, as `git worktree list` says.
pub fn list(cwd: &Path, home: &Path) -> Vec<Tree> {
    let Ok(out) = git(cwd, &["worktree".as_ref(), "list".as_ref(), "--porcelain".as_ref()]) else {
        return Vec::new();
    };
    let root = root(home);
    let root = root.canonicalize().unwrap_or(root);
    let mut trees = Vec::new();
    for block in out.split("\n\n") {
        let mut tree = Tree {
            path: PathBuf::new(),
            branch: None,
        };
        for line in block.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                tree.path = PathBuf::from(path);
            } else if let Some(branch) = line.strip_prefix("branch ") {
                tree.branch = Some(branch.trim_start_matches("refs/heads/").to_owned());
            }
        }
        let at = tree.path.canonicalize().unwrap_or_else(|_| tree.path.clone());
        if !tree.path.as_os_str().is_empty() && at.starts_with(&root) {
            trees.push(tree);
        }
    }
    trees
}

/// Removes the worktree at `path` as git does: refused while it holds what is not committed.
pub fn remove(cwd: &Path, path: &Path) -> Result<(), String> {
    git(cwd, &["worktree".as_ref(), "remove".as_ref(), path.as_os_str()]).map(|_| ())
}

/// Runs the folder's command to ready a new worktree in it; what it wrote last, if it failed.
async fn ready(dir: &Path, command: &str) -> Result<(), String> {
    let out = crate::scripts::shell(command, dir)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    let said = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let tail: Vec<&str> = said.lines().filter(|l| !l.trim().is_empty()).collect();
    Err(tail[tail.len().saturating_sub(4)..].join("\n"))
}

impl App {
    /// The command that readies this folder's new worktrees, if it was given one.
    pub(crate) fn worktree_setup(&self) -> Option<String> {
        self.prefs
            .more
            .folders
            .get(&self.folder_key())
            .map(|f| f.setup.trim().to_owned())
            .filter(|c| !c.is_empty())
    }

    /// Makes a worktree from the branch checked out, or from `base`, readies it, and opens it
    /// in a window of its own.
    pub(crate) fn worktree(&mut self, base: Option<String>) -> Task<Msg> {
        let Some(home) = self.home.clone() else {
            return Task::none();
        };
        let (cwd, setup) = (self.cwd.clone(), self.worktree_setup());
        self.toast = Some(self.s.w().worktree_making.to_owned());
        Task::perform(
            async move {
                let made =
                    tokio::task::spawn_blocking(move || crate::system::make_worktree(&cwd, &home, base.as_deref()))
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|r| r)?;
                let note = match setup {
                    Some(command) => ready(&made, &command).await.err(),
                    None => None,
                };
                Ok((made, note))
            },
            Msg::WorktreeMade,
        )
    }

    /// The worktrees made here, read again for the list.
    pub(crate) fn list_worktrees(&mut self) -> Task<Msg> {
        self.worktree_list = None;
        let Some(home) = self.home.clone() else {
            self.worktree_list = Some(Vec::new());
            return Task::none();
        };
        let cwd = self.cwd.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || list(&cwd, &home))
                    .await
                    .unwrap_or_default()
            },
            Msg::Worktrees,
        )
    }

    /// The list of worktrees: a new one first, then each made here, to remove. The one this
    /// window is in is not offered.
    pub(crate) fn worktree_entries(&self, query: &str) -> Vec<Entry> {
        let (s, w) = (self.s, self.s.w());
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut all = vec![Entry {
            label: s.c_worktree.to_owned(),
            keys: Some("/worktree".into()),
            msg: Msg::Command(crate::input::Command::Worktree),
        }];
        let here = self.cwd.canonicalize().unwrap_or_else(|_| self.cwd.clone());
        for tree in self.worktree_list.iter().flatten() {
            let at = tree.path.canonicalize().unwrap_or_else(|_| tree.path.clone());
            if here.starts_with(&at) {
                continue;
            }
            let name = tree
                .path
                .file_name()
                .map_or_else(|| tree.path.display().to_string(), |n| n.to_string_lossy().into_owned());
            let label = crate::i18n::fill(
                &crate::i18n::fill(w.worktree_remove, "name", &name),
                "branch",
                tree.branch.as_deref().unwrap_or("-"),
            );
            all.push(Entry {
                label,
                keys: None,
                msg: Msg::WorktreeRemove(tree.path.clone()),
            });
        }
        all.retain(|e| {
            let label = e.label.to_lowercase();
            words.iter().all(|w| label.contains(w))
        });
        all
    }

    /// The worktree messages; any other comes back.
    pub(crate) fn update_worktrees(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let (s, w) = (self.s, self.s.w());
        let task = match msg {
            Msg::WorktreeMade(Ok((path, note))) => {
                let name = path
                    .file_name()
                    .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                let made = crate::i18n::fill(s.worktree_made, "path", name);
                // Readied or not, it opens: what failed is said, to be done by hand there.
                self.toast = Some(match note {
                    Some(why) => format!("{made}\n{}", crate::i18n::fill(w.worktree_setup_failed, "why", why)),
                    None => made,
                });
                self.new_window(path, None)
            }
            Msg::WorktreeMade(Err(error)) => {
                self.toast = Some(crate::i18n::fill(s.worktree_failed, "error", error));
                Task::none()
            }
            Msg::Worktrees(list) => {
                self.worktree_list = Some(list);
                Task::none()
            }
            Msg::WorktreeRemove(path) => {
                // Only one made here, and never the one this window is in.
                let ours = self.worktree_list.iter().flatten().any(|t| t.path == path);
                let here = self.cwd.canonicalize().unwrap_or_else(|_| self.cwd.clone());
                if !ours || here.starts_with(path.canonicalize().unwrap_or_else(|_| path.clone())) {
                    return Ok(Task::none());
                }
                let cwd = self.cwd.clone();
                Task::perform(
                    async move {
                        let at = path.clone();
                        let done = tokio::task::spawn_blocking(move || remove(&cwd, &at))
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|r| r);
                        (path, done)
                    },
                    |(path, done)| Msg::WorktreeRemoved(path, done),
                )
            }
            Msg::WorktreeRemoved(path, done) => {
                let name = path
                    .file_name()
                    .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                self.toast = Some(match done {
                    Ok(()) => crate::i18n::fill(w.worktree_removed, "name", name),
                    Err(why) => crate::i18n::fill(w.worktree_keep, "why", why),
                });
                if let Some(list) = &mut self.worktree_list {
                    list.retain(|t| t.path.exists());
                }
                Task::none()
            }
            Msg::SetWorktreeSetup(command) => {
                let key = self.folder_key();
                self.set(|p| p.more.folders.entry(key).or_default().setup = command)
            }
            other => return self.update_jobs(other),
        };
        Ok(task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cwd: &Path, args: &[&str]) {
        let out = Command::new("git").args(args).current_dir(cwd).output().unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn a_worktree_made_here_is_listed_made_from_the_base_named_and_removed_only_when_clean() {
        let base = std::env::temp_dir().join(format!("sasacode-gui-trees-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (repo, home) = (base.join("repo"), base.join("home"));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run(&repo, &["init", "-q", "-b", "main"]);
        run(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@e",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "one",
            ],
        );
        run(&repo, &["branch", "older"]);
        run(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@e",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "two",
            ],
        );
        // One the user made elsewhere is not the window's to list.
        run(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "theirs",
                base.join("theirs").to_str().unwrap(),
            ],
        );
        assert!(list(&repo, &home).is_empty());

        let made = crate::system::make_worktree(&repo, &home, Some("older")).unwrap();
        let trees = list(&repo, &home);
        assert_eq!(trees.len(), 1);
        assert!(trees[0].branch.as_deref().is_some_and(|b| b.starts_with("sasacode/")));
        let head = |dir: &Path| git(dir, &["log".as_ref(), "-1".as_ref(), "--format=%s".as_ref()]).unwrap();
        assert_eq!(head(&made), "one", "made from the base named");
        // What was changed in it and not committed keeps it.
        std::fs::write(made.join("draft.txt"), "まだ").unwrap();
        assert!(remove(&repo, &trees[0].path).is_err());
        assert!(made.exists());
        std::fs::remove_file(made.join("draft.txt")).unwrap();
        remove(&repo, &trees[0].path).unwrap();
        assert!(!made.exists() && list(&repo, &home).is_empty());
        // Its branch is kept.
        assert!(
            git(&repo, &["branch".as_ref(), "--list".as_ref(), "sasacode/*".as_ref()])
                .unwrap()
                .contains("sasacode/")
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_new_worktree_is_readied_by_the_folders_command_and_a_failure_says_its_last_lines() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-ready-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(ready(&dir, "echo ok > readied.txt")).unwrap();
        assert!(dir.join("readied.txt").exists());
        let why = rt.block_on(ready(&dir, "echo one; echo two >&2; exit 3")).unwrap_err();
        assert!(why.contains("one") && why.contains("two"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
