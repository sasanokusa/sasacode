//! Escape sequences inside lines: finding them, stripping them, and tracking the SGR style and
//! OSC 8 link in effect so a wrapped line can reopen them (pi-tui `extractAnsiCode`,
//! `AnsiCodeTracker`).

use super::{Piece, pieces};

/// The length in bytes of the escape sequence starting at byte `pos`, if one does: CSI up to its
/// first `m`, `G`, `K`, `H` or `J` (as pi-tui scans), OSC and APC up to BEL or ST.
pub fn extract_ansi_code(s: &str, pos: usize) -> Option<usize> {
    let b = s.as_bytes();
    if b.get(pos) != Some(&0x1b) {
        return None;
    }
    match b.get(pos + 1) {
        Some(b'[') => {
            let mut j = pos + 2;
            while j < b.len() {
                if matches!(b[j], b'm' | b'G' | b'K' | b'H' | b'J') {
                    return Some(j + 1 - pos);
                }
                j += 1;
            }
            None
        }
        Some(b']') | Some(b'_') => {
            let mut j = pos + 2;
            while j < b.len() {
                if b[j] == 0x07 {
                    return Some(j + 1 - pos);
                }
                if b[j] == 0x1b && b.get(j + 1) == Some(&b'\\') {
                    return Some(j + 2 - pos);
                }
                j += 1;
            }
            None
        }
        _ => None,
    }
}

