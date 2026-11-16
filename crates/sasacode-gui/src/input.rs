//! The input as it is typed in: what it offers after `@` and `/`, what was sent before under ↑,
//! files dropped on the window, and the window's own commands that `/` and the palette run.

use std::sync::Arc;

use iced::Task;
use iced::widget::operation;
use iced::widget::text_editor::{self, Action, Edit};

use crate::app::{App, FIND_IN, MODEL, Msg, Overlay};
use crate::composer::{self, SHOWN};
use crate::i18n::Strings;
use crate::system;

/// A command of the window: one that `/` offers in the input, that the palette lists, or that a
/// key does. None of them reaches the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    New,
    Btw,
    Find,
    Export,
    Steps,
    Model,
    Rename,
    Pin,
    Archive,
    Archived,
    Window,
    Folder,
    Terminal,
    Editor,
    Worktree,
    Settings,
    Keys,
    Attention,
    Mention,
    Branch,
}

impl Command {
    /// What `/` offers, by the word typed after it.
    pub const SLASH: &'static [(&'static str, Command)] = &[
        ("new", Command::New),
        ("btw", Command::Btw),
        ("find", Command::Find),
        ("export", Command::Export),
        ("steps", Command::Steps),
        ("model", Command::Model),
        ("rename", Command::Rename),
        ("pin", Command::Pin),
        ("archive", Command::Archive),
        ("window", Command::Window),
        ("folder", Command::Folder),
        ("terminal", Command::Terminal),
        ("editor", Command::Editor),
        ("worktree", Command::Worktree),
        ("settings", Command::Settings),
        ("keys", Command::Keys),
        ("branch", Command::Branch),
    ];

    pub fn what(self, s: &Strings) -> &'static str {
        match self {
            Command::New => s.new_session,
            Command::Btw => s.c_btw,
            Command::Find => s.c_find,
            Command::Export => s.c_export,
            Command::Steps => s.c_steps,
            Command::Model => s.c_model,
            Command::Rename => s.c_rename,
            Command::Pin => s.c_pin,
            Command::Archive => s.c_archive,
            Command::Archived => s.c_archived,
            Command::Window => s.c_window,
            Command::Folder => s.c_folder,
            Command::Terminal => s.c_terminal,
            Command::Editor => s.c_editor,
            Command::Worktree => s.c_worktree,
            Command::Settings => s.settings,
            Command::Keys => s.c_keys,
            Command::Attention => s.c_attention,
            Command::Mention => s.c_mention,
            Command::Branch => s.w().branch_switch,
        }
    }

    /// Takes what is typed after it: `/btw` and its question.
    fn takes_text(self) -> bool {
        self == Command::Btw
    }
}

