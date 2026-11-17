//! One question to the model, apart from any conversation: a commit message, a pull request's
//! title and description, or what the user may say next. Asked of `sasacode -p` with nothing saved,
//! one turn and nothing let run, what it is written from piped in.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use iced::Task;
use iced::widget::text_editor;
use tokio::io::AsyncWriteExt;

use crate::app::{App, Msg};
use crate::git::{Op, PrContext};
use crate::overlay::Overlay;
use crate::transcript::Step;

/// How long an answer is waited for.
const WAIT: Duration = Duration::from_secs(180);
/// How much of a diff the model is given.
const DIFF_MAX: usize = 24 * 1024;
/// How much of a reply the next message is guessed from.
const REPLY_MAX: usize = 6 * 1024;

/// What is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Draft {
    Commit,
    Pr,
    /// What the user may say next in the conversation of this key, after this many messages.
    Next(u64, usize),
}

/// A pull request being written before it is made.
#[derive(Debug)]
pub struct PrDraft {
    pub title: String,
    pub body: text_editor::Content,
    pub context: PrContext,
    /// Made as a draft, not ready for review yet.
    pub draft: bool,
}

/// The start of `text`, at most `max` bytes, cut at a line; said to be cut if it is.
fn head(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let end = text[..end].rfind('\n').unwrap_or(end);
    format!("{}\n[… cut]", &text[..end])
}

/// The answer as it is meant: the fence a model may put around it taken off.
fn clean(answer: &str) -> String {
    let answer = answer.trim();
    let inner = answer
        .strip_prefix("```")
        .and_then(|rest| rest.split_once('\n'))
        .and_then(|(_, rest)| rest.trim_end().strip_suffix("```"));
    inner.unwrap_or(answer).trim().to_owned()
}

/// A title and a description: the answer's first line, and the rest.
fn title_and_body(answer: &str) -> (String, String) {
    let answer = clean(answer);
    let (title, body) = answer.split_once('\n').unwrap_or((answer.as_str(), ""));
    let title = title.trim().trim_start_matches('#').trim();
    let title = title
        .strip_prefix("Title:")
        .or_else(|| title.strip_prefix("タイトル:"))
        .unwrap_or(title);
    (title.trim().to_owned(), body.trim().to_owned())
}

/// Asks `sasacode` once: `instruction` as the prompt, `context` piped in after it.
async fn ask(
    program: PathBuf,
    cwd: PathBuf,
    model: Option<String>,
    instruction: String,
    context: String,
) -> Result<String, String> {
    let mut c = tokio::process::Command::new(&program);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c.args(["-p", &instruction, "--stdin", "--output", "text", "--no-session"])
        .args(["--permission", "ask", "--max-turns", "1"]);
    if let Some(model) = &model {
        c.args(["-m", model]);
    }
    let mut child = c
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(context.as_bytes()).await;
    }
    let out = tokio::time::timeout(WAIT, child.wait_with_output())
        .await
        .map_err(|_| "no answer in time".to_owned())?
        .map_err(|e| e.to_string())?;
    let answer = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !answer.is_empty() {
        return Ok(answer);
    }
    let err = String::from_utf8_lossy(&out.stderr);
    Err(err
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no answer")
        .trim()
        .to_owned())
}

