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

/// The branches a worktree can be made from, the latest committed to first: this repository's
/// and its remotes'.
pub fn bases(cwd: &Path) -> Vec<String> {
    let Ok(out) = git(
        cwd,
        &[
            "for-each-ref".as_ref(),
            "--sort=-committerdate".as_ref(),
            "--count=40".as_ref(),
            "--format=%(refname)".as_ref(),
            "refs/heads".as_ref(),
            "refs/remotes".as_ref(),
        ],
    ) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|r| {
            let r = r.trim();
            r.strip_prefix("refs/heads/")
                // A remote's HEAD names another of its branches; it is not one.
                .or_else(|| r.strip_prefix("refs/remotes/").filter(|b| !b.ends_with("/HEAD")))
        })
        .map(str::to_owned)
        .collect()
}

/// What the remotes have now, fetched before a worktree is made from it. Nothing is asked for: a
/// remote that wants a password or a passphrase fails instead. None to fetch from is no error.
pub fn fetch(cwd: &Path) -> Result<(), String> {
    if git(cwd, &["remote".as_ref()]).is_ok_and(|r| r.trim().is_empty()) {
        return Ok(());
    }
    let mut command = Command::new("git");
    command
        .args(["fetch", "--quiet"])
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null());
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    let out = command.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// When the worktree at `path` was last used: its last commit, or what was last staged in it.
fn last_used(path: &Path) -> Option<std::time::SystemTime> {
    let committed = git(path, &["log".as_ref(), "-1".as_ref(), "--format=%ct".as_ref()])
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok())
        .map(|s| std::time::UNIX_EPOCH + std::time::Duration::from_secs(s));
    let staged = git(path, &["rev-parse".as_ref(), "--git-path".as_ref(), "index".as_ref()])
        .ok()
        .map(|index| path.join(index))
        .and_then(|index| std::fs::metadata(index).ok()?.modified().ok());
    committed.max(staged)
}

/// How long a worktree goes unused before the list offers to clean it up.
pub const OLD_DAYS: u64 = 14;

/// Of `trees`, those last used before `before`, but never the one at `here`.
pub fn old(trees: &[Tree], before: std::time::SystemTime, here: &Path) -> Vec<PathBuf> {
    let here = here.canonicalize().unwrap_or_else(|_| here.to_path_buf());
    trees
        .iter()
        .filter(|t| !here.starts_with(t.path.canonicalize().unwrap_or_else(|_| t.path.clone())))
        .filter(|t| last_used(&t.path).is_some_and(|used| used < before))
        .map(|t| t.path.clone())
        .collect()
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
        let prefix = self.prefs.more.worktree_prefix.clone();
        let w = self.s.w();
        self.toast = Some(w.worktree_making.to_owned());
        Task::perform(
            async move {
                // What the remotes have now first; made from what is here when that fails.
                let (fetched, made) = tokio::task::spawn_blocking(move || {
                    let fetched = fetch(&cwd);
                    (
                        fetched,
                        crate::system::make_worktree(&cwd, &home, base.as_deref(), &prefix),
                    )
                })
                .await
                .map_err(|e| e.to_string())?;
                let made = made?;
                let mut notes: Vec<String> = Vec::new();
                if let Err(why) = fetched {
                    notes.push(crate::i18n::fill(w.worktree_fetch_failed, "why", why));
                }
                if let Some(command) = setup
                    && let Err(why) = ready(&made, &command).await
                {
                    notes.push(crate::i18n::fill(w.worktree_setup_failed, "why", why));
                }
                Ok((made, (!notes.is_empty()).then(|| notes.join("\n"))))
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
                tokio::task::spawn_blocking(move || (list(&cwd, &home), bases(&cwd)))
                    .await
                    .unwrap_or_default()
            },
            |(trees, bases)| Msg::Worktrees(trees, bases),
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
        // From a branch chosen, the latest committed to first.
        for base in &self.worktree_bases {
            all.push(Entry {
                label: crate::i18n::fill(w.worktree_from, "branch", base),
                keys: None,
                msg: Msg::WorktreeFrom(base.clone()),
            });
        }
        if self.worktree_list.as_ref().is_some_and(|l| !l.is_empty()) {
            all.push(Entry {
                label: crate::i18n::fill(w.worktree_sweep, "days", OLD_DAYS),
                keys: None,
                msg: Msg::WorktreeSweep,
            });
        }
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

    /// Whether a new conversation's message can be started in a new worktree from here.
    pub(crate) fn offers_worktree(&self) -> bool {
        self.git.repo
            && self.home.is_some()
            && !self.busy()
            && self.chat.session.is_none()
            && self.chat.transcript.exchanges.is_empty()
            && !self.chat.scratch
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
                // The message it was made for, left for its window to take.
                if let Some(text) = self.handoff.take() {
                    let left = self.home.as_deref().map(|home| leave_handoff(home, &path, &text));
                    if left.is_none_or(|left| left.is_err()) {
                        let _ = self.put_in_input(&text);
                    }
                }
                // Readied or not, it opens: what failed is said, to be done by hand there.
                self.toast = Some(match note {
                    Some(why) => format!("{made}\n{}", crate::i18n::fill(w.worktree_setup_failed, "why", why)),
                    None => made,
                });
                self.new_window(path, None)
            }
            Msg::WorktreeMade(Err(error)) => {
                self.toast = Some(crate::i18n::fill(s.worktree_failed, "error", error));
                // Not made: the message it was for is back in the input.
                match self.handoff.take() {
                    Some(text) => self.put_in_input(&text),
                    None => Task::none(),
                }
            }
            Msg::ToWorktree(on) => {
                self.to_worktree = on;
                Task::none()
            }
            Msg::Worktrees(list, bases) => {
                self.worktree_list = Some(list);
                self.worktree_bases = bases;
                Task::none()
            }
            Msg::WorktreeFrom(base) => {
                self.overlay = None;
                self.worktree(Some(base))
            }
            // Those unused for long, removed as one is: those holding changes are kept, and none
            // other than the window made.
            Msg::WorktreeSweep => {
                let trees = self.worktree_list.clone().unwrap_or_default();
                let (cwd, here) = (self.cwd.clone(), self.cwd.clone());
                let before = std::time::SystemTime::now() - std::time::Duration::from_secs(OLD_DAYS * 86_400);
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let (mut gone, mut kept) = (0, Vec::new());
                            for path in old(&trees, before, &here) {
                                match remove(&cwd, &path) {
                                    Ok(()) => gone += 1,
                                    Err(_) => kept.push(path.file_name().map_or_else(
                                        || path.display().to_string(),
                                        |n| n.to_string_lossy().into_owned(),
                                    )),
                                }
                            }
                            (gone, kept)
                        })
                        .await
                        .unwrap_or_default()
                    },
                    |(gone, kept)| Msg::WorktreesSwept(gone, kept),
                )
            }
            Msg::WorktreesSwept(gone, kept) => {
                let mut said = if gone == 0 && kept.is_empty() {
                    w.worktree_sweep_none.to_owned()
                } else {
                    crate::i18n::fill(w.worktree_swept, "n", gone)
                };
                if !kept.is_empty() {
                    said = format!(
                        "{said}\n{}",
                        crate::i18n::fill(w.worktree_swept_kept, "names", kept.join(", "))
                    );
                }
                self.toast = Some(said);
                self.list_worktrees()
            }
            Msg::SetWorktreePrefix(prefix) => self.set(|p| p.more.worktree_prefix = prefix),
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

