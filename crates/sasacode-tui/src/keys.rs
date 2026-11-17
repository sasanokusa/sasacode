//! Keys by name ("ctrl+shift+f", "pageUp") and the default bindings (pi-tui keys.js and
//! keybindings.js). crossterm parses the bytes; this matches its events against key ids, folding
//! the legacy encodings (ctrl+] arrives as ctrl+5, ctrl+- as ctrl+7, shift+tab as BackTab, an
//! uppercase letter as its shifted lowercase one).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// One terminal input event, as the UI handles it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Key(KeyEvent),
    /// A bracketed paste, whole.
    Paste(String),
    Mouse(crossterm::event::MouseEvent),
    Resize(u16, u16),
    FocusGained,
    FocusLost,
}

/// A key as matched: the code with ASCII letters lowercased, and the modifiers that count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Norm {
    code: KeyCode,
    ctrl: bool,
    shift: bool,
    alt: bool,
    sup: bool,
}

fn normalize(ev: &KeyEvent) -> Norm {
    let m = ev.modifiers;
    let mut n = Norm {
        code: ev.code,
        ctrl: m.contains(KeyModifiers::CONTROL),
        shift: m.contains(KeyModifiers::SHIFT),
        alt: m.contains(KeyModifiers::ALT),
        sup: m.contains(KeyModifiers::SUPER),
    };
    match ev.code {
        KeyCode::BackTab => {
            n.code = KeyCode::Tab;
            n.shift = true;
        }
        KeyCode::Char(c) if c.is_ascii_uppercase() => {
            n.code = KeyCode::Char(c.to_ascii_lowercase());
            n.shift = true;
        }
        // Legacy control bytes 0x1C-0x1F come out of crossterm as ctrl+4..7.
        KeyCode::Char('5') if n.ctrl => n.code = KeyCode::Char(']'),
        KeyCode::Char('7') | KeyCode::Char('_') | KeyCode::Char('/') if n.ctrl => n.code = KeyCode::Char('-'),
        KeyCode::Char('4') if n.ctrl => n.code = KeyCode::Char('\\'),
        _ => {}
    }
    n
}

fn parse_key_name(key: &str) -> Option<KeyCode> {
    Some(match key {
        "escape" | "esc" => KeyCode::Esc,
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Char(' '),
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        _ => {
            if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())
                && (1..=24).contains(&n)
            {
                return Some(KeyCode::F(n));
            }
            let mut chars = key.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(c)
        }
    })
}

/// Whether `ev` is the key `id` ("ctrl+c", "shift+tab", "pageUp", "ctrl+alt+]"): the same key with
/// exactly the same ctrl, shift, alt and super.
pub fn matches_key(ev: &KeyEvent, id: &str) -> bool {
    let lower = id.to_lowercase();
    let parts: Vec<&str> = lower.split('+').collect();
    // "ctrl++" names the plus key.
    let (key, mods) = if lower.ends_with("++") {
        ("+", &parts[..parts.len().saturating_sub(2)])
    } else {
        match parts.split_last() {
            Some((k, m)) => (*k, m),
            None => return false,
        }
    };
    if key.is_empty() {
        return false;
    }
    let Some(code) = parse_key_name(key) else { return false };
    let n = normalize(ev);
    n.code == code
        && n.ctrl == mods.contains(&"ctrl")
        && n.shift == mods.contains(&"shift")
        && n.alt == mods.contains(&"alt")
        && n.sup == mods.contains(&"super")
}

/// A key release (reported with the kitty keyboard protocol): no component acts on one.
pub fn is_key_release(ev: &KeyEvent) -> bool {
    ev.kind == KeyEventKind::Release
}

/// The text a key types, when it types one: a char with no ctrl, alt or super.
pub fn printable(ev: &KeyEvent) -> Option<char> {
    let m = ev.modifiers;
    if m.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) {
        return None;
    }
    match ev.code {
        // shift+n as the base key and a modifier (kitty's alternate keys can report it so): `N`.
        KeyCode::Char(c) if !c.is_control() && m.contains(KeyModifiers::SHIFT) && c.is_lowercase() => {
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(u), None) => Some(u),
                _ => Some(c),
            }
        }
        KeyCode::Char(c) if !c.is_control() => Some(c),
        _ => None,
    }
}