/// What the input offers under the cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    /// The line and the bytes of it that taking an entry replaces.
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub kind: OfferKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OfferKind {
    /// Files and folders after `@`, best first.
    Files(Vec<String>),
    /// The window's commands after `/`.
    Commands(Vec<(&'static str, Command)>),
}

impl Offer {
    pub fn len(&self) -> usize {
        match &self.kind {
            OfferKind::Files(files) => files.len(),
            OfferKind::Commands(commands) => commands.len(),
        }
    }
}

impl App {
    /// What the input offers where its cursor is: files after an `@` that starts a word, the
    /// window's commands after a `/` that starts the message. Nothing once put away with Esc, or
    /// while text is selected.
    pub(crate) fn offer(&self) -> Option<Offer> {
        if self.dismissed {
            return None;
        }
        let cursor = self.chat.editor.cursor();
        if cursor.selection.is_some() {
            return None;
        }
        let at = cursor.position;
        let line = self.chat.editor.line(at.line)?;
        let before = line.text.get(..at.column)?;
        if let Some(word) = composer::slash(at.line, before) {
            let commands: Vec<_> = Command::SLASH
                .iter()
                .copied()
                .filter(|(name, _)| name.starts_with(&word.to_lowercase()))
                .collect();
            return (!commands.is_empty()).then_some(Offer {
                line: at.line,
                start: 0,
                end: at.column,
                kind: OfferKind::Commands(commands),
            });
        }
        let (start, query) = composer::mention(before)?;
        let files = composer::rank(&self.files, query, SHOWN)
            .into_iter()
            .map(str::to_owned)
            .collect();
        Some(Offer {
            line: at.line,
            start,
            end: at.column,
            kind: OfferKind::Files(files),
        })
    }

    /// An action on the input. Typing ends going through what was sent before, and brings back
    /// what was put away; the first `@` asks for the project's files.
    pub(crate) fn edit(&mut self, action: Action) -> Task<Msg> {
        let edits = action.is_edit();
        self.chat.editor.perform(action);
        if edits {
            self.recall = None;
            self.dismissed = false;
            self.pick = 0;
        }
        self.ask_files()
    }

    /// The project's files, listed once an `@` asks for them; again after a run, which may have
    /// made or removed some.
    fn ask_files(&mut self) -> Task<Msg> {
        if self.files_asked
            || !matches!(
                self.offer(),
                Some(Offer {
                    kind: OfferKind::Files(_),
                    ..
                })
            )
        {
            return Task::none();
        }
        self.files_asked = true;
        let cwd = self.cwd.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || composer::project_files(&cwd))
                    .await
                    .unwrap_or_default()
            },
            Msg::Files,
        )
    }

    /// Replaces the bytes `start..end` of `line` of the input with `with`, the cursor after it.
    fn replace(&mut self, line: usize, start: usize, end: usize, with: &str) {
        let text = self.chat.editor.text();
        let offset: usize = text.split('\n').take(line).map(|l| l.len() + 1).sum();
        let (from, to) = (offset + start, offset + end);
        if to > text.len() || !text.is_char_boundary(from) || !text.is_char_boundary(to) {
            return;
        }
        let changed = format!("{}{with}{}", &text[..from], &text[to..]);
        self.chat.editor = text_editor::Content::with_text(&changed);
        self.chat.editor.move_to(text_editor::Cursor {
            position: text_editor::Position {
                line,
                column: start + with.len(),
            },
            selection: None,
        });
    }

    /// Puts `text` in place of what the input has.
    fn set_input(&mut self, text: &str) {
        self.chat.editor = text_editor::Content::with_text(text);
        self.chat.editor.perform(Action::Move(text_editor::Motion::DocumentEnd));
    }

    /// Takes the entry `which` of what the input offers, or the chosen one: a file goes in as
    /// `@path`, a command runs, or waits for its text.
    pub(crate) fn complete(&mut self, which: Option<usize>) -> Task<Msg> {
        let Some(offer) = self.offer() else {
            return Task::none();
        };
        let at = which.unwrap_or(self.pick).min(offer.len().saturating_sub(1));
        self.pick = 0;
        match offer.kind {
            OfferKind::Files(files) => {
                let Some(file) = files.get(at) else {
                    return Task::none();
                };
                let mut with = composer::at_ref(file);
                // A folder is gone into: what is in it is offered next.
                if !file.ends_with('/') {
                    with.push(' ');
                }
                self.replace(offer.line, offer.start, offer.end, &with);
                self.ask_files()
            }
            OfferKind::Commands(commands) => {
                let Some(&(name, command)) = commands.get(at) else {
                    return Task::none();
                };
                if command.takes_text() {
                    self.replace(offer.line, offer.start, offer.end, &format!("/{name} "));
                    return Task::none();
                }
                // The `/word` goes; the command runs.
                self.replace(offer.line, offer.start, offer.end, "");
                self.command(command)
            }
        }
    }

    /// A message that is one of the window's commands: it is run instead of being sent. Only a
    /// command named whole, alone or with its text.
    pub(crate) fn as_command(&mut self) -> Option<Task<Msg>> {
        let text = self.chat.editor.text();
        let word = text.trim().strip_prefix('/')?;
        let (name, rest) = word.split_once(char::is_whitespace).unwrap_or((word, ""));
        let &(_, command) = Command::SLASH.iter().find(|(n, _)| *n == name)?;
        if !rest.trim().is_empty() && !command.takes_text() {
            return None;
        }
        let rest = rest.trim().to_owned();
        if command == Command::Btw && rest.is_empty() {
            return Some(Task::none());
        }
        self.set_input("");
        if command == Command::Btw {
            return Some(self.side_question(rest));
        }
        Some(self.command(command))
    }

    /// ↑ goes back through what was sent before, ↓ forward to what was being typed.
    pub(crate) fn recall(&mut self, step: i8) -> Task<Msg> {
        if self.history.is_empty() {
            return Task::none();
        }
        let at = match (self.recall, step < 0) {
            (None, true) => {
                self.recall_draft = self.chat.editor.text();
                Some(self.history.len() - 1)
            }
            (None, false) => return Task::none(),
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) => (i + 1 < self.history.len()).then_some(i + 1),
        };
        self.recall = at;
        let text = match at {
            Some(i) => self.history[i].clone(),
            None => std::mem::take(&mut self.recall_draft),
        };
        self.set_input(&text);
        self.dismissed = true;
        Task::none()
    }

    /// A file dropped on the window goes into the input where its cursor is, as `@path`.
    pub(crate) fn dropped(&mut self, path: &std::path::Path) -> Task<Msg> {
        self.dropping = false;
        let cursor = self.chat.editor.cursor().position;
        let before = self
            .chat
            .editor
            .line(cursor.line)
            .and_then(|l| l.text.get(..cursor.column).map(str::to_owned))
            .unwrap_or_default();
        let lead = if before.is_empty() || before.ends_with(char::is_whitespace) {
            ""
        } else {
            " "
        };
        let text = format!("{lead}{} ", composer::path_ref(path, &self.cwd));
        self.chat.editor.perform(Action::Edit(Edit::Paste(Arc::new(text))));
        self.dismissed = true;
        operation::focus(crate::app::INPUT)
    }

    /// Runs a command of the window.
    pub(crate) fn command(&mut self, command: Command) -> Task<Msg> {
        let session = self.chat.session.clone();
        match command {
            Command::New => return self.update(Msg::NewSession),
            Command::Btw => self.set_input("/btw "),
            Command::Find => {
                self.find_in.get_or_insert_with(String::new);
                self.find_held = true;
                return Task::batch([operation::focus(FIND_IN), operation::select_all(FIND_IN)]);
            }
            Command::Export => return self.update(Msg::Copy(crate::app::Clip::Conversation)),
            Command::Steps => return self.update(Msg::SetSteps(!self.prefs.steps)),
            Command::Model => return operation::focus(MODEL),
            Command::Rename | Command::Pin | Command::Archive => {
                let Some(id) = session else {
                    return Task::none();
                };
                return self.update(match command {
                    Command::Rename => Msg::Rename(id),
                    Command::Pin => Msg::Pin(id),
                    _ => Msg::Archive(id),
                });
            }
            Command::Archived => {
                self.show_archived = !self.show_archived;
                self.side = true;
            }
            Command::Window => return self.new_window(self.cwd.clone(), session),
            Command::Folder => {
                system::reveal(&self.cwd);
            }
            Command::Terminal => {
                system::terminal(&self.cwd);
            }
            Command::Editor => {
                system::editor(&self.cwd);
            }
            Command::Worktree => return self.worktree(),
            Command::Settings => self.overlay = Some(Overlay::Settings),
            Command::Keys => self.overlay = Some(Overlay::Keys),
            Command::Attention => return self.attention(),
            Command::Branch => {
                return self.update(Msg::Show(Some(Overlay::picker(crate::overlay::Pick::Branches))));
            }
            Command::Mention => {
                // An `@` where the cursor is, a word of its own: the files are offered.
                let cursor = self.chat.editor.cursor().position;
                let before = self
                    .chat
                    .editor
                    .line(cursor.line)
                    .and_then(|l| l.text.get(..cursor.column).map(str::to_owned))
                    .unwrap_or_default();
                let lead = if before.is_empty() || before.ends_with(char::is_whitespace) {
                    "@"
                } else {
                    " @"
                };
                self.chat
                    .editor
                    .perform(Action::Edit(Edit::Paste(Arc::new(lead.to_owned()))));
                self.dismissed = false;
                self.pick = 0;
                return Task::batch([operation::focus(crate::app::INPUT), self.ask_files()]);
            }
        }
        Task::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Options;
    use crate::layout::{Layout, Prefs};

    fn window() -> App {
        let options = Options {
            program: "sasacode".into(),
            cwd: "/work/p".into(),
            session: None,
            layout: Layout::default(),
            prefs: Prefs::default(),
            layout_file: None,
        };
        let mut app = App::blank(options, Strings::get("ja"));
        app.files = ["src/app.rs", "src/view/", "src/view/dock.rs", "README.md"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        app.files_asked = true;
        app
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            let _ = app.update(Msg::Edit(Action::Edit(if c == '\n' {
                Edit::Enter
            } else {
                Edit::Insert(c)
            })));
        }
    }

    #[test]
    fn an_at_sign_offers_files_and_takes_one() {
        let mut app = window();
        typed(&mut app, "これを見て @dock");
        let offer = app.offer().unwrap();
        assert_eq!(offer.kind, OfferKind::Files(vec!["src/view/dock.rs".into()]));
        let _ = app.update(Msg::Complete(None));
        assert_eq!(app.chat.editor.text(), "これを見て @src/view/dock.rs ");
        assert!(app.offer().is_none());

        // A folder is gone into; Esc puts the offer away until something is typed.
        typed(&mut app, "と @vi");
        let _ = app.update(Msg::Complete(None));
        assert!(app.chat.editor.text().ends_with("@src/view/"));
        assert_eq!(app.offer().unwrap().len(), 2);
        let _ = app.update(Msg::Pick(1));
        let _ = app.update(Msg::Dismiss);
        assert!(app.offer().is_none());
        typed(&mut app, "d");
        assert_eq!(
            app.offer().unwrap().kind,
            OfferKind::Files(vec!["src/view/dock.rs".into()])
        );
    }

    #[test]
    fn a_slash_offers_the_windows_commands_which_are_not_sent() {
        let mut app = window();
        typed(&mut app, "/ste");
        let Some(Offer {
            kind: OfferKind::Commands(found),
            ..
        }) = app.offer()
        else {
            panic!("no commands offered");
        };
        assert_eq!(found, vec![("steps", Command::Steps)]);
        let _ = app.update(Msg::Complete(None));
        assert!(!app.prefs.steps && app.chat.editor.text().is_empty());

        // Typed out and sent, it is run too; a word that is no command is a message.
        typed(&mut app, "/steps");
        let _ = app.update(Msg::Send);
        assert!(app.prefs.steps && app.chat.transcript.is_empty());
        typed(&mut app, "/btw ");
        assert!(app.offer().is_none());
    }

    #[test]
    fn up_goes_back_through_what_was_sent() {
        let mut app = window();
        app.history = vec!["一つ目".into(), "二つ目".into()];
        typed(&mut app, "書きかけ");
        let _ = app.update(Msg::Recall(-1));
        assert_eq!(app.chat.editor.text(), "二つ目");
        let _ = app.update(Msg::Recall(-1));
        let _ = app.update(Msg::Recall(-1));
        assert_eq!(app.chat.editor.text(), "一つ目");
        let _ = app.update(Msg::Recall(1));
        let _ = app.update(Msg::Recall(1));
        assert_eq!(app.chat.editor.text(), "書きかけ");
        assert_eq!(app.recall, None);
    }

    #[test]
    fn a_dropped_file_goes_in_where_the_cursor_is() {
        let mut app = window();
        typed(&mut app, "見て");
        let _ = app.update(Msg::Dropped("/work/p/src/app.rs".into()));
        let _ = app.update(Msg::Dropped("/Users/me/Desktop/shot 1.png".into()));
        assert_eq!(
            app.chat.editor.text(),
            "見て @src/app.rs @\"/Users/me/Desktop/shot 1.png\" "
        );
    }
}
