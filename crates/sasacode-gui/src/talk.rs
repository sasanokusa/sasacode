//! What is done in the conversation shown: branching it off after any reply, stepping through what
//! its search finds, which steps are shown, and files named with their line in what a program
//! wrote.

use iced::Task;
use iced::widget::{operation, text_editor};
use sasacode_protocol::ControlMessage;

use crate::app::{App, Msg, Overlay};
use crate::i18n::fill;

impl App {
    /// The exchanges the search in the conversation finds, by index.
    pub(crate) fn found_in(&self) -> Vec<usize> {
        let Some(needle) = self
            .find_in
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_lowercase)
        else {
            return Vec::new();
        };
        self.chat
            .transcript
            .exchanges
            .iter()
            .enumerate()
            .filter(|(_, ex)| crate::view::has(ex, &needle))
            .map(|(i, _)| i)
            .collect()
    }

    /// The exchange in view of those the search finds.
    pub(crate) fn find_current(&self) -> Option<usize> {
        let found = self.found_in();
        found.get(self.find_at.min(found.len().saturating_sub(1))).copied()
    }

    /// The branch the conversation shown was started on, when HEAD is now another one and the
    /// notice of it was not put away: then, and now.
    pub(crate) fn branch_moved(&self) -> Option<(&str, &str)> {
        let id = self.chat.session.as_deref()?;
        let was = self.prefs.branches.get(id)?;
        let now = self.git.branch.as_deref()?;
        (was != now && !self.branch_seen.contains(id)).then_some((was.as_str(), now))
    }

    /// Another conversation as context: written as Markdown under the sasacode folder, and named
    /// in the input with `@`, as a file is.
    fn attach_conversation(&mut self, id: &str) -> Task<Msg> {
        let w = self.s.w();
        let Some(session) = self.sessions.iter().find(|s| s.id == id) else {
            return Task::none();
        };
        let entries = match sasacode_protocol::read_entries(&session.path) {
            Ok(entries) => entries,
            Err(e) => {
                self.toast = Some(format!("{}: {e}", session.path.display()));
                return Task::none();
            }
        };
        let transcript = crate::transcript::Transcript::from_entries(&entries);
        let name = self.prefs.names.get(id).map(String::as_str);
        let text = crate::export::markdown(name, &transcript, self.s);
        let dir = self.home.clone().unwrap_or_else(std::env::temp_dir).join("context");
        let path = dir.join(format!("{id}.md"));
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, text)) {
            self.toast = Some(format!("{}: {e}", path.display()));
            return Task::none();
        }
        self.toast = Some(w.attached_conversation.to_owned());
        self.dropped(&path)
    }

    /// A new conversation holding the one shown through exchange `i`'s reply, shown with nothing
    /// typed: before the next message, or all of it after the last.
    fn fork_after(&mut self, i: usize) -> Task<Msg> {
        if self.busy() {
            return Task::none();
        }
        let Some(log) = self
            .chat
            .session
            .as_ref()
            .and_then(|id| self.sessions.iter().find(|s| s.id == *id))
            .map(|s| s.path.clone())
        else {
            return Task::none();
        };
        let made = match self.chat.transcript.exchanges.get(i + 1) {
            Some(next) => crate::fork::fork(&log, i + 1, &next.prompt.clone()),
            None => crate::fork::copy_whole(&log),
        };
        match made {
            Ok((_, path)) => {
                self.reload_sessions();
                if self.open_session(&path) {
                    self.toast = Some(self.s.w().forked.to_owned());
                }
                operation::focus(crate::app::INPUT)
            }
            Err(error) => {
                self.toast = Some(fill(self.s.fork_failed, "error", error));
                Task::none()
            }
        }
    }

    /// The conversation's messages; any other comes back.
    pub(crate) fn update_talk(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let task = match msg {
            Msg::ForkAfter(i) => self.fork_after(i),
            Msg::FindStep(step) => {
                let found = self.found_in();
                if found.is_empty() {
                    return Ok(Task::none());
                }
                let n = found.len() as isize;
                self.find_at = (self.find_at.min(found.len() - 1) as isize + step as isize).rem_euclid(n) as usize;
                self.locate(found[self.find_at])
            }
            Msg::OpenRef(uri) => match crate::files::linked_file(&uri, &self.cwd) {
                Some((path, line)) => return Ok(self.update(Msg::ViewFile(path, line))),
                None => {
                    self.toast = Some(fill(self.s.w().ref_missing, "path", uri));
                    Task::none()
                }
            },
            Msg::FollowEdit(i) => {
                if i < self.chat.follow.len() {
                    let text = self.chat.follow.remove(i);
                    let typed = self.chat.editor.text();
                    let typed = typed.trim();
                    self.set_input(&if typed.is_empty() {
                        text
                    } else {
                        format!("{text}\n\n{typed}")
                    });
                }
                operation::focus(crate::app::INPUT)
            }
            Msg::FollowNow(i) => {
                if i >= self.chat.follow.len() {
                    return Ok(Task::none());
                }
                let text = self.chat.follow.remove(i);
                if self.running() {
                    // To the run, at the model's next turn, as one sent while it works is.
                    self.chat.transcript.queue(&text);
                    self.control(ControlMessage::Message { text });
                    self.back_to_newest()
                } else if self.busy() {
                    self.chat.next = Some(text);
                    Task::none()
                } else {
                    self.start(self.chat.key, text)
                }
            }
            Msg::FollowUp(i) => {
                if i > 0 && i < self.chat.follow.len() {
                    self.chat.follow.swap(i, i - 1);
                }
                Task::none()
            }
            Msg::FollowDrop(i) => {
                if i < self.chat.follow.len() {
                    self.chat.follow.remove(i);
                }
                Task::none()
            }
            Msg::SetQueue(on) => self.set(|p| p.more.queue = on),
            Msg::AttachConversation(id) => self.attach_conversation(&id),
            Msg::QuoteOpen(i) => {
                let Some(raw) = self.copy_text(&crate::app::Clip::Reply(i)) else {
                    return Ok(Task::none());
                };
                self.quoting = Some((i, text_editor::Content::with_text(&raw)));
                self.overlay = Some(Overlay::Quote);
                operation::focus(QUOTE)
            }
            Msg::QuoteEdit(action) => {
                // Read only: the text is chosen, never changed.
                if let Some((_, content)) = &mut self.quoting
                    && !action.is_edit()
                {
                    content.perform(action);
                }
                Task::none()
            }
            Msg::QuoteTake => {
                let chosen = self.quoting.as_ref().and_then(|(_, c)| c.selection());
                let Some(chosen) = chosen.filter(|c| !c.trim().is_empty()) else {
                    self.toast = Some(self.s.w().quote_none.to_owned());
                    return Ok(Task::none());
                };
                self.quoting = None;
                self.overlay = None;
                let quoted: String = chosen.trim_end().lines().map(|l| format!("> {l}\n")).collect();
                let typed = self.chat.editor.text();
                let typed = typed.trim_end();
                self.set_input(&if typed.is_empty() {
                    format!("{quoted}\n")
                } else {
                    format!("{typed}\n\n{quoted}\n")
                });
                operation::focus(crate::app::INPUT)
            }
            Msg::BackToBranch(branch) => return Ok(self.update(Msg::GitDo(crate::git::Op::Switch(branch)))),
            Msg::BranchSeen(id) => {
                self.branch_seen.insert(id);
                Task::none()
            }
            Msg::SetFailedOnly(on) => self.set(|p| p.more.failed_only = on),
            Msg::SetHideThinking(hide) => self.set(|p| p.more.hide_thinking = hide),
            other => return self.update_plugins(other),
        };
        Ok(task)
    }
}

