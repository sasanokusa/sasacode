//! The folders git is run in without asking: the git tab's status, a turn's snapshots, the files
//! git lists and the sweep of merged branches. A repository's config can name programs that such
//! a run starts, so a folder the window has not been told about is read only once the user asks
//! (the git tab's button), trusts it, or the window made it (a worktree). Kept in `gui.json` by
//! the folder's real path.

use std::collections::BTreeMap;
use std::path::Path;

use iced::Task;

use crate::app::{App, Msg};

/// How a folder is kept: its real path, else as it is spelled.
pub fn key(folder: &Path) -> String {
    std::fs::canonicalize(folder)
        .unwrap_or_else(|_| folder.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// The folders a window before this one was used in, where git already ran: those with a last
/// run kept here and those sasacode keeps conversations for.
pub fn seen_before<'a>(
    kept: impl IntoIterator<Item = &'a str>,
    projects: &[std::path::PathBuf],
) -> BTreeMap<String, bool> {
    kept.into_iter()
        .map(|f| key(Path::new(f)))
        .chain(projects.iter().map(|p| key(p)))
        .map(|k| (k, true))
        .collect()
}

impl App {
    /// Whether git may be run here unasked: the folder is known, or its config is used in this
    /// window.
    pub(crate) fn git_allowed(&self) -> bool {
        self.trust_once || self.git_known(&self.cwd)
    }

    pub(crate) fn git_known(&self, folder: &Path) -> bool {
        let folders = self.prefs.more.git_folders.as_ref();
        folders.is_some_and(|all| all.contains_key(&key(folder)))
    }

    /// The first launch of a window that keeps the folders: those used before are known.
    pub(crate) fn seed_git_folders(&mut self) {
        if self.prefs.more.git_folders.is_some() {
            return;
        }
        let projects = self.home.as_deref().map(crate::system::projects).unwrap_or_default();
        let seen = seen_before(self.prefs.more.folders.keys().map(String::as_str), &projects);
        let _ = self.set(|p| p.more.git_folders = Some(seen));
    }

    /// `folder` known from now on.
    pub(crate) fn know_git(&mut self, folder: &Path) {
        if !self.git_known(folder) {
            let k = key(folder);
            let _ = self.set(|p| {
                p.more.git_folders.get_or_insert_default().insert(k, true);
            });
        }
    }

    /// This window's folder known, and what waited for it done now: the repository read, the
    /// conversations of merged branches looked for, the files listed again by git.
    pub(crate) fn know_git_here(&mut self) -> Task<Msg> {
        let before = self.git_allowed();
        self.know_git(&self.cwd.clone());
        self.git_now_allowed(before)
    }

    /// What waited for git, once it may be run.
    pub(crate) fn git_now_allowed(&mut self, before: bool) -> Task<Msg> {
        if before || !self.git_allowed() {
            return Task::none();
        }
        self.files_asked = false;
        Task::batch([self.refresh_git(), self.sweep()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folders_used_before_are_known_by_their_real_path() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let spelled = a.join("..").join("a");
        let seen = seen_before([spelled.to_str().unwrap()], std::slice::from_ref(&b));
        assert_eq!(seen.keys().cloned().collect::<Vec<_>>(), [key(&a), key(&b)]);
        assert!(seen.contains_key(&key(&std::fs::canonicalize(&a).unwrap())));
    }
}
