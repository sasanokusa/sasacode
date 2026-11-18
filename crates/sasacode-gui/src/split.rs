//! A second conversation beside the one shown: opened by ⌘ and a click in the list, or from the
//! palette. It goes on as it would in the list, its run's news drawn as they come; to answer or
//! type in it, it is taken into the main place, and the one there goes beside.

use std::path::Path;

use iced::Task;
use sasacode_protocol::read_entries;

use crate::app::{App, Msg};
use crate::overlay::Entry;
use crate::transcript::Transcript;

impl App {
    /// The conversation beside the one shown, if any.
    pub(crate) fn split_chat(&self) -> Option<&crate::app::Chat> {
        let id = self.split.as_deref()?;
        self.parked.iter().find(|c| c.session.as_deref() == Some(id))
    }

    /// The conversations of the list that can go beside the one shown, by what is typed.
    pub(crate) fn split_entries(&self, query: &str) -> Vec<Entry> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        self.found()
            .filter(|s| self.chat.session.as_deref() != Some(s.id.as_str()))
            .map(|s| {
                let name = self
                    .prefs
                    .names
                    .get(&s.id)
                    .map(String::as_str)
                    .unwrap_or(s.first_prompt.trim());
                (s, crate::view::clip(crate::view::first_line(name), 70))
            })
            .filter(|(_, label)| {
                let lower = label.to_lowercase();
                words.iter().all(|w| lower.contains(w))
            })
            .map(|(s, label)| Entry {
                label,
                keys: None,
                msg: Msg::Split(s.path.clone()),
            })
            .collect()
    }

    /// The split's messages; any other comes back.
    pub(crate) fn update_split(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        match msg {
            Msg::Split(path) => {
                self.open_beside(&path);
            }
            Msg::SplitClose => {
                // Kept only to be shown beside: let go.
                if let Some(id) = self.split.take() {
                    self.parked
                        .retain(|c| c.session.as_deref() != Some(id.as_str()) || c.wanted());
                }
            }
            Msg::SplitSwap => {
                let Some(id) = self.split.take() else {
                    return Ok(Task::none());
                };
                let Some(i) = self
                    .parked
                    .iter()
                    .position(|c| c.session.as_deref() == Some(id.as_str()))
                else {
                    return Ok(Task::none());
                };
                let chat = self.parked.remove(i);
                let left = self.chat.session.clone();
                self.bring_keeping(chat);
                // The one that was shown goes beside, if it is a conversation yet.
                self.split = left;
                return Ok(Task::batch([
                    iced::widget::operation::focus(crate::app::INPUT),
                    self.update(Msg::Latest),
                ]));
            }
            Msg::Modifiers(m) => self.modifiers = m,
            other => return self.update_helper(other),
        }
        Ok(Task::none())
    }

    /// Puts the conversation a log holds beside the one shown: as it is kept if it is, else read
    /// from its log.
    fn open_beside(&mut self, path: &Path) {
        let Some(id) = self.sessions.iter().find(|s| s.path == path).map(|s| s.id.clone()) else {
            return;
        };
        if self.chat.session.as_deref() == Some(id.as_str()) {
            return;
        }
        if !self.parked.iter().any(|c| c.session.as_deref() == Some(id.as_str())) {
            let Ok(entries) = read_entries(path) else {
                return;
            };
            let mut chat = self.new_chat();
            chat.session = Some(id.clone());
            chat.transcript = Transcript::from_entries(&entries);
            for ex in &mut chat.transcript.exchanges {
                ex.prompt = crate::transcript::as_typed(&ex.prompt, &self.cwd);
            }
            chat.loaded = true;
            self.parked.push(chat);
        }
        self.split = Some(id);
    }
}