/// The read-only text of a reply being chosen to quote.
pub const QUOTE: &str = "quote";

/// Files named with their line in a program's output: where each is in `line`, and the name as a
/// link takes it, `path:line`. Only a name with a line and a dot or slash in it, as compilers and
/// test runners write them; whether the file is there is asked when it is clicked.
pub fn refs(line: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let token = |c: char| c.is_alphanumeric() || "._-/~+@:".contains(c);
    let mut out = Vec::new();
    let mut at = 0;
    while at < line.len() {
        let rest = &line[at..];
        let Some(start) = rest.find(token) else {
            break;
        };
        let start = at + start;
        let len = line[start..].find(|c: char| !token(c)).unwrap_or(line.len() - start);
        let word = &line[start..start + len];
        at = start + len.max(1);
        // `path:12`, `path:12:5`, `path:12:` as rustc, tsc and pytest write them.
        let word = word.trim_end_matches(':');
        let mut parts = word.splitn(3, ':');
        let (Some(path), Some(n)) = (parts.next(), parts.next()) else {
            continue;
        };
        let named = (path.contains('.') || path.contains('/'))
            && !path.contains("//")
            && path.chars().any(char::is_alphabetic)
            && !path.ends_with('.');
        if named && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
            let end = start + path.len() + 1 + n.len();
            out.push((start..end, format!("{path}:{n}")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_named_with_its_line_is_found_in_what_a_compiler_wrote() {
        let line = "  --> crates/a/src/lib.rs:12:5 and tests/x.test.ts:7, not http://h:80 or 10:30";
        let found: Vec<String> = refs(line).into_iter().map(|(_, r)| r).collect();
        assert_eq!(found, vec!["crates/a/src/lib.rs:12", "tests/x.test.ts:7"]);
        let (range, _) = &refs(line)[0];
        assert_eq!(&line[range.clone()], "crates/a/src/lib.rs:12");
        assert!(refs("ok: 3 passed").is_empty());
        assert!(refs("Error: foo.rs: no line").is_empty());
        let (range, r) = &refs("FAILED src/日本.py:3: assert")[0];
        assert_eq!((r.as_str(), range.start), ("src/日本.py:3", 7));
    }
}
