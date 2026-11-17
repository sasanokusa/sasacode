//! What a folder's window starts with again: the model, permission and effort its last run used,
//! the conversation last shown there, and what was typed and not sent, over a restart.

use iced::widget::text_editor;

use crate::app::{App, Chat, EFFORTS, PERMISSIONS};

impl App {
    /// The name this folder is kept under: its path.
    pub(crate) fn folder_key(&self) -> String {
        self.cwd.to_string_lossy().into_owned()
    }

    /// What the window opens with in this folder: the choices of its last run, and, unless a
    /// conversation was `asked` for, the one last shown, with what was typed in it. Running
    /// everything without asking is not taken up again: it is chosen each time.
    pub(crate) fn restore_folder(&mut self, asked: bool) {
        let folder = self
            .prefs
            .more
            .folders
            .get(&self.folder_key())
            .cloned()
            .unwrap_or_default();
        self.model = folder.model;
        self.permission = folder
            .permission
            .as_deref()
            .filter(|p| *p != "auto")
            .and_then(|p| PERMISSIONS.iter().copied().find(|x| *x == p));
        self.effort = folder
            .effort
            .as_deref()
            .and_then(|e| EFFORTS.iter().copied().find(|x| *x == e));
        if !asked && let Some(last) = folder.last {
            self.open_named(&last);
        }
        if self.chat.session.is_none()
            && let Some(draft) = self.prefs.more.drafts.get(&self.folder_key())
        {
            self.chat.editor = typed(draft);
        }
    }

    /// A new conversation, with what was typed for one in this folder and not sent.
    pub(crate) fn fresh_chat(&mut self) -> Chat {
        let mut chat = self.new_chat();
        if let Some(draft) = self.prefs.more.drafts.get(&self.folder_key()) {
            chat.editor = typed(draft);
        }
        chat
    }

    /// Keeps what a run used as the folder's choices for its next window.
    pub(crate) fn keep_choices(&mut self) {
        let key = self.folder_key();
        let model = self.model.trim().to_owned();
        let (permission, effort) = (self.permission.map(str::to_owned), self.effort.map(str::to_owned));
        let had = self.prefs.more.folders.get(&key);
        if had.is_some_and(|f| f.model == model && f.permission == permission && f.effort == effort) {
            return;
        }
        let _ = self.set(|p| {
            let folder = p.more.folders.entry(key).or_default();
            folder.model = model;
            folder.permission = permission;
            folder.effort = effort;
        });
    }

    /// Keeps `last` as the conversation shown in this folder, none for a new one.
    pub(crate) fn keep_last(&mut self, last: Option<String>) {
        let key = self.folder_key();
        let had = self.prefs.more.folders.get(&key);
        if had.map_or(last.is_none(), |f| f.last == last) {
            return;
        }
        let _ = self.set(|p| p.more.folders.entry(key).or_default().last = last);
    }

    /// The name what is typed in `chat` is kept under: its session's id, or, for a new
    /// conversation, the folder's.
    fn draft_key(&self, chat: &Chat) -> String {
        chat.session.clone().unwrap_or_else(|| self.folder_key())
    }

    /// What is typed in `chat`, under the name it is kept by.
    fn draft_of(&self, chat: &Chat) -> (String, String) {
        (self.draft_key(chat), chat.editor.text().trim_end().to_owned())
    }

    /// Keeps each typed text over a restart, by name, and forgets a draft emptied. Of new
    /// conversations, which share the folder's name, one typed in wins over an empty one.
    pub(crate) fn keep_drafts(&mut self, all: Vec<(String, String)>) {
        let blank = |t: &str| t.trim().is_empty();
        let mut by_key: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        for (key, text) in all {
            if by_key.get(&key).is_none_or(|had| blank(had)) {
                by_key.insert(key, text);
            }
        }
        let held = &self.prefs.more.drafts;
        let changed = by_key.iter().any(|(k, t)| {
            if blank(t) {
                held.contains_key(k)
            } else {
                held.get(k) != Some(t)
            }
        });
        if !changed {
            return;
        }
        let _ = self.set(|p| {
            for (key, text) in by_key {
                if blank(&text) {
                    p.more.drafts.remove(&key);
                } else {
                    p.more.drafts.insert(key, text);
                }
            }
        });
    }

    /// Keeps the draft of `chat`, a conversation being left. A new one left empty says nothing:
    /// the folder's draft may be another new one's.
    pub(crate) fn keep_draft(&mut self, chat: &Chat) {
        let one = self.draft_of(chat);
        if chat.session.is_some() || !one.1.trim().is_empty() {
            self.keep_drafts(vec![one]);
        }
    }

    /// Keeps every conversation's draft, and the one shown as the folder's last: the window
    /// closes.
    pub(crate) fn keep_all_drafts(&mut self) {
        let all = std::iter::once(&self.chat)
            .chain(&self.parked)
            .filter(|c| !c.scratch)
            .map(|c| self.draft_of(c))
            .collect();
        self.keep_drafts(all);
        self.keep_last(self.chat.session.clone().filter(|_| !self.chat.scratch));
        // The scratch conversations go with the window, their runs stopped with it.
        let scratch: Vec<crate::app::Chat> = std::iter::once(&self.chat)
            .chain(&self.parked)
            .filter(|c| c.scratch)
            .map(|c| {
                let mut gone = Chat::new(c.key);
                gone.session = c.session.clone();
                gone.scratch = true;
                gone
            })
            .collect();
        for chat in &scratch {
            self.drop_scratch(chat);
        }
    }

    /// A message was sent from conversation `key`: what was kept typed in it is gone.
    pub(crate) fn forget_draft(&mut self, key: String) {
        if self.prefs.more.drafts.contains_key(&key) {
            let _ = self.set(|p| {
                p.more.drafts.remove(&key);
            });
        }
    }

    /// What was typed in the conversation with session `id`, kept over a restart.
    pub(crate) fn kept_draft(&self, id: &str) -> Option<&String> {
        self.prefs.more.drafts.get(id)
    }

    /// The draft key of conversation `key`, shown or kept; none for a scratch one, whose drafts
    /// are not kept (and whose new conversation is not the folder's).
    pub(crate) fn draft_key_of(&self, key: u64) -> Option<String> {
        std::iter::once(&self.chat)
            .chain(&self.parked)
            .find(|c| c.key == key && !c.scratch)
            .map(|c| self.draft_key(c))
    }
}

/// An editor holding `text`, the cursor at its end.
fn typed(text: &str) -> text_editor::Content {
    let mut editor = text_editor::Content::with_text(text);
    editor.perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
    editor
}
