//! What the window is and where its things are: the versions, the folders, what the runs wrote on
//! stderr; the instruction files the agent reads here; a review of the changes, asked for.

use std::path::{Path, PathBuf};

use iced::Task;

use crate::app::{App, Msg};
use crate::system;

/// The window's own version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The AGENTS.md files a run in `cwd` reads, as the CLI finds them: the global one in `home`,
/// then from the repository's top down to `cwd`. Only those that are there.
pub fn instruction_files(home: Option<&Path>, cwd: &Path) -> Vec<PathBuf> {
    let mut chain = Vec::new();
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        chain.insert(0, d.join("AGENTS.md"));
        if d.join(".git").exists() {
            break;
        }
        dir = d.parent();
    }
    home.map(|h| h.join("AGENTS.md"))
        .into_iter()
        .chain(chain)
        .filter(|p| p.is_file())
        .collect()
}

impl App {
    /// The versions, the paths and the log as text, for a report of something wrong.
    pub(crate) fn about_text(&self) -> String {
        let w = self.s.w();
        let unknown = w.about_unknown.to_owned();
        let mut lines = vec![
            format!("sasacode-gui {VERSION}"),
            format!("sasacode {}", self.listing.version.clone().unwrap_or(unknown.clone())),
            format!("{}: {}", w.about_program, self.program.display()),
            format!("{}: {}", w.about_folder, self.cwd.display()),
            format!(
                "{}: {}",
                w.about_home,
                self.home.as_ref().map_or(unknown, |h| h.display().to_string())
            ),
            format!(
                "{}: {} {}",
                w.about_system,
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        ];
        if !self.log.is_empty() {
            lines.push(String::new());
            lines.extend(self.log.iter().cloned());
        }
        lines.join("\n")
    }

    /// The about screen's messages, the review asked for and the instruction files; any other
    /// comes back.
    pub(crate) fn update_about(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let task = match msg {
            Msg::AskReview => {
                self.overlay = None;
                let prompt = self.s.w().review_prompt.to_owned();
                self.put_in_input(&prompt)
            }
            Msg::OpenInstructions(path) => {
                // Not there yet: made empty, for the user to write in.
                if !path.exists()
                    && let Err(e) = std::fs::write(&path, "")
                {
                    self.toast = Some(format!("{}: {e}", path.display()));
                    return Ok(Task::none());
                }
                system::editor_with(&path, &self.prefs.more.editor);
                Task::none()
            }
            Msg::OpenHome => {
                if let Some(home) = &self.home {
                    let _ = std::fs::create_dir_all(home);
                    system::open(home);
                }
                Task::none()
            }
            other => return Err(other),
        };
        Ok(task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_instruction_files_go_from_the_global_one_to_the_repositorys_top_down_to_the_folder() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-agents-{}", std::process::id()));
        let (home, repo) = (dir.join("home"), dir.join("repo"));
        let deep = repo.join("pkg").join("a");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        for p in [home.join("AGENTS.md"), repo.join("AGENTS.md"), deep.join("AGENTS.md")] {
            std::fs::write(p, "x").unwrap();
        }
        // One above the repository is not read.
        std::fs::write(dir.join("AGENTS.md"), "x").unwrap();
        let found = instruction_files(Some(&home), &deep);
        assert_eq!(
            found,
            [home.join("AGENTS.md"), repo.join("AGENTS.md"), deep.join("AGENTS.md")]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
