//! Scratch conversations: asked and answered, then gone. The log is put in the trash once the
//! conversation is left and its run is over, as a deleted one is, and the list never shows it.

use iced::Task;

use crate::app::{App, Chat, Msg};

impl App {
    /// Shows a new scratch conversation.
    pub(crate) fn new_scratch(&mut self) -> Task<Msg> {
        let mut chat = self.new_chat();
        chat.scratch = true;
        self.bring(chat);
        Task::batch([iced::widget::operation::focus(crate::app::INPUT), self.back_to_newest()])
    }

    /// Puts the log of `chat`, a scratch conversation left, in the trash.
    pub(crate) fn drop_scratch(&mut self, chat: &Chat) {
        let (Some(id), Some(home)) = (chat.session.as_ref(), self.home.clone()) else {
            return;
        };
        let log = self.sessions.iter().find(|s| &s.id == id).map(|s| s.path.clone());
        if let Some(log) = log
            && crate::rows::to_trash(&home, &log).is_ok()
        {
            self.reload_sessions();
        }
    }
}
