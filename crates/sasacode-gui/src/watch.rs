//! The conversation shown, continued elsewhere: when its log grows while no run of this window
//! writes it (the TUI, a headless run, another window), it is read again.

use std::path::Path;
use std::time::SystemTime;

use crate::app::App;
use crate::transcript::Transcript;

/// What a log is now: its length and when it was last written.
pub type Seen = (u64, Option<SystemTime>);

/// How `log` is now, if it can be read.
pub fn seen(log: &Path) -> Option<Seen> {
    let meta = std::fs::metadata(log).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

impl App {
    /// Whether the conversation shown is worth watching: it has a log, and no run here writes it.
    pub(crate) fn watches_log(&self) -> bool {
        self.chat.session.is_some() && !self.chat.busy()
    }

    /// Reads the conversation shown again if its log changed since it was last looked at. The
    /// first look only takes note.
    pub(crate) fn watch_log(&mut self) {
        if !self.watches_log() {
            return;
        }
        let Some(id) = self.chat.session.clone() else {
            return;
        };
        let Some(log) = self.sessions.iter().find(|s| s.id == id).map(|s| s.path.clone()) else {
            return;
        };
        let now = seen(&log);
        let before = self.chat.log_seen.replace(now.unwrap_or((0, None)));
        if before.is_none() || before == now || now.is_none() {
            return;
        }
        let Ok(entries) = sasacode_protocol::read_entries(&log) else {
            return;
        };
        let mut transcript = Transcript::from_entries(&entries);
        for ex in &mut transcript.exchanges {
            ex.prompt = crate::transcript::as_typed(&ex.prompt, &self.cwd);
        }
        self.chat.transcript = transcript;
        self.chat.loaded = true;
        self.reload_sessions();
    }
}
