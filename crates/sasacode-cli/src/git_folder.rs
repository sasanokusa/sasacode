//! Whether the shipped plugins that run git (git-context, jev-guard) may run it in this folder.
//! A repository's config can name programs git starts, so a folder is asked about once, on the
//! terminal, and the answer kept with the desktop window's in `git-folders.json`
//! ([`sasacode_protocol::git_folders`]). A project with nothing that needs trust is not, by that
//! alone, a folder git may run in.

use std::future::Future;
use std::path::Path;

use sasacode_host::{plain_line, t};
use sasacode_protocol::git_folders;

use crate::config::{ConfigFiles, Settings, load_config};
use crate::setup::PluginChoice;

/// What the user said of the project's config this run.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProjectGit {
    /// Something in it needs trust, and the user trusted it for good (remembered, or answered now).
    pub trusted: bool,
    /// `--trust-project` was given: git may run for this run only, whether or not anything in the
    /// config needs trust, and the folder is not kept.
    pub once: bool,
}

impl ProjectGit {
    pub fn said(needs_trust: bool, for_good: bool, trust_project_flag: bool) -> ProjectGit {
        ProjectGit {
            trusted: needs_trust && for_good,
            once: trust_project_flag,
        }
    }
}

/// The list of folders git may run in, written when there is none yet, from the conversations
/// kept so far. It is done first in every run, whatever plugins load, so that a conversation kept
/// from now on does not make its folder known.
pub fn start_the_list(home: &Path) {
    let _ = git_folders::load(home, &[]);
}

/// Whether a plugin that runs git loads with the config this run uses.
pub fn runs_git(cwd: &Path, trusted: bool, files: &ConfigFiles) -> bool {
    let Ok(loaded) = load_config(cwd, trusted, Some(files.clone())) else {
        return false;
    };
    let choice = PluginChoice::of(&Settings::of(&loaded.config));
    ["git-context", "jev-guard"]
        .iter()
        .filter_map(|name| sasacode_bundled::find(name))
        .any(|p| choice.loads(p))
}

/// Whether git may run in `cwd`: it is kept as known, its project was trusted (for good: kept from
/// now on), or the user allows it when `ask` is given (kept too). Nothing is asked or kept when no
/// plugin that runs git loads; the folders used before are known the first time (see
/// [`git_folders::load`]).
pub async fn allowed_in(
    home: &Path,
    cwd: &Path,
    wanted: bool,
    project: ProjectGit,
    ask: Option<impl Future<Output = bool>>,
) -> bool {
    if !wanted {
        return false;
    }
    if git_folders::is_known(home, cwd) {
        return true;
    }
    if project.trusted {
        let _ = git_folders::add(home, cwd, &[]);
        return true;
    }
    if project.once {
        return true;
    }
    let Some(ask) = ask else {
        return false;
    };
    let yes = ask.await;
    if yes {
        let _ = git_folders::add(home, cwd, &[]);
    }
    yes
}

/// Ask on the terminal whether git may run in `cwd`.
pub async fn ask_git(cwd: &str) -> bool {
    eprint!(
        "\n{}",
        t!(
            "Run git in {cwd}? A repository's config can start programs. [y/N] ",
            cwd = plain_line(cwd)
        )
    );
    let answer = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        line
    })
    .await
    .unwrap_or_default();
    let answer = answer.trim().to_lowercase();
    answer == "y" || answer == "yes"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn folders() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let (home, cwd) = (dir.path().join("home"), dir.path().join("proj"));
        std::fs::create_dir_all(&cwd).unwrap();
        (dir, home, cwd)
    }

    fn never() -> Option<std::future::Ready<bool>> {
        None
    }

    #[tokio::test]
    async fn a_folder_with_no_config_is_not_allowed_until_it_is_known() {
        let (_d, home, cwd) = folders();
        assert!(!allowed_in(&home, &cwd, true, ProjectGit::default(), never()).await);
        git_folders::add(&home, &cwd, &[]).unwrap();
        assert!(allowed_in(&home, &cwd, true, ProjectGit::default(), never()).await);
    }

    #[tokio::test]
    async fn trust_project_allows_git_for_the_run_in_a_folder_with_nothing_that_needs_trust() {
        // Nothing needs trust, `--trust-project` given: allowed for this run, not kept.
        let said = ProjectGit::said(false, false, true);
        assert!(said.once && !said.trusted);
        let (_d, home, cwd) = folders();
        assert!(allowed_in(&home, &cwd, true, said, never()).await);
        assert!(!git_folders::is_known(&home, &cwd));
        // Without the flag, a saved trust of a project with nothing to trust is not enough.
        let said = ProjectGit::said(false, true, false);
        assert!(!said.once && !said.trusted);
        assert!(ProjectGit::said(true, true, false).trusted);
    }

    /// A conversation kept for `folder`, as the CLI keeps one.
    fn conversation(home: &Path, folder: &Path) {
        let dir = sasacode_protocol::session_dir(home, folder);
        std::fs::create_dir_all(&dir).unwrap();
        let head = serde_json::json!({ "type": "session", "cwd": folder.to_string_lossy() });
        std::fs::write(dir.join("a.jsonl"), format!("{head}\n")).unwrap();
    }

    #[test]
    fn the_list_is_started_once_from_the_conversations_kept_then() {
        let (d, home, cwd) = folders();
        let later = d.path().join("later");
        std::fs::create_dir_all(&later).unwrap();
        conversation(&home, &cwd);
        start_the_list(&home);
        assert!(git_folders::is_known(&home, &cwd));
        // A conversation kept afterwards, or a second start, makes nothing known.
        conversation(&home, &later);
        start_the_list(&home);
        assert!(!git_folders::is_known(&home, &later));
    }

    #[tokio::test]
    async fn trusting_the_project_allows_it_and_keeps_it_but_once_does_not_keep_it() {
        let (_d, home, cwd) = folders();
        let once = ProjectGit {
            trusted: false,
            once: true,
        };
        assert!(allowed_in(&home, &cwd, true, once, never()).await);
        assert!(!git_folders::is_known(&home, &cwd));
        let trusted = ProjectGit {
            trusted: true,
            once: false,
        };
        assert!(allowed_in(&home, &cwd, true, trusted, never()).await);
        assert!(git_folders::is_known(&home, &cwd));
    }

    #[tokio::test]
    async fn asked_only_when_a_plugin_runs_git_and_the_folder_is_not_known() {
        let (_d, home, cwd) = folders();
        let asked = AtomicBool::new(false);
        let ask = |yes: bool| {
            let asked = &asked;
            Some(async move {
                asked.store(true, Ordering::SeqCst);
                yes
            })
        };
        // No plugin that runs git: nothing asked, nothing kept.
        assert!(!allowed_in(&home, &cwd, false, ProjectGit::default(), ask(true)).await);
        assert!(!asked.load(Ordering::SeqCst) && git_folders::read(&home).unwrap().is_none());
        // "No" is not kept: asked again next time.
        assert!(!allowed_in(&home, &cwd, true, ProjectGit::default(), ask(false)).await);
        assert!(asked.swap(false, Ordering::SeqCst) && !git_folders::is_known(&home, &cwd));
        assert!(allowed_in(&home, &cwd, true, ProjectGit::default(), ask(true)).await);
        assert!(asked.swap(false, Ordering::SeqCst) && git_folders::is_known(&home, &cwd));
        // Known now: not asked.
        assert!(allowed_in(&home, &cwd, true, ProjectGit::default(), ask(false)).await);
        assert!(!asked.load(Ordering::SeqCst));
    }
}
