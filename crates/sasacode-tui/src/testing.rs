//! A stand-in terminal for driving the UI in tests: input in, everything written recorded, no TTY.

use std::sync::{Arc, Mutex, MutexGuard};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::mpsc;

use crate::keys::InputEvent;
use crate::term::Terminal;

/// One lock for tests that touch process-wide state (the UI language, the ambiguous width, the
/// expanded-view toggle): Rust runs tests in parallel threads of one process.
pub fn global_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

struct State {
    out: String,
    columns: u16,
    rows: u16,
    tx: mpsc::UnboundedSender<InputEvent>,
    rx: Option<mpsc::UnboundedReceiver<InputEvent>>,
    started: usize,
    stopped: usize,
    ambiguous: Option<u8>,
}

/// The terminal a test holds a clone of while the UI runs with another.
#[derive(Clone)]
pub struct FakeTerminal {
    state: Arc<Mutex<State>>,
}

/// The KeyEvent a key id names ("ctrl+c", "shift+tab", "enter", "a").
pub fn key_event(id: &str) -> KeyEvent {
    let lower = id.to_lowercase();
    let parts: Vec<&str> = lower.split('+').collect();
    let (key, mods) = parts
        .split_last()
        .map(|(k, m)| (*k, m.to_vec()))
        .unwrap_or(("", Vec::new()));
    let mut m = KeyModifiers::NONE;
    for part in mods {
        match part {
            "ctrl" => m |= KeyModifiers::CONTROL,
            "shift" => m |= KeyModifiers::SHIFT,
            "alt" => m |= KeyModifiers::ALT,
            "super" => m |= KeyModifiers::SUPER,
            _ => {}
        }
    }
    let code = match key {
        "escape" | "esc" => KeyCode::Esc,
        "enter" => KeyCode::Enter,
        "tab" if m.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Char(' '),
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        k => KeyCode::Char(k.chars().next().unwrap_or(' ')),
    };
    KeyEvent::new(code, m)
}

impl FakeTerminal {
    pub fn new(columns: u16, rows: u16) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            state: Arc::new(Mutex::new(State {
                out: String::new(),
                columns,
                rows,
                tx,
                rx: Some(rx),
                started: 0,
                stopped: 0,
                ambiguous: None,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Answer the ① probe with this width.
    pub fn set_ambiguous_answer(&self, width: Option<u8>) {
        self.lock().ambiguous = width;
    }

    pub fn send(&self, ev: InputEvent) {
        let _ = self.lock().tx.send(ev);
    }

    /// A key by id ("enter", "shift+tab", "ctrl+c").
    pub fn key(&self, id: &str) {
        self.send(InputEvent::Key(key_event(id)));
    }

    /// Typed text: one key per char.
    pub fn type_text(&self, text: &str) {
        for c in text.chars() {
            let ev = match c {
                '\r' | '\n' => key_event("enter"),
                c => KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            };
            self.send(InputEvent::Key(ev));
        }
    }

    /// A bracketed paste.
    pub fn paste(&self, text: &str) {
        self.send(InputEvent::Paste(text.to_string()));
    }

    pub fn resize(&self, columns: u16, rows: u16) {
        {
            let mut s = self.lock();
            s.columns = columns;
            s.rows = rows;
        }
        self.send(InputEvent::Resize(columns, rows));
    }

    /// Everything written so far, escape sequences included.
    pub fn output(&self) -> String {
        self.lock().out.clone()
    }

    /// The length of the output so far, to read only what comes after.
    pub fn mark(&self) -> usize {
        self.lock().out.len()
    }

    /// Everything written since `from`, with CSI and BEL-terminated OSC sequences removed (the
    /// TypeScript tests' `text()`).
    pub fn text(&self, from: usize) -> String {
        let out = self.lock().out.clone();
        strip_for_tests(out.get(from..).unwrap_or(""))
    }

    pub fn starts(&self) -> usize {
        self.lock().started
    }

    pub fn stops(&self) -> usize {
        self.lock().stopped
    }
}

/// `s.replace(/\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07/g, "")`.
pub fn strip_for_tests(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut copy_from = 0;
    while i < b.len() {
        if b[i] == 0x1b && b.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b';' || b[j] == b'?') {
                j += 1;
            }
            if j < b.len() && b[j].is_ascii_alphabetic() {
                out.push_str(&s[copy_from..i]);
                i = j + 1;
                copy_from = i;
                continue;
            }
        } else if b[i] == 0x1b
            && b.get(i + 1) == Some(&b']')
            && let Some(end) = b[i + 2..].iter().position(|&c| c == 0x07)
        {
            out.push_str(&s[copy_from..i]);
            i = i + 2 + end + 1;
            copy_from = i;
            continue;
        }
        i += 1;
    }
    out.push_str(&s[copy_from..]);
    out
}

impl Terminal for FakeTerminal {
    fn start(&mut self) -> anyhow::Result<BoxStream<'static, InputEvent>> {
        let mut s = self.lock();
        s.started += 1;
        let rx = match s.rx.take() {
            Some(rx) => rx,
            None => {
                // Started again (after a suspend): a fresh channel.
                let (tx, rx) = mpsc::unbounded_channel();
                s.tx = tx;
                rx
            }
        };
        Ok(futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|ev| (ev, rx)) }).boxed())
    }

    fn stop(&mut self) {
        self.lock().stopped += 1;
    }

    fn write(&mut self, data: &str) {
        self.lock().out.push_str(data);
    }

    fn columns(&self) -> u16 {
        self.lock().columns
    }

    fn rows(&self) -> u16 {
        self.lock().rows
    }

    fn probe_ambiguous_width(&mut self) -> Option<u8> {
        self.lock().ambiguous
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_like_the_typescript_helper() {
        assert_eq!(strip_for_tests("a\x1b[31mb\x1b[0m\x1b]8;;u\x07c\x1b]8;;\x07"), "abc");
        assert_eq!(strip_for_tests("\x1b[?2026h日本\x1b[2K"), "日本");
        assert_eq!(strip_for_tests("\x1b_pi:c\x07x"), "\x1b_pi:c\x07x");
    }
}