/// `s` without its escape sequences.
pub fn strip_terminal_sequences(s: &str) -> String {
    if !s.contains('\x1b') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for p in pieces(s) {
        if let Piece::Text(t) = p {
            out.push_str(t);
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hyperlink {
    pub params: String,
    pub url: String,
    pub terminator: &'static str,
}

/// An OSC 8 sequence: `None` when `code` is not one, `Some(None)` for a close (empty URL).
pub fn parse_osc8(code: &str) -> Option<Option<Hyperlink>> {
    let rest = code.strip_prefix("\x1b]8;")?;
    let (body, terminator) = if let Some(b) = rest.strip_suffix('\x07') {
        (b, "\x07")
    } else {
        (rest.strip_suffix("\x1b\\").unwrap_or(rest), "\x1b\\")
    };
    let (params, url) = body.split_once(';')?;
    if url.is_empty() {
        return Some(None);
    }
    Some(Some(Hyperlink {
        params: params.to_string(),
        url: url.to_string(),
        terminator,
    }))
}

fn format_osc8(h: &Hyperlink) -> String {
    format!("\x1b]8;{};{}{}", h.params, h.url, h.terminator)
}

fn format_osc8_close(terminator: &str) -> String {
    format!("\x1b]8;;{terminator}")
}

/// The OSC 8 close for a link still open at the end of `prefix`, or "".
pub fn active_osc8_close(prefix: &str) -> String {
    if !prefix.contains("\x1b]8;") {
        return String::new();
    }
    let mut active: Option<Hyperlink> = None;
    for p in pieces(prefix) {
        if let Piece::Ansi(code) = p
            && let Some(h) = parse_osc8(code)
        {
            active = h;
        }
    }
    active.map(|h| format_osc8_close(h.terminator)).unwrap_or_default()
}

/// The SGR attributes and the link in effect after some ANSI text.
#[derive(Debug, Clone, Default)]
pub struct AnsiCodeTracker {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    inverse: bool,
    hidden: bool,
    strikethrough: bool,
    fg: Option<String>,
    bg: Option<String>,
    link: Option<Hyperlink>,
}

impl AnsiCodeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn process(&mut self, code: &str) {
        if let Some(h) = parse_osc8(code) {
            self.link = h;
            return;
        }
        if !code.ends_with('m') {
            return;
        }
        let Some(params) = code.strip_prefix("\x1b[").and_then(|s| s.strip_suffix('m')) else {
            return;
        };
        if !params.bytes().all(|b| b.is_ascii_digit() || b == b';') {
            return;
        }
        if params.is_empty() || params == "0" {
            self.reset();
            return;
        }
        let parts: Vec<&str> = params.split(';').collect();
        let mut i = 0;
        while i < parts.len() {
            let code: Option<i64> = parts[i].parse().ok();
            if let Some(c @ (38 | 48)) = code {
                if parts.get(i + 1) == Some(&"5") && i + 2 < parts.len() {
                    let color = format!("{};{};{}", parts[i], parts[i + 1], parts[i + 2]);
                    if c == 38 {
                        self.fg = Some(color);
                    } else {
                        self.bg = Some(color);
                    }
                    i += 3;
                    continue;
                } else if parts.get(i + 1) == Some(&"2") && i + 4 < parts.len() {
                    let color = parts[i..i + 5].join(";");
                    if c == 38 {
                        self.fg = Some(color);
                    } else {
                        self.bg = Some(color);
                    }
                    i += 5;
                    continue;
                }
            }
            match code {
                Some(0) => self.reset(),
                Some(1) => self.bold = true,
                Some(2) => self.dim = true,
                Some(3) => self.italic = true,
                Some(4) => self.underline = true,
                Some(5) => self.blink = true,
                Some(7) => self.inverse = true,
                Some(8) => self.hidden = true,
                Some(9) => self.strikethrough = true,
                Some(21) => self.bold = false,
                Some(22) => {
                    self.bold = false;
                    self.dim = false;
                }
                Some(23) => self.italic = false,
                Some(24) => self.underline = false,
                Some(25) => self.blink = false,
                Some(27) => self.inverse = false,
                Some(28) => self.hidden = false,
                Some(29) => self.strikethrough = false,
                Some(39) => self.fg = None,
                Some(49) => self.bg = None,
                Some(c @ (30..=37 | 90..=97)) => self.fg = Some(c.to_string()),
                Some(c @ (40..=47 | 100..=107)) => self.bg = Some(c.to_string()),
                _ => {}
            }
            i += 1;
        }
    }

    /// SGR 0: every attribute off. The link stays (SGR does not close OSC 8).
    pub fn reset(&mut self) {
        let link = self.link.take();
        *self = Self {
            link,
            ..Self::default()
        };
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The sequence that turns the tracked style and link back on.
    pub fn active_codes(&self) -> String {
        let mut codes: Vec<&str> = Vec::new();
        for (on, c) in [
            (self.bold, "1"),
            (self.dim, "2"),
            (self.italic, "3"),
            (self.underline, "4"),
            (self.blink, "5"),
            (self.inverse, "7"),
            (self.hidden, "8"),
            (self.strikethrough, "9"),
        ] {
            if on {
                codes.push(c);
            }
        }
        if let Some(fg) = &self.fg {
            codes.push(fg);
        }
        if let Some(bg) = &self.bg {
            codes.push(bg);
        }
        let mut out = if codes.is_empty() {
            String::new()
        } else {
            format!("\x1b[{}m", codes.join(";"))
        };
        if let Some(h) = &self.link {
            out.push_str(&format_osc8(h));
        }
        out
    }

    pub fn active_background_code(&self) -> String {
        self.bg.as_ref().map(|bg| format!("\x1b[{bg}m")).unwrap_or_default()
    }

    pub fn has_active_codes(&self) -> bool {
        self.bold
            || self.dim
            || self.italic
            || self.underline
            || self.blink
            || self.inverse
            || self.hidden
            || self.strikethrough
            || self.fg.is_some()
            || self.bg.is_some()
            || self.link.is_some()
    }

    /// What a wrapped line must close at its end: underline (it would bleed into padding) and an
    /// open link (reopened on the next line by [`Self::active_codes`]).
    pub fn line_end_reset(&self) -> String {
        let mut out = String::new();
        if self.underline {
            out.push_str("\x1b[24m");
        }
        if let Some(h) = &self.link {
            out.push_str(&format_osc8_close(h.terminator));
        }
        out
    }

    /// Feed every escape sequence in `text`.
    pub fn update_from_text(&mut self, text: &str) {
        for p in pieces(text) {
            if let Piece::Ansi(code) = p {
                self.process(code);
            }
        }
    }
}

/// The background colour in effect at the end of `text`, as an SGR sequence, or "".
pub fn active_background_ansi(text: &str) -> String {
    let mut t = AnsiCodeTracker::new();
    t.update_from_text(text);
    t.active_background_code()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csi_ends_at_the_first_style_letter_as_pi_tui_scans() {
        assert_eq!(extract_ansi_code("\x1b[31mx", 0), Some(5));
        assert_eq!(extract_ansi_code("\x1b]8;;u\x07x", 0), Some(7));
        assert_eq!(extract_ansi_code("\x1b]8;;u\x1b\\x", 0), Some(8));
        assert_eq!(extract_ansi_code("\x1b_pi:c\x07", 0), Some(7));
        assert_eq!(extract_ansi_code("\x1bx", 0), None);
        assert_eq!(extract_ansi_code("\x1b[31", 0), None);
    }

    #[test]
    fn tracker_reopens_colours_and_links() {
        let mut t = AnsiCodeTracker::new();
        t.update_from_text("\x1b[1;38;5;196m\x1b]8;;http://x\x07");
        assert_eq!(t.active_codes(), "\x1b[1;38;5;196m\x1b]8;;http://x\x07");
        t.process("\x1b[0m");
        assert_eq!(t.active_codes(), "\x1b]8;;http://x\x07");
        assert_eq!(t.line_end_reset(), "\x1b]8;;\x07");
        t.process("\x1b]8;;\x07");
        assert!(!t.has_active_codes());
    }
}
