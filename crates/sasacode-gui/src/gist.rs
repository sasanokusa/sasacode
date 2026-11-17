//! A conversation shared as a secret gist: its Markdown, as `/export` copies it, given to
//! `gh gist create`. It is asked each time, saying what goes where: a secret gist is not listed,
//! but whoever has its link can read it.

use iced::Task;

use crate::app::{App, Msg};
use crate::i18n::fill;
use crate::overlay::Overlay;

/// The program gists are made with.
const GH: &str = "gh";

/// What a gist is made of.
pub struct Gist {
    pub file: String,
    pub description: String,
    pub markdown: String,
}

impl App {
    /// The conversation shown, as its gist would be; none when nothing was said in it.
    pub(crate) fn gist(&self) -> Option<Gist> {
        if self.chat.transcript.exchanges.is_empty() {
            return None;
        }
        let markdown = crate::export::markdown(self.name(), &self.chat.transcript, self.s);
        let id = self
            .chat
            .session
            .as_deref()
            .map_or("conversation", |id| &id[..id.len().min(8)]);
        let title = self
            .name()
            .map(str::to_owned)
            .or_else(|| self.chat.transcript.title().map(str::to_owned));
        let description = match title {
            Some(t) => format!("sasacode: {}", t.chars().take(100).collect::<String>()),
            None => "sasacode".to_owned(),
        };
        Some(Gist {
            file: format!("sasacode-{id}.md"),
            description,
            markdown,
        })
    }

    /// The gist's messages; any other comes back.
    pub(crate) fn update_gist(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let w = self.s.w();
        let task = match msg {
            Msg::GistAsk => {
                if self.gist().is_some() {
                    self.overlay = Some(Overlay::ConfirmGist);
                } else {
                    self.toast = Some(w.gist_empty.to_owned());
                }
                Task::none()
            }
            Msg::GistCreate => {
                self.overlay = None;
                let Some(gist) = self.gist() else {
                    return Ok(Task::none());
                };
                if self.gisting {
                    return Ok(Task::none());
                }
                self.gisting = true;
                self.toast = Some(w.gist_making.to_owned());
                let cwd = self.cwd.clone();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            crate::git::gist(&cwd, GH, &gist.file, &gist.description, &gist.markdown)
                        })
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()))
                    },
                    Msg::GistMade,
                )
            }
            Msg::GistMade(made) => {
                self.gisting = false;
                match made {
                    Ok(url) => {
                        self.toast = Some(fill(w.gist_made, "url", &url));
                        iced::clipboard::write(url)
                    }
                    Err(why) => {
                        self.toast = Some(fill(w.gist_failed, "why", why));
                        Task::none()
                    }
                }
            }
            other => return self.update_automations(other),
        };
        Ok(task)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    /// A `gh` that keeps what it was given and says a gist's URL, as `gh gist create` does.
    fn stub(dir: &std::path::Path, ok: bool) -> std::path::PathBuf {
        let path = dir.join("gh");
        let script = if ok {
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\ncat > \"$(dirname \"$0\")/stdin\"\necho '- Creating gist sasacode-0a1b2c3d.md' >&2\necho 'https://gist.github.com/someone/0123abcd'\n"
        } else {
            "#!/bin/sh\ncat > /dev/null\necho 'To get started with GitHub CLI, please run:  gh auth login' >&2\nexit 4\n"
        };
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sasacode-gist-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_gist_is_made_secret_of_the_markdown_and_its_url_is_said() {
        let dir = dir("ok");
        let gh = stub(&dir, true);
        let url = crate::git::gist(
            &dir,
            gh.to_str().unwrap(),
            "sasacode-0a1b2c3d.md",
            "sasacode: 再試行",
            "# 再試行\n\n本文\n",
        );
        assert_eq!(url.as_deref(), Ok("https://gist.github.com/someone/0123abcd"));
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        assert_eq!(
            args,
            [
                "gist",
                "create",
                "--filename",
                "sasacode-0a1b2c3d.md",
                "--desc",
                "sasacode: 再試行",
                "-"
            ]
        );
        assert!(!args.iter().any(|a| *a == "--public" || *a == "-p"), "never public");
        assert_eq!(
            std::fs::read_to_string(dir.join("stdin")).unwrap(),
            "# 再試行\n\n本文\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_gist_not_made_says_why_in_a_line() {
        let dir = dir("fail");
        let gh = stub(&dir, false);
        let why = crate::git::gist(&dir, gh.to_str().unwrap(), "a.md", "d", "x").unwrap_err();
        assert_eq!(why, "To get started with GitHub CLI, please run:  gh auth login");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
