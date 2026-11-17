//! The terminal the UI draws on: the real one (raw mode, bracketed paste, the kitty keyboard
//! protocol, mouse reporting) or a stand-in that records what is written (tests). Everything shown
//! goes through [`Terminal::write`]; nothing else writes to stdout while the UI owns the terminal.

pub mod alt;
pub mod cells;
pub mod inline;
pub mod screen;

use std::io::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;
use futures::stream::BoxStream;

use crate::keys::InputEvent;

/// What the UI needs from a terminal.
pub trait Terminal: Send + 'static {
    /// Raw mode, bracketed paste and the kitty keyboard flags on; the input events from now on.
    fn start(&mut self) -> anyhow::Result<BoxStream<'static, InputEvent>>;
    /// Everything [`Terminal::start`] turned on, off again.
    fn stop(&mut self);
    fn write(&mut self, data: &str);
    fn columns(&self) -> u16;
    fn rows(&self) -> u16;
    /// How many cells ① takes here (2 in terminals set up for CJK), asked before [`Terminal::start`].
    /// `None` when there is no terminal to ask or it does not answer.
    fn probe_ambiguous_width(&mut self) -> Option<u8> {
        None
    }
    fn hide_cursor(&mut self) {
        self.write("\x1b[?25l");
    }
    fn show_cursor(&mut self) {
        self.write("\x1b[?25h");
    }
    /// Raw mode and the input modes on again after a panic hook gave the terminal back while the
    /// UI kept running (a plugin's panic is caught).
    fn reenter(&mut self) {}
}

/// What the panic hook must undo, kept where a panic on any thread can see it.
static RAW: AtomicBool = AtomicBool::new(false);
static ALT: AtomicBool = AtomicBool::new(false);
static MOUSE: AtomicBool = AtomicBool::new(false);
static HOOK: Mutex<bool> = Mutex::new(false);
/// Set when the panic hook gave the terminal back: the UI, if it is still running, takes it again.
static RESTORED: AtomicBool = AtomicBool::new(false);

/// True once after the panic hook restored the terminal.
pub fn take_panic_restored() -> bool {
    RESTORED.swap(false, Ordering::Relaxed)
}

/// The sequences that leave every mode the UI may have turned on.
fn restore_sequence() -> String {
    let mut s = String::from("\x1b[<u\x1b[?2004l");
    if MOUSE.load(Ordering::Relaxed) {
        s.push_str(alt::MOUSE_OFF);
    }
    s.push_str("\x1b[?7h\x1b[?25h");
    if ALT.load(Ordering::Relaxed) {
        s.push_str("\x1b[?1049l");
    }
    s
}

/// Leave raw mode, the alternate screen and the kitty keyboard flags before a panic message is
/// printed, so a crash leaves a usable shell. Installed once, by the real terminal only.
fn install_panic_hook() {
    let mut installed = HOOK.lock().unwrap_or_else(|e| e.into_inner());
    if *installed {
        return;
    }
    *installed = true;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if RAW.swap(false, Ordering::Relaxed) {
            let mut out = std::io::stdout();
            let _ = out.write_all(restore_sequence().as_bytes());
            let _ = out.flush();
            let _ = crossterm::terminal::disable_raw_mode();
            ALT.store(false, Ordering::Relaxed);
            MOUSE.store(false, Ordering::Relaxed);
            RESTORED.store(true, Ordering::Relaxed);
        }
        previous(info);
    }));
}

/// Mark the alternate screen and mouse reporting on or off, for the panic hook.
pub(crate) fn note_alt_screen(on: bool) {
    ALT.store(on, Ordering::Relaxed);
}
pub(crate) fn note_mouse(on: bool) {
    MOUSE.store(on, Ordering::Relaxed);
}

/// The process's own terminal (stdin / stdout).
pub struct ProcessTerminal {
    started: bool,
}

impl ProcessTerminal {
    pub fn new() -> Self {
        Self { started: false }
    }
}

impl Default for ProcessTerminal {
    fn default() -> Self {
        Self::new()
    }
}

fn map_event(ev: crossterm::event::Event) -> Option<InputEvent> {
    use crossterm::event::Event;
    Some(match ev {
        Event::Key(k) => InputEvent::Key(k),
        Event::Paste(p) => InputEvent::Paste(p),
        Event::Mouse(m) => InputEvent::Mouse(m),
        Event::Resize(w, h) => InputEvent::Resize(w, h),
        Event::FocusGained => InputEvent::FocusGained,
        Event::FocusLost => InputEvent::FocusLost,
    })
}

impl Terminal for ProcessTerminal {
    fn start(&mut self) -> anyhow::Result<BoxStream<'static, InputEvent>> {
        install_panic_hook();
        crossterm::terminal::enable_raw_mode()?;
        RAW.store(true, Ordering::Relaxed);
        self.started = true;
        // Bracketed paste, then the kitty keyboard protocol (disambiguate, event types, alternate
        // keys: flags 7, as pi-tui asks). Terminals without it ignore the push.
        self.write("\x1b[?2004h\x1b[>7u");
        let stream = crossterm::event::EventStream::new()
            .filter_map(|r| async move { r.ok().and_then(map_event) })
            .boxed();
        Ok(stream)
    }

    fn stop(&mut self) {
        if !self.started {
            return;
        }
        self.started = false;
        self.write("\x1b[?2004l\x1b[<u");
        let _ = crossterm::terminal::disable_raw_mode();
        RAW.store(false, Ordering::Relaxed);
    }

    fn write(&mut self, data: &str) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(data.as_bytes());
        let _ = out.flush();
    }

    fn columns(&self) -> u16 {
        crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80)
    }

    fn rows(&self) -> u16 {
        crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24)
    }

    fn reenter(&mut self) {
        if !self.started || crossterm::terminal::enable_raw_mode().is_err() {
            return;
        }
        RAW.store(true, Ordering::Relaxed);
        self.write("\x1b[?2004h\x1b[>7u");
    }

    fn probe_ambiguous_width(&mut self) -> Option<u8> {
        crate::ambiguous::probe_tty(std::time::Duration::from_millis(400))
    }
}