impl App {
    fn language(&self) -> &'static str {
        if self.s.ja { "Japanese" } else { "English" }
    }

    /// Asks the model for `draft`, from what it is written from.
    fn draft(&mut self, draft: Draft) -> Task<Msg> {
        if self.drafting.contains(&draft) {
            return Task::none();
        }
        let (program, cwd) = (self.program.clone(), self.cwd.clone());
        let model = Some(self.model.trim().to_owned()).filter(|m| !m.is_empty());
        let language = self.language();
        let job: std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> = match draft {
            Draft::Commit => Box::pin(async move {
                let c = cwd.clone();
                let (log, change) = tokio::task::spawn_blocking(move || crate::git::commit_context(&c))
                    .await
                    .map_err(|e| e.to_string())??;
                let instruction = "Write a git commit message for the change piped in below. Follow the style and \
                     language of the repository's recent commit subjects listed first. Reply with the message only: \
                     a subject line, then, if the change needs it, a blank line and a short body. Do not use any \
                     tools."
                    .to_owned();
                let context = format!(
                    "Recent commit subjects:\n{log}\n\nThe change:\n{}",
                    head(&change, DIFF_MAX)
                );
                ask(program, cwd, model, instruction, context).await.map(|a| clean(&a))
            }),
            Draft::Pr => {
                let Some(pr) = &self.pr_draft else {
                    return Task::none();
                };
                let ctx = pr.context.clone();
                Box::pin(async move {
                    let commits: String = ctx
                        .commits
                        .iter()
                        .map(|(s, b)| {
                            if b.is_empty() {
                                format!("- {s}\n")
                            } else {
                                format!("- {s}\n  {}\n", b.replace('\n', "\n  "))
                            }
                        })
                        .collect();
                    let instruction = format!(
                        "Write the title and the description of a pull request for the branch piped in below (its \
                         commits, the files it changes, and its diff against {}). Write them in {language}. Reply \
                         with the title on the first line, then a blank line, then the description in Markdown: what \
                         changes and why, and how it was checked if the commits say. Nothing else. Do not use any tools.",
                        ctx.base
                    );
                    let context = format!(
                        "Commits:\n{commits}\nFiles:\n{}\n\nDiff:\n{}",
                        ctx.stat,
                        head(&ctx.diff, DIFF_MAX)
                    );
                    ask(program, cwd, model, instruction, context).await
                })
            }
            Draft::Next(key, _) => {
                let chat = if self.chat.key == key {
                    &self.chat
                } else {
                    return Task::none();
                };
                let Some(ex) = chat.transcript.exchanges.last() else {
                    return Task::none();
                };
                let reply: String = ex
                    .steps
                    .iter()
                    .filter_map(|s| match s {
                        Step::Text(md) => Some(md.raw.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let tail_start = reply.len().saturating_sub(REPLY_MAX);
                let tail_start = (tail_start..reply.len())
                    .find(|&i| reply.is_char_boundary(i))
                    .unwrap_or(0);
                let context = format!(
                    "The user said:\n{}\n\nThe agent answered:\n{}",
                    head(&ex.prompt, 2048),
                    &reply[tail_start..]
                );
                Box::pin(async move {
                    let instruction = "Below is the last exchange of a conversation between a user and a coding \
                         agent. Guess the user's most likely next message to the agent: short, one sentence, in the \
                         language the user wrote in. Reply with that message only. Do not use any tools."
                        .to_owned();
                    ask(program, cwd, model, instruction, context).await.map(|a| {
                        clean(&a)
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .trim()
                            .trim_matches('"')
                            .to_owned()
                    })
                })
            }
        };
        self.drafting.push(draft);
        Task::perform(job, move |answer| Msg::Drafted(draft, answer))
    }

    /// What the user may say next, guessed once a run of the shown conversation ended, if asked
    /// for in the settings.
    pub(crate) fn guess_next(&mut self, key: u64) -> Task<Msg> {
        if !self.prefs.suggest
            || key != self.chat.key
            || self.chat.busy()
            || !self.chat.editor.text().trim().is_empty()
            || self.chat.transcript.pending_approval().is_some()
        {
            return Task::none();
        }
        let at = self.chat.transcript.exchanges.len();
        if at == 0 {
            return Task::none();
        }
        self.draft(Draft::Next(key, at))
    }

    /// The helper's messages; any other comes back.
    pub(crate) fn update_helper(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let task = match msg {
            Msg::Draft(draft) => self.draft(draft),
            Msg::Drafted(draft, answer) => {
                self.drafting.retain(|d| *d != draft);
                match (draft, answer) {
                    (Draft::Commit, Ok(message)) => self.commit_msg = message,
                    (Draft::Pr, Ok(answer)) => {
                        if let Some(pr) = &mut self.pr_draft {
                            let (title, body) = title_and_body(&answer);
                            pr.title = title;
                            pr.body = text_editor::Content::with_text(&body);
                        }
                    }
                    (Draft::Next(key, at), Ok(next)) => {
                        // Still the conversation it was guessed for, as it was, with nothing typed.
                        if key == self.chat.key
                            && self.chat.transcript.exchanges.len() == at
                            && !self.chat.busy()
                            && self.chat.editor.text().trim().is_empty()
                            && !next.is_empty()
                        {
                            self.suggestion = Some((key, next));
                        }
                    }
                    (Draft::Next(..), Err(_)) => {}
                    (_, Err(error)) => {
                        self.toast = Some(crate::i18n::fill(self.s.w().draft_failed, "error", error));
                    }
                }
                Task::none()
            }
            Msg::PrOpen => {
                let cwd = self.cwd.clone();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || crate::git::pr_context(&cwd))
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|r| r)
                    },
                    |r| Msg::PrContext(r.map(Box::new)),
                )
            }
            Msg::PrContext(Ok(context)) => {
                let branch = self.git.branch.clone().unwrap_or_default();
                let (title, body) = context.filled(&branch);
                self.pr_draft = Some(PrDraft {
                    title,
                    body: text_editor::Content::with_text(&body),
                    context: *context,
                    draft: false,
                });
                self.overlay = Some(Overlay::Pr);
                iced::widget::operation::focus(crate::app::PR_TITLE)
            }
            Msg::PrContext(Err(error)) => {
                self.toast = Some(crate::i18n::fill(self.s.w().draft_failed, "error", error));
                Task::none()
            }
            Msg::PrTitle(title) => {
                if let Some(pr) = &mut self.pr_draft {
                    pr.title = title;
                }
                Task::none()
            }
            Msg::PrBody(action) => {
                if let Some(pr) = &mut self.pr_draft {
                    pr.body.perform(action);
                }
                Task::none()
            }
            Msg::PrCreate => {
                let Some(pr) = &self.pr_draft else {
                    return Ok(Task::none());
                };
                let (title, body) = (pr.title.trim().to_owned(), pr.body.text().trim().to_owned());
                let draft = pr.draft;
                if title.is_empty() {
                    return Ok(Task::none());
                }
                self.overlay = None;
                self.pr_draft = None;
                return Ok(self.update(Msg::GitDo(if draft {
                    Op::DraftPr(title, body)
                } else {
                    Op::Pr(Some((title, body)))
                })));
            }
            Msg::PrAsDraft(on) => {
                if let Some(pr) = &mut self.pr_draft {
                    pr.draft = on;
                }
                Task::none()
            }
            Msg::PrFill => {
                self.overlay = None;
                self.pr_draft = None;
                return Ok(self.update(Msg::GitDo(Op::Pr(None))));
            }
            Msg::TakeSuggestion => {
                let Some((key, next)) = self.suggestion.take() else {
                    return Ok(Task::none());
                };
                if key != self.chat.key {
                    return Ok(Task::none());
                }
                self.put_in_input(&next)
            }
            Msg::DropSuggestion => {
                self.suggestion = None;
                Task::none()
            }
            other => return self.update_scope(other),
        };
        Ok(task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_in_a_fence_is_taken_out_and_a_title_parted_from_its_body() {
        assert_eq!(clean("```\nfix: a\n\nbody\n```"), "fix: a\n\nbody");
        assert_eq!(clean("```text\nfix: a\n```\n"), "fix: a");
        assert_eq!(clean("  fix: a  "), "fix: a");
        assert_eq!(
            title_and_body("# 再試行を直す\n\n## 変更\n- a"),
            ("再試行を直す".to_owned(), "## 変更\n- a".to_owned())
        );
        assert_eq!(title_and_body("Title: x"), ("x".to_owned(), String::new()));
    }

    #[test]
    fn a_long_diff_is_cut_at_a_line() {
        let text = "一行目\n二行目\n三行目";
        assert_eq!(head(text, 100), text);
        assert_eq!(head(text, 20), "一行目\n二行目\n[… cut]");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_model_is_asked_once_with_nothing_saved_and_what_it_is_written_from_piped_in() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("sasacode-gui-helper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("sasacode");
        // Says its arguments and how much it was given on stdin.
        std::fs::write(&fake, "#!/bin/sh\nn=$(wc -c | tr -d ' ')\necho \"$* [$n]\"\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let said = ask(
            fake.clone(),
            dir.clone(),
            Some("p/m".into()),
            "write".into(),
            "差分".into(),
        )
        .await
        .unwrap();
        assert_eq!(
            said,
            "-p write --stdin --output text --no-session --permission ask --max-turns 1 -m p/m [6]"
        );
        // Nothing on stdout: what stderr last said.
        std::fs::write(&fake, "#!/bin/sh\necho 'no key' >&2\nexit 1\n").unwrap();
        assert_eq!(
            ask(fake, dir.clone(), None, "w".into(), String::new()).await,
            Err("no key".into())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_branchs_own_commit_fills_a_pull_request_as_gh_does() {
        let one = PrContext {
            commits: vec![("fix: a".into(), "why".into())],
            ..PrContext::default()
        };
        assert_eq!(one.filled("b"), ("fix: a".into(), "why".into()));
        let two = PrContext {
            commits: vec![("one".into(), String::new()), ("two".into(), String::new())],
            ..PrContext::default()
        };
        assert_eq!(two.filled("topic"), ("topic".into(), "- one\n- two".into()));
    }
}
