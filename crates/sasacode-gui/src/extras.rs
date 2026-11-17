//! Smaller things the window does: asking before it closes on a run or turns on `auto`, marking a
//! conversation unread, writing it to a file, going on with it in the terminal, how wide it is
//! drawn and in which language.

use iced::{Task, window};

use crate::app::{App, Msg, Overlay};
use crate::i18n::{Strings, fill};
use crate::layout::Width;

impl App {
    /// Runs that would stop if the window closed: the agent's, and the folder's scripts.
    pub(crate) fn runs_going(&self) -> usize {
        let chats = std::iter::once(&self.chat)
            .chain(&self.parked)
            .chain(self.aside.as_ref())
            .filter(|c| c.busy())
            .count();
        chats + self.script_runs.iter().filter(|r| r.running()).count()
    }

    /// Writes the conversation as Markdown to `path`, from the folder when it is not absolute.
    fn export_to(&mut self, path: &str) {
        let w = self.s.w();
        let path = match path.strip_prefix("~/") {
            Some(rest) => std::env::home_dir().map_or_else(|| self.cwd.join(path), |home| home.join(rest)),
            None => self.cwd.join(path),
        };
        let text = crate::export::markdown(self.name(), &self.chat.transcript, self.s);
        let done = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, text));
        self.toast = Some(match done {
            Ok(()) => fill(w.exported, "path", path.display()),
            Err(e) => fill(w.export_failed, "error", e),
        });
    }

    /// The window's words in `lang`, or the system's.
    pub(crate) fn strings_for(lang: Option<&str>) -> Strings {
        match lang {
            Some(lang) => Strings::get(lang),
            None => Strings::detect(),
        }
    }

    /// The extras' messages; any other comes back.
    pub(crate) fn update_extras(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let task = match msg {
            Msg::CloseAsked(id) => {
                if self.runs_going() == 0 {
                    return Ok(window::close(id));
                }
                self.overlay = Some(Overlay::Quit(id));
                Task::none()
            }
            Msg::Quit(id) => window::close(id),
            Msg::AutoAgree => {
                let _ = self.set(|p| p.more.auto_agreed = true);
                self.overlay = None;
                self.permission = Some("auto");
                Task::none()
            }
            Msg::MarkUnread(id) => {
                let unread = !self.prefs.more.unread.contains(&id) && !self.chat_of(&id).is_some_and(|c| c.unread);
                if let Some(chat) = std::iter::once(&mut self.chat)
                    .chain(self.parked.iter_mut())
                    .find(|c| c.session.as_deref() == Some(id.as_str()))
                {
                    chat.unread = unread;
                }
                let _ = self.set(|p| {
                    if unread {
                        p.more.unread.insert(id);
                    } else {
                        p.more.unread.remove(&id);
                    }
                });
                Task::none()
            }
            Msg::ExportTo(path) => {
                self.export_to(&path);
                Task::none()
            }
            Msg::SetWidth(width) => {
                let _ = self.set(|p| p.more.chat_width = width);
                Task::none()
            }
            Msg::Whole(i) => {
                if !self.chat.whole.remove(&i) {
                    self.chat.whole.insert(i);
                }
                Task::none()
            }
            Msg::SetLang(lang) => {
                let _ = self.set(|p| p.more.lang = lang.map(str::to_owned));
                self.s = App::strings_for(lang);
                Task::none()
            }
            other => return self.update_rows(other),
        };
        Ok(task)
    }
}

/// How wide the conversation and the input are drawn at most.
pub fn column_width(width: Width) -> f32 {
    match width {
        Width::Normal => 760.0,
        Width::Wide => 1040.0,
        Width::Full => f32::INFINITY,
    }
}

/// A message longer than this is folded until it is opened.
pub const LONG_LINES: usize = 14;
const LONG_BYTES: usize = 1600;

/// The start of a long message, to show folded: none when it is short.
pub fn folded(prompt: &str) -> Option<String> {
    let lines = prompt.lines().count();
    if lines <= LONG_LINES && prompt.len() <= LONG_BYTES {
        return None;
    }
    let mut head: String = prompt.lines().take(LONG_LINES - 2).collect::<Vec<_>>().join("\n");
    if head.len() > LONG_BYTES {
        let mut end = LONG_BYTES;
        while !head.is_char_boundary(end) {
            end -= 1;
        }
        head.truncate(end);
    }
    head.push_str(" …");
    Some(head)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_message_is_folded_to_its_start_and_a_short_one_is_not() {
        assert_eq!(folded("短い"), None);
        let long = (1..=30).map(|n| format!("{n}行目")).collect::<Vec<_>>().join("\n");
        let head = folded(&long).unwrap();
        assert!(head.starts_with("1行目\n") && head.ends_with("12行目 …"));
        let wide = "あ".repeat(2000);
        assert!(folded(&wide).unwrap().len() <= LONG_BYTES + 4);
    }
}
