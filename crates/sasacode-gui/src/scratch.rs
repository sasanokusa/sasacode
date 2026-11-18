//! Scratch conversations: asked and answered, then gone. The log is put in the trash once the
//! conversation is left and its run is over, as a deleted one is, and the list never shows it.

use iced::Task;

use crate::app::{App, Chat, Msg};

impl App {
    /// Shows a new scratch conversation; a `bare` one is run in an empty folder of its own.
    pub(crate) fn new_scratch(&mut self, bare: bool) -> Task<Msg> {
        let mut chat = self.new_chat();
        chat.scratch = true;
        chat.bare = bare;
        self.bring(chat);
        Task::batch([iced::widget::operation::focus(crate::app::INPUT), self.back_to_newest()])
    }

    /// Whether conversation `key` is run with no folder.
    pub(crate) fn is_bare(&self, key: u64) -> bool {
        std::iter::once(&self.chat)
            .chain(&self.parked)
            .find(|c| c.key == key)
            .is_some_and(|c| c.bare)
    }

    /// The empty folder conversations with no folder run in, made when it is not there: under
    /// sasacode's home, which holds no project.
    pub(crate) fn bare_folder(&self) -> std::path::PathBuf {
        let dir = self.home.clone().unwrap_or_else(std::env::temp_dir).join("scratch");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("sasacode-gui: could not make {}: {e}", dir.display());
        }
        dir
    }

    /// Puts the log of `chat`, a scratch conversation left, in the trash.
    pub(crate) fn drop_scratch(&mut self, chat: &Chat) {
        let (Some(id), Some(home)) = (chat.session.as_ref(), self.home.clone()) else {
            return;
        };
        // One with no folder was logged under its own.
        let theirs = chat
            .bare
            .then(|| sasacode_protocol::list_sessions(&home, &self.bare_folder()));
        let log = theirs
            .as_deref()
            .unwrap_or(&self.sessions)
            .iter()
            .find(|s| &s.id == id)
            .map(|s| s.path.clone());
        if let Some(log) = log
            && crate::rows::to_trash(&home, &log).is_ok()
        {
            self.reload_sessions();
        }
    }
}
