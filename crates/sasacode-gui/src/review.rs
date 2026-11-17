//! What the git tab does besides staging and committing: comments on lines of a diff that go with
//! the next message, the branches to switch to, and the branch's pull request with its checks and
//! what its reviewers said.

use std::path::{Path, PathBuf};

use iced::Task;
use iced::widget::{operation, text_editor};

use crate::app::{App, Msg};
use crate::git::{self, Pr};

/// How many branches one look asks `gh` about: each is a request.
const SWEPT_BRANCHES: usize = 30;

/// The field of a comment being written.
pub const COMMENT: &str = "comment";

/// A comment on a line of a diff, waiting to go with the next message.
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub path: PathBuf,
    pub line: usize,
    pub text: String,
}

/// The comment being written under a line: in the staged part of the file's diff, or the other.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub path: PathBuf,
    pub line: usize,
    pub staged: bool,
    pub text: String,
}

/// `@path:line`, as a dropped file's path is given.
pub fn place(path: &Path, line: usize, cwd: &Path) -> String {
    let at = crate::composer::path_ref(path, cwd);
    match at.strip_suffix('"') {
        Some(quoted) => format!("{quoted}:{line}\""),
        None => format!("{at}:{line}"),
    }
}

/// `text` and after it the comments, each with where it was made, as the agent is sent them.
pub fn with_comments(text: &str, comments: &[Comment], cwd: &Path, head: &str) -> String {
    if comments.is_empty() {
        return text.to_owned();
    }
    let mut out = text.trim_end().to_owned();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(head);
    for c in comments {
        out.push_str(&format!("\n- {}: {}", place(&c.path, c.line, cwd), c.text.trim()));
    }
    out
}