/// The bindable actions with their default keys (pi-tui `TUI_KEYBINDINGS`).
const DEFAULTS: &[(&str, &[&str])] = &[
    ("tui.editor.historyPrevious", &[]),
    ("tui.editor.historyNext", &[]),
    ("tui.editor.cursorUp", &["up"]),
    ("tui.editor.cursorDown", &["down"]),
    ("tui.editor.cursorLeft", &["left", "ctrl+b"]),
    ("tui.editor.cursorRight", &["right", "ctrl+f"]),
    ("tui.editor.cursorWordLeft", &["alt+left", "ctrl+left", "alt+b"]),
    ("tui.editor.cursorWordRight", &["alt+right", "ctrl+right", "alt+f"]),
    ("tui.editor.cursorLineStart", &["home", "ctrl+home", "ctrl+a"]),
    ("tui.editor.cursorLineEnd", &["end", "ctrl+end", "ctrl+e"]),
    ("tui.editor.jumpForward", &["ctrl+]"]),
    ("tui.editor.jumpBackward", &["ctrl+alt+]"]),
    ("tui.editor.pageUp", &["pageUp", "ctrl+pageUp"]),
    ("tui.editor.pageDown", &["pageDown", "ctrl+pageDown"]),
    ("tui.editor.deleteCharBackward", &["backspace"]),
    ("tui.editor.deleteCharForward", &["delete", "ctrl+d"]),
    ("tui.editor.deleteWordBackward", &["ctrl+w", "alt+backspace"]),
    ("tui.editor.deleteWordForward", &["alt+d", "alt+delete"]),
    ("tui.editor.deleteToLineStart", &["ctrl+u"]),
    ("tui.editor.deleteToLineEnd", &["ctrl+k"]),
    ("tui.editor.yank", &["ctrl+y"]),
    ("tui.editor.yankPop", &["alt+y"]),
    ("tui.editor.undo", &["ctrl+-"]),
    ("tui.input.newLine", &["shift+enter", "ctrl+j"]),
    ("tui.input.submit", &["enter"]),
    ("tui.input.tab", &["tab"]),
    ("tui.input.copy", &["ctrl+c"]),
    ("tui.select.up", &["up"]),
    ("tui.select.down", &["down"]),
    ("tui.select.pageUp", &["pageUp"]),
    ("tui.select.pageDown", &["pageDown"]),
    ("tui.select.confirm", &["enter"]),
    ("tui.select.cancel", &["escape", "ctrl+c"]),
    ("tui.altScreen.pageUp", &["pageUp"]),
    ("tui.altScreen.pageDown", &["pageDown"]),
    ("tui.altScreen.halfPageUp", &[]),
    ("tui.altScreen.halfPageDown", &[]),
    ("tui.altScreen.lineUp", &[]),
    ("tui.altScreen.lineDown", &[]),
    ("tui.altScreen.top", &["home"]),
    ("tui.altScreen.bottom", &["end"]),
    ("tui.altScreen.previousPrompt", &["ctrl+shift+up", "ctrl+up"]),
    ("tui.altScreen.nextPrompt", &["ctrl+shift+down", "ctrl+down"]),
    ("tui.altScreen.search", &["ctrl+shift+f"]),
    ("tui.altScreen.searchNext", &["enter", "ctrl+g"]),
    ("tui.altScreen.searchPrevious", &["shift+enter", "ctrl+shift+g"]),
    ("tui.altScreen.searchClose", &["escape"]),
];

/// The key bindings in effect: the defaults, with the actions the app rebinds.
#[derive(Debug, Clone, Default)]
pub struct Keybindings {
    overrides: Vec<(String, Vec<String>)>,
}

impl Keybindings {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebind `action` (pi-tui `setUserBindings`).
    pub fn set(&mut self, action: &str, keys: &[&str]) {
        self.overrides.retain(|(a, _)| a != action);
        self.overrides
            .push((action.to_string(), keys.iter().map(|k| k.to_string()).collect()));
    }

    pub fn keys(&self, action: &str) -> Vec<String> {
        if let Some((_, keys)) = self.overrides.iter().find(|(a, _)| a == action) {
            return keys.clone();
        }
        DEFAULTS
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, keys)| keys.iter().map(|k| k.to_string()).collect())
            .unwrap_or_default()
    }

    pub fn matches(&self, ev: &KeyEvent, action: &str) -> bool {
        self.keys(action).iter().any(|k| matches_key(ev, k))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn key_ids_match_crossterm_events_and_legacy_encodings() {
        assert!(matches_key(&key(KeyCode::Char('c'), KeyModifiers::CONTROL), "ctrl+c"));
        assert!(!matches_key(&key(KeyCode::Char('c'), KeyModifiers::CONTROL), "c"));
        assert!(matches_key(&key(KeyCode::BackTab, KeyModifiers::SHIFT), "shift+tab"));
        assert!(matches_key(&key(KeyCode::BackTab, KeyModifiers::NONE), "shift+tab"));
        assert!(matches_key(&key(KeyCode::Char('5'), KeyModifiers::CONTROL), "ctrl+]"));
        let ctrl_alt = KeyModifiers::CONTROL | KeyModifiers::ALT;
        assert!(matches_key(&key(KeyCode::Char('5'), ctrl_alt), "ctrl+alt+]"));
        assert!(matches_key(&key(KeyCode::Char('7'), KeyModifiers::CONTROL), "ctrl+-"));
        assert!(matches_key(
            &key(KeyCode::Char('F'), KeyModifiers::CONTROL),
            "ctrl+shift+f"
        ));
        assert!(matches_key(&key(KeyCode::PageUp, KeyModifiers::NONE), "pageUp"));
        assert!(matches_key(&key(KeyCode::Char(' '), KeyModifiers::NONE), "space"));
        assert!(matches_key(&key(KeyCode::Enter, KeyModifiers::SHIFT), "shift+enter"));
        assert!(!matches_key(&key(KeyCode::Enter, KeyModifiers::SHIFT), "enter"));
    }

    #[test]
    fn user_bindings_replace_the_defaults() {
        let mut kb = Keybindings::new();
        assert!(kb.matches(&key(KeyCode::Home, KeyModifiers::NONE), "tui.altScreen.top"));
        kb.set("tui.altScreen.top", &["ctrl+home"]);
        assert!(!kb.matches(&key(KeyCode::Home, KeyModifiers::NONE), "tui.altScreen.top"));
        assert!(kb.matches(&key(KeyCode::Home, KeyModifiers::CONTROL), "tui.altScreen.top"));
    }
}
