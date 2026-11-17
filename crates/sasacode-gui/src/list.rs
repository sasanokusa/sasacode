//! What the list of conversations does besides opening one: its groups, which the user names and
//! folds, and deleting a conversation's log, into the trash.

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
        // Newest first, by the last time written to, or as the settings say by when started.
        let mut by_order: Vec<&SessionSummary> = self.sessions.iter().collect();
        if self.prefs.more.list_order == crate::layout::Order::Created {
            by_order.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        }
        let wanted = |s: &&SessionSummary| self.finds(s) && self.shows(s);
        let found: Vec<&SessionSummary> = if self.show_archived {
            by_order.into_iter().filter(wanted).collect()
        } else {
            let pins = self
                .prefs
                .pinned
                .iter()
                .filter_map(|id| self.sessions.iter().find(|s| s.id == *id));
            let rest = by_order.into_iter().filter(|s| !self.prefs.pinned.contains(&s.id));
            pins.chain(rest).filter(wanted).collect()
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
                // Asked twice: the log goes to the trash, where `sasacode -r` and `/usage` do not
                // see it, and comes back from it with 元に戻す for a week.
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
                self.delete_one(&id)
            }
            Msg::Group(id) => {
                let name = self.prefs.groups.get(&id).cloned().unwrap_or_default();
                self.overlay = Some(Overlay::Group { ids: vec![id], name });
                Task::batch([operation::focus(GROUP), operation::select_all(GROUP)])
            }
            Msg::GroupPick(name) => {
                if let Some(Overlay::Group { name: typed, .. }) = &mut self.overlay {
                    *typed = name;
                }
                return Ok(self.update(Msg::GroupSave));
            }
            Msg::GroupSave => {
                if let Some(Overlay::Group { ids, name }) = self.overlay.take() {
                    let name = name.trim().to_owned();
                    self.remember();
                    let _ = self.set(|p| {
                        for id in ids {
                            if name.is_empty() {
                                p.groups.remove(&id);
                            } else {
                                p.groups.insert(id, name.clone());
                            }
                        }
                    });
                    self.selected.clear();
                }
                operation::focus(crate::app::INPUT)
            }
            Msg::FoldGroup(name) => {
                if !self.collapsed.remove(&name) {
                    self.collapsed.insert(name);
                }
                Task::none()
            }
            other => return self.update_files(other),
        };
        Ok(task)
    }

    /// A deleted conversation: what the window held of it, and what the settings said of it.
    pub(crate) fn forget(&mut self, id: &str) {
        self.parked.retain(|c| c.session.as_deref() != Some(id));
        let _ = self.set(|p| {
            p.names.remove(id);
            p.pinned.retain(|x| x != id);
            p.archived.remove(id);
            p.branches.remove(id);
            p.groups.remove(id);
            p.more.unread.remove(id);
            p.more.kept.remove(id);
        });
        self.selected.remove(id);
        self.sessions.retain(|s| s.id != id);
    }
}