/// Where a message to start a new worktree with waits for that worktree's window: one file for
/// each folder, under the home (not in its arguments, where any process can read it).
pub(crate) fn handoff_file(home: &Path, folder: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let real = folder.canonicalize().unwrap_or_else(|_| folder.to_path_buf());
    let mut hash = std::hash::DefaultHasher::new();
    real.hash(&mut hash);
    home.join("gui")
        .join("handoff")
        .join(format!("{:016x}.md", hash.finish()))
}

/// `text` left for the window opened on `folder`.
pub(crate) fn leave_handoff(home: &Path, folder: &Path, text: &str) -> std::io::Result<()> {
    let file = handoff_file(home, folder);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, text)
}

/// The message left for a window on `folder`, taken: read once, and gone.
pub(crate) fn take_handoff(home: &Path, folder: &Path) -> Option<String> {
    let file = handoff_file(home, folder);
    let text = std::fs::read_to_string(&file).ok()?;
    let _ = std::fs::remove_file(&file);
    (!text.trim().is_empty()).then_some(text)
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

        let made = crate::system::make_worktree(&repo, &home, Some("older"), "").unwrap();
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

        // The branches offered to make one from: here, the latest committed to first.
        let offered = bases(&repo);
        assert_eq!(offered.first().map(String::as_str), Some("main"), "{offered:?}");
        assert!(offered.iter().any(|b| b == "older") && offered.iter().any(|b| b == "theirs"));
        // With no remote, nothing to fetch; with one that is not there, why it failed.
        fetch(&repo).unwrap();
        run(
            &repo,
            &["remote", "add", "origin", base.join("nowhere").to_str().unwrap()],
        );
        assert!(fetch(&repo).is_err());

        // The settings' start of the branch's name; one git would take as an option is not.
        let named = crate::system::make_worktree(&repo, &home, None, "me/").unwrap();
        let named_tree = list(&repo, &home);
        assert!(named_tree[0].branch.as_deref().is_some_and(|b| b.starts_with("me/")));
        // Old: last used before the time given, and never the one this window is in.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(86_400);
        assert_eq!(old(&named_tree, later, &repo), vec![named_tree[0].path.clone()]);
        assert!(old(&named_tree, std::time::UNIX_EPOCH, &repo).is_empty());
        assert!(old(&named_tree, later, &named).is_empty());
        // Made in the same second as those before it would share their names: they go first.
        remove(&repo, &named).unwrap();
        let first = git(&repo, &["branch".as_ref(), "--list".as_ref(), "sasacode/*".as_ref()]).unwrap();
        run(&repo, &["branch", "-d", first.trim()]);
        let odd = crate::system::make_worktree(&repo, &home, None, "--force").unwrap();
        assert!(list(&repo, &home).iter().any(|t| crate::system::same_dir(&t.path, &odd)
            && t.branch.as_deref().is_some_and(|b| b.starts_with("sasacode/"))));
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
