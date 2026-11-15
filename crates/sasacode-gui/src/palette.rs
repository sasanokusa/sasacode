//! The command list (Ctrl/⌘+K): every action, conversation and setting of the window, found by
//! typing. What it offers is decided here; how it looks is in `view/find.rs`.

use std::path::PathBuf;

use crate::app::{App, EFFORTS, PERMISSIONS};
use crate::icon;

/// What the list is listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Actions and conversations.
    Root,
    Model,
    Permission,
    Effort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    New,
    Stop,
    Sidebar,
    Changes,
    CopyAnswer,
    /// Show another list.
    Go(Mode),
    Open(PathBuf),
    /// `None` is sasacode's own default.
    Model(Option<String>),
    Permission(String),
    Effort(Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Entries of one group are listed under its name; empty for a list with one kind of thing.
    pub group: &'static str,
    pub icon: char,
    pub label: String,
    /// A sentence under the label.
    pub note: &'static str,
    /// At the row's end: the current value, a key, or how long ago.
    pub value: String,
    /// The current choice of a setting.
    pub chosen: bool,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub mode: Mode,
    pub query: String,
    /// The entry Enter runs.
    pub cursor: usize,
}

impl Palette {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            query: String::new(),
            cursor: 0,
        }
    }
}

/// Whether every word of `query` is somewhere in the entry. Case is ignored.
fn matches(entry: &Entry, query: &str) -> bool {
    let hay = format!("{} {} {}", entry.label, entry.value, entry.note).to_lowercase();
    query.to_lowercase().split_whitespace().all(|word| hay.contains(word))
}

impl App {
    /// The entries the list shows for what is typed, in order.
    pub(crate) fn entries(&self, palette: &Palette) -> Vec<Entry> {
        let s = &self.s;
        let query = palette.query.trim();
        let entry = |group, icon, label: String, command| Entry {
            group,
            icon,
            label,
            note: "",
            value: String::new(),
            chosen: false,
            command,
        };
        let mut all = Vec::new();
        match palette.mode {
            Mode::Root => {
                let act = s.group_actions;
                if self.running() {
                    all.push(Entry {
                        value: s.key("."),
                        ..entry(act, icon::SQUARE, s.do_stop.into(), Command::Stop)
                    });
                } else {
                    all.push(Entry {
                        value: s.key("N"),
                        ..entry(act, icon::SQUARE_PEN, s.do_new.into(), Command::New)
                    });
                }
                all.push(Entry {
                    value: self.model_label(),
                    ..entry(act, icon::BOX, s.do_model.into(), Command::Go(Mode::Model))
                });
                all.push(Entry {
                    value: s.permission(self.permission()),
                    ..entry(act, icon::SHIELD, s.do_permission.into(), Command::Go(Mode::Permission))
                });
                all.push(Entry {
                    value: self.effort_label(),
                    ..entry(act, icon::GAUGE, s.do_effort.into(), Command::Go(Mode::Effort))
                });
                let changes = if self.changes { s.hide_changes } else { s.show_changes };
                all.push(Entry {
                    value: s.key("D"),
                    ..entry(act, icon::FILE_DIFF, changes.into(), Command::Changes)
                });
                // Offered only where the window has room for the list.
                if self.list_fits() {
                    let sidebar = if self.sidebar { s.hide_sidebar } else { s.show_sidebar };
                    all.push(Entry {
                        value: s.key("B"),
                        ..entry(act, icon::PANEL_LEFT, sidebar.into(), Command::Sidebar)
                    });
                }
                if self.last_answer().is_some() {
                    all.push(entry(act, icon::COPY, s.do_copy.into(), Command::CopyAnswer));
                }
                // A run belongs to its conversation: another one is opened once it has ended.
                if !self.running() {
                    for session in &self.sessions {
                        let title = if session.first_prompt.is_empty() {
                            s.untitled
                        } else {
                            session.first_prompt.lines().next().unwrap_or("")
                        };
                        all.push(Entry {
                            value: s.ago(session.modified, self.clock),
                            chosen: self.session.as_deref() == Some(session.id.as_str()),
                            ..entry(
                                s.group_sessions,
                                icon::MESSAGE_SQUARE,
                                title.into(),
                                Command::Open(session.path.clone()),
                            )
                        });
                    }
                }
            }
            Mode::Model => {
                all.push(Entry {
                    note: s.model_default_note,
                    // Which model that is now, where the window can tell.
                    value: self.unchosen_model().unwrap_or("").to_owned(),
                    chosen: self.prefs.model.is_none(),
                    ..entry("", icon::BOX, s.model_default.into(), Command::Model(None))
                });
                for model in self.known_models() {
                    all.push(Entry {
                        chosen: self.prefs.model.as_deref() == Some(model.as_str()),
                        ..entry("", icon::BOX, model.clone(), Command::Model(Some(model)))
                    });
                }
            }
            Mode::Permission => {
                for mode in PERMISSIONS {
                    all.push(Entry {
                        note: s.permission_note(mode),
                        chosen: self.permission() == mode,
                        ..entry(
                            "",
                            permission_icon(mode),
                            s.permission(mode),
                            Command::Permission(mode.into()),
                        )
                    });
                }
            }
            Mode::Effort => {
                all.push(Entry {
                    note: s.effort_default_note,
                    chosen: self.prefs.effort.is_none(),
                    ..entry("", icon::GAUGE, s.effort_default.into(), Command::Effort(None))
                });
                for level in EFFORTS {
                    all.push(Entry {
                        chosen: self.prefs.effort.as_deref() == Some(level),
                        ..entry("", icon::GAUGE, s.effort(level), Command::Effort(Some(level.into())))
                    });
                }
            }
        }
        all.retain(|e| matches(e, query));
        // A model the window has not seen is used by typing its name.
        if palette.mode == Mode::Model
            && !query.is_empty()
            && !all.iter().any(|e| e.command == Command::Model(Some(query.to_owned())))
        {
            all.push(entry(
                "",
                icon::CORNER_DOWN_LEFT,
                s.use_model(query),
                Command::Model(Some(query.to_owned())),
            ));
        }
        all
    }
}

pub fn permission_icon(mode: &str) -> char {
    match mode {
        "ask" => icon::HAND,
        "agent" => icon::SHIELD_CHECK,
        "auto" => icon::SHIELD_ALERT,
        _ => icon::SHIELD,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_matches_when_every_typed_word_is_in_it_whatever_the_case() {
        let entry = Entry {
            group: "",
            icon: icon::BOX,
            label: "PGX/Qwen3.8-Flash-Next".into(),
            note: "",
            value: "3時間前".into(),
            chosen: false,
            command: Command::New,
        };
        assert!(matches(&entry, ""));
        assert!(matches(&entry, "qwen flash"));
        assert!(matches(&entry, "3時間"));
        assert!(!matches(&entry, "qwen opus"));
    }
}
