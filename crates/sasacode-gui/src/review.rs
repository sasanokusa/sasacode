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

/// How many times a window starts a conversation to fix one pull request's failed checks.
pub const AUTO_FIXES: usize = 3;

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

/// How long one finding of a review is kept, in characters.
const FINDING_CHARS: usize = 600;

/// Whether `prompt` is the window's request for a review, in either language: it begins with the
/// request's first sentence, whatever was changed or added after it.
pub fn asks_review(prompt: &str) -> bool {
    let prompt = prompt.trim_start();
    [crate::words::JA.review_prompt, crate::words::EN.review_prompt]
        .iter()
        .map(|asked| asked.split_inclusive(['。', '.']).next().unwrap_or(asked))
        .any(|first| prompt.starts_with(first))
}

/// What the agent found when the newest message asked for a review, each on the line of the file
/// in the folder it names: none when it was not asked, or its reply is not there yet.
pub fn of_review(transcript: &crate::transcript::Transcript, cwd: &Path) -> Vec<Comment> {
    match transcript.exchanges.last() {
        Some(ex) if asks_review(&ex.prompt) && ex.answer().is_some() => findings(&ex.answer_text(), cwd),
        _ => Vec::new(),
    }
}

/// The findings of a review: each list item, heading or row of a table that names a line of a
/// file in `cwd` (`path:12`, `path:12-14`, `path#L12`), with what it says, the lines that carry it
/// on with it. Its first such line is where it goes. A paragraph of its own is not one: what is
/// said around the findings names files too.
pub fn findings(reply: &str, cwd: &Path) -> Vec<Comment> {
    let mut out: Vec<Comment> = Vec::new();
    let mut fence = false;
    // The finding the lines that follow carry on, until a blank line, an item or a heading.
    let mut open: Option<usize> = None;
    for raw in reply.lines() {
        let line = raw.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            fence = !fence;
            open = None;
            continue;
        }
        if fence || line.is_empty() {
            open = None;
            continue;
        }
        let starts = item(line) || line.starts_with('#') || line.starts_with('|');
        match first_place(line, cwd).filter(|_| starts) {
            Some((path, at, word)) => {
                out.push(Comment {
                    path,
                    line: at,
                    text: plain(line, word),
                });
                open = Some(out.len() - 1);
            }
            None => match open {
                Some(i) if !starts => {
                    let c = &mut out[i];
                    if c.text.chars().count() < FINDING_CHARS {
                        c.text.push(' ');
                        c.text.push_str(&plain(line, ""));
                    }
                }
                _ => open = None,
            },
        }
    }
    for c in &mut out {
        if c.text.chars().count() > FINDING_CHARS {
            c.text = c.text.chars().take(FINDING_CHARS).collect::<String>() + "…";
        }
    }
    out
}

/// Whether a line begins an item of a list.
fn item(line: &str) -> bool {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    ["- ", "* ", "+ "].iter().any(|m| line.starts_with(m))
        || (digits > 0 && (line[digits..].starts_with(". ") || line[digits..].starts_with(") ")))
}

/// A line of Markdown as it reads: without the mark of its item or heading, the bars of a table,
/// emphasis, code marks and the targets of links; and without `place` (where it is, said apart)
/// when it begins with it.
fn plain(line: &str, place: &str) -> String {
    let mut rest = line.trim_start_matches('#').trim_start();
    if item(rest) {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        rest = rest[1..].trim_start();
    }
    let rest = if rest.starts_with('|') {
        let cells: Vec<&str> = rest.split('|').map(str::trim).filter(|c| !c.is_empty()).collect();
        cells.join(" — ")
    } else {
        rest.to_owned()
    };
    let mut said = unlink(&rest).replace("**", "").replace("__", "").replace('`', "");
    if !place.is_empty()
        && let Some(after) = said.trim_start_matches('@').strip_prefix(place.trim_start_matches('@'))
    {
        // Only when what follows is set apart from it: `path:12: what`, `path:12 — what`.
        let apart = after.trim_start().starts_with(|c: char| "：:—–-".contains(c));
        let after = after.trim_start_matches(|c: char| c.is_whitespace() || "：:—–-".contains(c));
        if apart && !after.is_empty() {
            said = after.to_owned();
        }
    }
    said.trim().to_owned()
}

