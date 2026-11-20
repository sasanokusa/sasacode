//! The folders git is run in without asking: the git tab's status, a turn's snapshots, the files
//! git lists and the sweep of merged branches. A repository's config can name programs that such
//! a run starts, so a folder the window has not been told about is read only once the user asks
//! (the git tab's button), trusts it, or the window made it (a worktree). Kept with the CLI's, in
//! `git-folders.json` in sasacode's home ([`sasacode_protocol::git_folders`]), by real path.

use std::path::{Path, PathBuf};

use iced::Task;
use sasacode_protocol::git_folders;

use crate::app::{App, Msg};

impl App {
    /// Whether git may be run here unasked: the folder is known, or its config is used in this
    /// window.
    pub(crate) fn git_allowed(&self) -> bool {
        self.trust_once || self.git_known(&self.cwd)
    }

    pub(crate) fn git_known(&self, folder: &Path) -> bool {
        self.git_folders.contains_key(&git_folders::key(folder))
    }

    /// The folders this window's last runs were in (`gui.json`): used before, where git already ran.
    fn folders_used(&self) -> Vec<PathBuf> {
        self.prefs.more.folders.keys().map(PathBuf::from).collect()
    }

    /// The folders kept, read at launch; the first time, those used before are written as known.
    pub(crate) fn seed_git_folders(&mut self) {
        if let Some(home) = self.git_home.clone() {
            self.git_folders = git_folders::load(&home, &self.folders_used());
        }
    }

    /// `folder` known from now on, here and to the CLI.
    pub(crate) fn know_git(&mut self, folder: &Path) {
        if self.git_known(folder) {
            return;
        }
        let added = self
            .git_home
            .clone()
            .map(|home| git_folders::add(&home, folder, &self.folders_used()));
        match added {
            Some(Ok(all)) => self.git_folders = all,
            _ => {
                self.git_folders.insert(git_folders::key(folder), true);
            }
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
