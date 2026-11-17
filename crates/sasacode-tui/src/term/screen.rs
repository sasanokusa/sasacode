//! One interface over the two renderers: the app hands over the transcript and the dock, the
//! screen decides how they reach the terminal (inline: one document diffed by rows; full screen:
//! a scroll view over the dock, diffed by cells). It owns the terminal while the UI runs and gives
//! it back as it was, also when dropped early.

use futures::stream::BoxStream;

use super::Terminal;
use super::alt::{self, AltScreen, DockPart};
use super::inline::InlineRenderer;
use crate::keys::{InputEvent, Keybindings};

enum Mode {
    Inline(InlineRenderer),
    Alt(Box<AltScreen>),
}

pub struct Screen {
    term: Box<dyn Terminal>,
    mode: Mode,
    started: bool,
}

impl Screen {
    pub fn new(term: Box<dyn Terminal>, full_screen: bool) -> Self {
        let mode = if full_screen {
            Mode::Alt(Box::default())
        } else {
            Mode::Inline(InlineRenderer::new())
        };
        Self {
            term,
            mode,
            started: false,
        }
    }

    pub fn terminal(&mut self) -> &mut dyn Terminal {
        self.term.as_mut()
    }

    pub fn columns(&self) -> usize {
        self.term.columns().max(1) as usize
    }

    pub fn rows(&self) -> usize {
        self.term.rows().max(1) as usize
    }

    pub fn is_full_screen(&self) -> bool {
        matches!(self.mode, Mode::Alt(_))
    }

    pub fn alt(&mut self) -> Option<&mut AltScreen> {
        match &mut self.mode {
            Mode::Alt(a) => Some(a),
            Mode::Inline(_) => None,
        }
    }

    pub fn alt_ref(&self) -> Option<&AltScreen> {
        match &self.mode {
            Mode::Alt(a) => Some(a),
            Mode::Inline(_) => None,
        }
    }

    /// Take the terminal: the alternate screen first (the keyboard flags are pushed inside it),
    /// then raw mode and the input.
    pub fn start(&mut self) -> anyhow::Result<BoxStream<'static, InputEvent>> {
        if let Mode::Alt(_) = self.mode {
            self.term.write(&alt::enter_sequence());
            super::note_alt_screen(true);
            super::note_mouse(true);
        }
        let input = self.term.start();
        if input.is_err() && self.is_full_screen() {
            self.term.write(&alt::before_stop_sequence());
            self.term.write(&alt::after_stop_sequence());
            super::note_alt_screen(false);
            super::note_mouse(false);
        }
        self.started = input.is_ok();
        input
    }

    /// Back into the terminal after a panic hook gave it back (the UI kept running).
    pub fn reenter(&mut self) {
        if !self.started {
            return;
        }
        if let Mode::Alt(a) = &mut self.mode {
            self.term.write(&alt::enter_sequence());
            super::note_alt_screen(true);
            super::note_mouse(true);
            a.force_full();
        }
        self.term.reenter();
        self.force_full();
    }

    /// Give the terminal back: the normal screen as it was (full screen), or the cursor under the
    /// last line (inline).
    pub fn stop(&mut self) {
        if !self.started {
            return;
        }
        self.started = false;
        match &self.mode {
            Mode::Alt(_) => {
                self.term.write(&alt::before_stop_sequence());
                super::note_mouse(false);
                self.term.show_cursor();
                self.term.stop();
                self.term.write(&alt::after_stop_sequence());
                super::note_alt_screen(false);
            }
            Mode::Inline(r) => {
                let tail = r.stop_sequence(false);
                if !tail.is_empty() {
                    self.term.write(&tail);
                }
                self.term.show_cursor();
                self.term.stop();
            }
        }
    }

    /// The next frame is drawn whole.
    pub fn force_full(&mut self) {
        match &mut self.mode {
            Mode::Alt(a) => a.force_full(),
            Mode::Inline(r) => r.reset(),
        }
    }

    /// Draw a frame: `transcript` (rendered at the screen's width) and the dock's parts under it.
    pub fn render(&mut self, transcript: Vec<String>, dock: &[DockPart], kb: &Keybindings) {
        let (w, h) = (self.columns(), self.rows());
        let out = match &mut self.mode {
            Mode::Alt(a) => a.render(transcript, dock, w, h, kb),
            Mode::Inline(r) => {
                let mut lines = transcript;
                for part in dock {
                    lines.extend(part.lines.iter().cloned());
                }
                r.render(lines, w, h)
            }
        };
        if !out.is_empty() {
            self.term.write(&out);
        }
    }

    pub fn write(&mut self, data: &str) {
        self.term.write(data);
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        // An early return or a panic in the loop still gives the terminal back.
        self.stop();
    }
}