/// `[text](target)` as its text alone.
fn unlink(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some((inner, after)) = rest[open + 1..].split_once("](") else {
            break;
        };
        let Some(close) = after.find(')') else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(inner);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// The first line of a file in `cwd` the text names, read as `path:12`, `path:12-14`, `path:12:3`
/// or `path#L12`; a path may have `@` or `./` before it, or be the folder's own absolute path.
/// The file as the system names it (`/var` is `/private/var` on macOS, as git says it), and the
/// word that named it.
fn first_place<'a>(line: &'a str, cwd: &Path) -> Option<(PathBuf, usize, &'a str)> {
    let breaks = |c: char| {
        c.is_whitespace()
            || !c.is_ascii()
            || matches!(
                c,
                '`' | '(' | ')' | '[' | ']' | '*' | ',' | '"' | '\'' | '<' | '>' | '|'
            )
    };
    line.split(breaks).find_map(|raw| {
        let word = raw.trim_start_matches('@');
        // A drive's colon is not where the line is said.
        let drive = word.as_bytes().get(1) == Some(&b':') && matches!(word.as_bytes().get(2), Some(b'\\' | b'/'));
        let (head, word) = word.split_at(if drive { 2 } else { 0 });
        let (path, at) = match word.split_once("#L") {
            Some(split) => split,
            None => word.split_once(':')?,
        };
        let digits = at.chars().take_while(char::is_ascii_digit).count();
        let n: usize = at[..digits].parse().ok().filter(|&n| n > 0)?;
        let path = format!("{head}{}", path.strip_prefix("./").unwrap_or(path));
        let path = path.as_str();
        if path.is_empty()
            || Path::new(path)
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            return None;
        }
        let full = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            cwd.join(path)
        };
        (full.starts_with(cwd) && full.is_file()).then(|| (full.canonicalize().unwrap_or(full), n, raw))
    })
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
            Msg::ReviewTake(i) => {
                if i < self.chat.review.len() {
                    let found = self.chat.review.remove(i);
                    self.chat.comments.push(found);
                }
                Task::none()
            }
            Msg::ReviewDrop(i) => {
                if i < self.chat.review.len() {
                    self.chat.review.remove(i);
                }
                Task::none()
            }
            Msg::ReviewClear => {
                self.chat.review.clear();
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
                let mut fix = Task::none();
                if let Some(failed) = git::checks_finished(before.as_ref(), self.pr.as_ref()) {
                    self.tell_checks(failed);
                    // Failed: their logs read for a conversation to fix them, when the settings
                    // say so, a few times a pull request at most.
                    if failed > 0
                        && self.prefs.more.auto_fix
                        && let Some(pr) = self.pr.clone()
                    {
                        let tries = self.auto_fixes.entry(pr.number).or_default();
                        if *tries < AUTO_FIXES {
                            *tries += 1;
                            let cwd = self.cwd.clone();
                            fix = Task::perform(
                                async move {
                                    tokio::task::spawn_blocking(move || git::failed_logs(&cwd, &pr))
                                        .await
                                        .unwrap_or_default()
                                },
                                Msg::AutoFixReady,
                            );
                        }
                    }
                }
                // Kept by branch, for the conversations started on it.
                if let (Some(pr), Some(branch)) = (&self.pr, &self.git.branch) {
                    self.prs.insert(branch.clone(), (pr.number, pr.state.clone()));
                }
                self.pr_loading = false;
                fix
            }
            Msg::AutoFixReady(logs) => {
                let Some(pr) = self.pr.clone() else {
                    return Ok(Task::none());
                };
                let w = self.s.w();
                let prompt = crate::i18n::fill(&crate::i18n::fill(w.fix_prompt, "url", &pr.url), "logs", logs.trim());
                // Beside the one shown, as an automation runs: in the mode chosen in the window, but
                // never `auto` (`edits` in its place, commands asked); asking for all with none chosen.
                let mut chat = self.new_chat();
                chat.mode = Some(match self.permission {
                    Some("auto") => "edits",
                    Some(mode) => mode,
                    None => "ask",
                });
                let key = chat.key;
                self.parked.push(chat);
                self.toast = Some(crate::i18n::fill(w.auto_fix_started, "n", pr.number));
                self.start_run(key, prompt, false)
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

    #[test]
    fn a_reviews_findings_are_read_with_the_line_of_the_file_each_names() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-findings-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        let reply = "## 指摘\n\n\
            1. **`src/a.rs:12`**：境界の扱いが逆です。\n   12より大きいときに落ちます。\n\
            2. [b.txt](b.txt#L3) のテストがありません\n\
            - ないファイル src/gone.rs:4 は出さない\n\
            - URL https://example.com:8080 も出さない\n\n\
            ```\n- src/a.rs:99 コードの中は読まない\n```\n\n\
            ### src/a.rs:20-24 の名前\n\n\
            見出しの下の段落は別のものです。\n\n\
            まとめ：段落の@./src/a.rs:7は指摘ではない。\n\n\
            | 場所 | 指摘 |\n| --- | --- |\n| src/a.rs:30 | 名前が長い |\n";
        let real = dir.canonicalize().unwrap();
        let found = findings(reply, &dir);
        let got: Vec<(String, usize, &str)> = found
            .iter()
            .map(|c| {
                (
                    c.path.strip_prefix(&real).unwrap().display().to_string(),
                    c.line,
                    c.text.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "src/a.rs".into(),
                    12,
                    "境界の扱いが逆です。 12より大きいときに落ちます。"
                ),
                ("b.txt".into(), 3, "b.txt のテストがありません"),
                ("src/a.rs".into(), 20, "src/a.rs:20-24 の名前"),
                ("src/a.rs".into(), 30, "名前が長い"),
            ]
        );
        assert!(
            asks_review(crate::words::JA.review_prompt)
                && asks_review(&format!("  {}\n特にa.rs", crate::words::EN.review_prompt))
        );
        assert!(asks_review(
            "まだコミットしていない変更（git statusとgit diffで見られます）をレビューしてください。セキュリティだけ見て"
        ));
        assert!(!asks_review("レビューして"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
