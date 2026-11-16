//! What the list of conversations does besides opening one: its groups, which the user names and
//! folds, and deleting a conversation's log for good.

use iced::Task;
use iced::widget::operation;
use sasacode_protocol::SessionSummary;

use crate::app::{App, GROUP, Msg, Overlay};

/// A section of the list: the conversations shown in it, and how many its fold hides.
pub struct Listed<'a> {
    pub section: Section,
    pub rows: Vec<&'a SessionSummary>,
    pub folded: usize,
}

/// Where a conversation is listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Section {
    Pinned,
    /// A group the user named.
    Group(String),
    /// Neither pinned nor in a group.
    Rest,
    /// Taken out of the list, while those are shown.
    Archived,
}

impl App {
    /// Where `session` is listed.
    pub(crate) fn section_of(&self, session: &SessionSummary) -> Section {
        if self.show_archived {
            Section::Archived
        } else if self.prefs.pinned.contains(&session.id) {
            Section::Pinned
        } else if let Some(group) = self.prefs.groups.get(&session.id) {
            Section::Group(group.clone())
        } else {
            Section::Rest
        }
    }

    /// The list in its order, those the search finds: the pinned ones in the order they were
    /// pinned, then those in no group, newest first, then each group, the one with the newest
    /// conversation first. The archived ones alone instead, while they are shown. A folded group
    /// shows none of its own, unless something is searched for.
    pub(crate) fn listed(&self) -> Vec<Listed<'_>> {
        let searching = !self.filter.trim().is_empty();
        let found: Vec<&SessionSummary> = if self.show_archived {
            self.sessions.iter().filter(|s| self.finds(s)).collect()
        } else {
            let pins = self
                .prefs
                .pinned
                .iter()
                .filter_map(|id| self.sessions.iter().find(|s| s.id == *id));
            let rest = self.sessions.iter().filter(|s| !self.prefs.pinned.contains(&s.id));
            pins.chain(rest).filter(|s| self.finds(s)).collect()
        };
        let mut out: Vec<Listed<'_>> = Vec::new();
        let mut sections = vec![Section::Archived, Section::Pinned, Section::Rest];
        sections.extend(self.group_names().into_iter().map(Section::Group));
        for section in sections {
            let rows: Vec<&SessionSummary> = found
                .iter()
                .copied()
                .filter(|s| self.section_of(s) == section)
                .collect();
            if rows.is_empty() {
                continue;
            }
            let folded = match &section {
                Section::Group(name) if !searching && self.collapsed.contains(name) => rows.len(),
                _ => 0,
            };
            out.push(Listed {
                rows: if folded > 0 { Vec::new() } else { rows },
                section,
                folded,
            });
        }
        out
    }

    /// The groups named so far, the one with the newest conversation first.
    pub(crate) fn group_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for s in &self.sessions {
            if let Some(g) = self.prefs.groups.get(&s.id)
                && !names.contains(g)
            {
                names.push(g.clone());
            }
        }
        for g in self.prefs.groups.values() {
            if !names.contains(g) {
                names.push(g.clone());
            }
        }
        names
    }

    /// The list's messages; any other comes back.
    pub(crate) fn update_list(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let w = self.s.w();
        let task = match msg {
            Msg::Delete(id) => {
                // Asked twice: a log deleted is gone, and `sasacode -r` and `/usage` lose it too.
                if self.confirm_delete.as_deref() != Some(id.as_str()) {
                    self.confirm_delete = Some(id);
                    self.toast = Some(w.delete_sure.to_owned());
                    return Ok(Task::none());
                }
                self.confirm_delete = None;
                if self.chat_of(&id).is_some_and(|c| c.busy()) {
                    self.toast = Some(w.delete_running.to_owned());
                    return Ok(Task::none());
                }
                let Some(path) = self.sessions.iter().find(|s| s.id == id).map(|s| s.path.clone()) else {
                    return Ok(Task::none());
                };
                if let Err(e) = std::fs::remove_file(&path) {
                    self.toast = Some(format!("{}: {e}", path.display()));
                    return Ok(Task::none());
                }
                self.forget(&id);
                self.bodies.remove(&path);
                self.toast = Some(w.deleted.to_owned());
                if self.chat.session.as_deref() == Some(id.as_str()) {
                    self.chat = self.new_chat();
                    return Ok(operation::focus(crate::app::INPUT));
                }
                Task::none()
            }
            Msg::Group(id) => {
                let name = self.prefs.groups.get(&id).cloned().unwrap_or_default();
                self.overlay = Some(Overlay::Group { id, name });
                Task::batch([operation::focus(GROUP), operation::select_all(GROUP)])
            }
            Msg::GroupPick(name) => {
                if let Some(Overlay::Group { name: typed, .. }) = &mut self.overlay {
                    *typed = name;
                }
                return Ok(self.update(Msg::GroupSave));
            }
            Msg::GroupSave => {
                if let Some(Overlay::Group { id, name }) = self.overlay.take() {
                    let name = name.trim().to_owned();
                    let _ = self.set(|p| {
                        if name.is_empty() {
                            p.groups.remove(&id);
                        } else {
                            p.groups.insert(id, name);
                        }
                    });
                }
                operation::focus(crate::app::INPUT)
            }
            Msg::FoldGroup(name) => {
                if !self.collapsed.remove(&name) {
                    self.collapsed.insert(name);
                }
                Task::none()
            }
            other => return Err(other),
        };
        Ok(task)
    }

    /// A deleted conversation: what the window held of it, and what the settings said of it.
    fn forget(&mut self, id: &str) {
        self.parked.retain(|c| c.session.as_deref() != Some(id));
        let _ = self.set(|p| {
            p.names.remove(id);
            p.pinned.retain(|x| x != id);
            p.archived.remove(id);
            p.branches.remove(id);
            p.groups.remove(id);
        });
        self.sessions.retain(|s| s.id != id);
    }
}