impl App {
    /// The comments', the branches' and the pull request's messages; any other comes back.
    pub(crate) fn update_review(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let task = match msg {
            Msg::CommentAt(path, line, staged) => {
                self.commenting = Some(Draft {
                    path,
                    line,
                    staged,
                    text: String::new(),
                });
                operation::focus(COMMENT)
            }
            Msg::CommentText(text) => {
                if let Some(draft) = &mut self.commenting {
                    draft.text = text;
                }
                Task::none()
            }
            Msg::CommentSave => match self.commenting.take() {
                // Nothing written: where it is goes into the input, as a click did before.
                Some(draft) if draft.text.trim().is_empty() => self.update(Msg::Quote(draft.path, draft.line)),
                Some(draft) => {
                    self.chat.comments.push(Comment {
                        path: draft.path,
                        line: draft.line,
                        text: draft.text.trim().to_owned(),
                    });
                    Task::none()
                }
                None => Task::none(),
            },
            Msg::CommentQuote => match self.commenting.take() {
                Some(draft) => self.update(Msg::Quote(draft.path, draft.line)),
                None => Task::none(),
            },
            Msg::CommentCancel => {
                self.commenting = None;
                Task::none()
            }
            Msg::CommentRemove(i) => {
                if i < self.chat.comments.len() {
                    self.chat.comments.remove(i);
                }
                Task::none()
            }
            Msg::Branches(list) => {
                self.branch_list = Some(list);
                Task::none()
            }
            Msg::PrRefresh => self.refresh_pr(),
            Msg::PrLoaded(pr) => {
                let before = std::mem::replace(&mut self.pr, pr.map(|pr| *pr));
                // Its checks finished while they were watched: said, as a run that ends is.
                if let Some(failed) = git::checks_finished(before.as_ref(), self.pr.as_ref()) {
                    self.tell_checks(failed);
                }
                // Kept by branch, for the conversations started on it.
                if let (Some(pr), Some(branch)) = (&self.pr, &self.git.branch) {
                    self.prs.insert(branch.clone(), (pr.number, pr.state.clone()));
                }
                self.pr_loading = false;
                Task::none()
            }
            Msg::FixChecks => {
                let Some(pr) = self.pr.clone() else {
                    return Ok(Task::none());
                };
                self.toast = Some(self.s.w().fix_reading.to_owned());
                let cwd = self.cwd.clone();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || git::failed_logs(&cwd, &pr))
                            .await
                            .unwrap_or_default()
                    },
                    Msg::FixReady,
                )
            }
            Msg::FixReady(logs) => {
                self.toast = None;
                let url = self.pr.as_ref().map(|pr| pr.url.clone()).unwrap_or_default();
                let prompt = crate::i18n::fill(
                    &crate::i18n::fill(self.s.w().fix_prompt, "url", url),
                    "logs",
                    logs.trim(),
                );
                self.put_in_input(&prompt)
            }
            Msg::Address(i) => {
                let Some(remark) = self.pr.as_ref().and_then(|pr| pr.comments.get(i)).cloned() else {
                    return Ok(Task::none());
                };
                let who = match (&remark.path, remark.line) {
                    (Some(path), Some(line)) => format!(
                        "{} ({})",
                        remark.author,
                        place(&self.git.top.join(path), line, &self.cwd)
                    ),
                    _ => remark.author.clone(),
                };
                let prompt = crate::i18n::fill(
                    &crate::i18n::fill(self.s.w().address_prompt, "who", who),
                    "body",
                    remark.body.trim(),
                );
                self.put_in_input(&prompt)
            }
            Msg::SetSuggest(on) => {
                let _ = self.set(|p| p.suggest = on);
                if !on {
                    self.suggestion = None;
                }
                Task::none()
            }
            Msg::SetAutoArchive(on) => {
                let _ = self.set(|p| p.auto_archive = on);
                self.sweep()
            }
            Msg::Sweep => self.sweep(),
            Msg::Swept(ids) => {
                let ids: Vec<String> = ids.into_iter().filter(|id| !self.prefs.archived.contains(id)).collect();
                if !ids.is_empty() {
                    self.toast = Some(crate::i18n::fill(self.s.w().swept, "n", ids.len()));
                    let _ = self.set(|p| {
                        for id in ids {
                            p.pinned.retain(|x| *x != id);
                            p.archived.insert(id);
                        }
                    });
                }
                Task::none()
            }
            other => return Err(other),
        };
        Ok(task)
    }

    /// Looks, away from the window, for the conversations of this folder whose branch's pull
    /// request was merged or closed, when the settings say to archive them. The branch checked out
    /// now is left: its conversation may be going on.
    pub(crate) fn sweep(&mut self) -> Task<Msg> {
        if !self.prefs.auto_archive {
            return Task::none();
        }
        let here = self.git.branch.clone();
        let mut by_branch: Vec<(String, Vec<String>)> = Vec::new();
        for s in &self.sessions {
            let Some(branch) = self.prefs.branches.get(&s.id) else {
                continue;
            };
            if self.prefs.archived.contains(&s.id) || Some(branch) == here.as_ref() {
                continue;
            }
            match by_branch.iter_mut().find(|(b, _)| b == branch) {
                Some((_, ids)) => ids.push(s.id.clone()),
                None => by_branch.push((branch.clone(), vec![s.id.clone()])),
            }
        }
        if by_branch.is_empty() {
            return Task::none();
        }
        by_branch.truncate(SWEPT_BRANCHES);
        let cwd = self.cwd.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    by_branch
                        .into_iter()
                        .filter(|(branch, _)| {
                            matches!(git::pr_state(&cwd, branch).as_deref(), Some("MERGED" | "CLOSED"))
                        })
                        .flat_map(|(_, ids)| ids)
                        .collect()
                })
                .await
                .unwrap_or_default()
            },
            Msg::Swept,
        )
    }

    /// `text` in the input after what is typed there, the keyboard given to it.
    pub(crate) fn put_in_input(&mut self, text: &str) -> Task<Msg> {
        let typed = self.chat.editor.text();
        let all = if typed.trim().is_empty() {
            text.to_owned()
        } else {
            format!("{}\n\n{text}", typed.trim_end())
        };
        self.chat.editor = text_editor::Content::with_text(&all);
        self.chat
            .editor
            .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
        self.dismissed = true;
        operation::focus(crate::app::INPUT)
    }

    /// Whether the branch's pull request has checks still running: read again now and then
    /// until they finish.
    pub(crate) fn watches_checks(&self) -> bool {
        !self.pr_loading && self.pr.as_ref().is_some_and(git::Pr::pending)
    }

    /// Says that the pull request's checks finished, how, and toasts it in the window too.
    fn tell_checks(&mut self, failed: usize) {
        let w = self.s.w();
        let what = if failed == 0 {
            w.checks_passed.to_owned()
        } else {
            crate::i18n::fill(w.checks_failed, "n", failed)
        };
        self.toast = Some(what.clone());
        if self.prefs.notify && !self.prefs.more.quiet_done {
            let title = self
                .pr
                .as_ref()
                .map_or_else(|| "sasacode".to_owned(), |pr| format!("#{} {}", pr.number, pr.title));
            crate::system::notify(&title, &what, self.prefs.more.sound);
        }
    }

    /// The branch's pull request read again, away from the window.
    pub(crate) fn refresh_pr(&mut self) -> Task<Msg> {
        if !self.git.repo || self.git.branch.is_none() {
            self.pr = None;
            return Task::none();
        }
        self.pr_loading = true;
        let cwd = self.cwd.clone();
        Task::perform(
            async move { tokio::task::spawn_blocking(move || git::pr(&cwd)).await.ok().flatten() },
            |pr| Msg::PrLoaded(pr.map(Box::new)),
        )
    }

    /// The branches listed again, away from the window, for the list to switch to one.
    pub(crate) fn list_branches(&mut self) -> Task<Msg> {
        self.branch_list = None;
        let cwd = self.cwd.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || git::branches(&cwd))
                    .await
                    .unwrap_or_default()
            },
            Msg::Branches,
        )
    }

    /// The pull request's state, as the git tab says it.
    pub(crate) fn pr_state(&self, pr: &Pr) -> &'static str {
        let w = self.s.w();
        match pr.state.as_str() {
            "MERGED" => w.pr_merged,
            "CLOSED" => w.pr_closed,
            _ => w.pr_open,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_follow_the_message_each_with_its_place() {
        let cwd = Path::new("/work/p");
        let comments = vec![
            Comment {
                path: cwd.join("src/a.rs"),
                line: 12,
                text: " ここは逆では？ ".into(),
            },
            Comment {
                path: cwd.join("my file.rs"),
                line: 3,
                text: "名前を変える".into(),
            },
        ];
        assert_eq!(
            with_comments("直して\n", &comments, cwd, "差分へのコメント:"),
            "直して\n\n差分へのコメント:\n- @src/a.rs:12: ここは逆では？\n- @\"my file.rs:3\": 名前を変える"
        );
        assert_eq!(
            with_comments("", &comments[..1], cwd, "C:"),
            "C:\n- @src/a.rs:12: ここは逆では？"
        );
        assert_eq!(with_comments("そのまま", &[], cwd, "C:"), "そのまま");
    }
}
