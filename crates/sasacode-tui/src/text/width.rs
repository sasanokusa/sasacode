//! Cell widths (pi-tui `visibleWidth` / `graphemeWidth`), from tables dumped from the JavaScript
//! engine so the Rust and TypeScript agree on every code point, East Asian Ambiguous included.

use std::sync::atomic::{AtomicBool, Ordering};

use super::tables::{self, contains};
use super::{Piece, graphemes, pieces};

/// East Asian Ambiguous characters (①, ○, α, …) take 1 or 2 cells depending on the terminal: the
/// sasacode patch to pi-tui, set once from the ① probe (see `ambiguous.rs`).
static AMBIGUOUS_WIDE: AtomicBool = AtomicBool::new(false);

pub fn set_ambiguous_width(width: u8) {
    AMBIGUOUS_WIDE.store(width == 2, Ordering::Relaxed);
}

pub fn ambiguous_width() -> u8 {
    if AMBIGUOUS_WIDE.load(Ordering::Relaxed) { 2 } else { 1 }
}

fn east_asian_width(cp: u32) -> usize {
    if contains(tables::WIDE, cp) || (AMBIGUOUS_WIDE.load(Ordering::Relaxed) && contains(tables::AMBIGUOUS, cp)) {
        2
    } else {
        1
    }
}

fn is_default_ignorable(c: char) -> bool {
    contains(tables::DEFAULT_IGNORABLE, c as u32)
}
fn is_control(c: char) -> bool {
    contains(tables::CONTROL, c as u32)
}
fn is_format(c: char) -> bool {
    contains(tables::FORMAT, c as u32)
}
fn is_mark(c: char) -> bool {
    contains(tables::MARK, c as u32)
}

/// Marks terminals give a cell even after a base character: spacing marks (but U+1734, U+302E/F)
/// and the non-spacing ones legacy wcwidth tables count.
fn is_terminal_spacing_mark(c: char) -> bool {
    let cp = c as u32;
    contains(tables::SPACING_MARK, cp)
        || matches!(cp, 0x065F | 0x0F7F | 0x102B | 0x102C | 0x1031 | 0x1033..=0x1035 | 0x1038 | 0x103A..=0x103E)
}

fn is_non_printing(c: char) -> bool {
    is_default_ignorable(c) || is_control(c) || is_format(c) || is_mark(c)
}

/// JS `/\s/`.
pub fn is_js_whitespace(c: char) -> bool {
    contains(tables::JS_WHITESPACE, c as u32)
}

/// JS `String.prototype.trimEnd`.
pub fn js_trim_end(s: &str) -> &str {
    s.trim_end_matches(is_js_whitespace)
}

/// JS `String.prototype.trim`.
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// pi-tui `cjkBreakRegex`: Han, Hiragana, Katakana, Hangul or Bopomofo (by script extensions).
pub fn is_cjk_break(c: char) -> bool {
    contains(tables::CJK_BREAK, c as u32)
}

/// pi-tui `cjkPunctuationRegex`: CJK punctuation and the fullwidth marks prose uses.
pub fn is_cjk_punctuation(c: char) -> bool {
    contains(tables::CJK_PUNCT, c as u32)
}

/// JS `/[\p{L}\p{N}]/u`.
pub fn is_letter_or_number(c: char) -> bool {
    contains(tables::LETTER_OR_NUMBER, c as u32)
}

/// JS `/\p{P}/u`.
pub fn is_punctuation(c: char) -> bool {
    contains(tables::PUNCTUATION, c as u32)
}

fn is_regional_indicator(cp: u32) -> bool {
    (0x1F1E6..=0x1F1FF).contains(&cp)
}

/// A cheap filter before the emoji test (pi-tui `couldBeEmoji`).
fn could_be_emoji(segment: &str) -> bool {
    let Some(c) = segment.chars().next() else { return false };
    let cp = c as u32;
    (0x1F000..=0x1FBFF).contains(&cp)
        || (0x2300..=0x23FF).contains(&cp)
        || (0x2600..=0x27BF).contains(&cp)
        || (0x2B50..=0x2B55).contains(&cp)
        || segment.contains('\u{FE0F}')
        || super::utf16_len(segment) > 2
}

/// `/^\p{RGI_Emoji}$/v`, from the sequence shapes the RGI set is made of: a presentation emoji, an
/// emoji with VS16, a keycap, a flag, a modifier sequence, a tag sequence or a ZWJ sequence of
/// pictographs. (The JS tests membership in the exact RGI list; the shapes are what terminals draw
/// wide, so the rare unlisted ZWJ combination differs at most.)
fn is_rgi_emoji(segment: &str) -> bool {
    let chars: Vec<char> = segment.chars().collect();
    let Some(&first) = chars.first() else { return false };
    let cp = first as u32;
    match chars.len() {
        1 => contains(tables::EMOJI_PRESENTATION, cp) && !is_regional_indicator(cp),
        2 if chars[1] == '\u{FE0F}' => contains(tables::EMOJI, cp),
        2 if is_regional_indicator(cp) => is_regional_indicator(chars[1] as u32),
        2 if (0x1F3FB..=0x1F3FF).contains(&(chars[1] as u32)) => contains(tables::EMOJI_MODIFIER_BASE, cp),
        3 if chars[1] == '\u{FE0F}' && chars[2] == '\u{20E3}' => matches!(first, '0'..='9' | '#' | '*'),
        _ => {
            if cp == 0x1F3F4 && chars.last() == Some(&'\u{E007F}') {
                return chars[1..chars.len() - 1]
                    .iter()
                    .all(|&c| (0xE0020..=0xE007E).contains(&(c as u32)));
            }
            chars.contains(&'\u{200D}') && contains(tables::EXTENDED_PICTOGRAPHIC, cp)
        }
    }
}

/// The cells one grapheme cluster takes (pi-tui `graphemeWidth`).
pub fn grapheme_width(segment: &str) -> usize {
    if segment == "\t" {
        return 3;
    }
    if segment.is_empty() {
        return 0;
    }
    // Some marks occupy cells even without a base character.
    if segment.chars().all(is_terminal_spacing_mark) {
        return segment.chars().count();
    }
    if segment
        .chars()
        .all(|c| is_default_ignorable(c) || is_control(c) || is_mark(c))
    {
        return 0;
    }
    if could_be_emoji(segment) && is_rgi_emoji(segment) {
        return 2;
    }
    let base = segment.trim_start_matches(is_non_printing);
    let mut chars = base.chars();
    let Some(first) = chars.next() else { return 0 };
    let cp = first as u32;
    // Regional indicators render as full-width emoji even alone (mid-stream): keep 2.
    if is_regional_indicator(cp) {
        return 2;
    }
    let mut width = east_asian_width(cp);
    // A cluster can hold several code points terminals give cells to: spacing marks, halfwidth and
    // fullwidth forms, Thai/Lao AM.
    let mut follows_mark = false;
    for c in chars {
        if is_terminal_spacing_mark(c) {
            width += 1;
            follows_mark = false;
        } else if is_mark(c) {
            follows_mark = true;
        } else if !is_non_printing(c) {
            let cp = c as u32;
            if follows_mark || (0xFF00..=0xFFEF).contains(&cp) {
                width += east_asian_width(cp);
            } else if cp == 0x0E33 || cp == 0x0EB3 {
                width += 1;
            }
            follows_mark = false;
        }
    }
    width
}

fn is_printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

/// The cells `s` takes on screen: escape sequences (CSI, OSC, APC) take none, a tab takes 3.
pub fn visible_width(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }
    if is_printable_ascii(s) {
        return s.len();
    }
    let mut width = 0;
    for piece in pieces(s) {
        if let Piece::Text(text) = piece {
            width += graphemes(text).map(grapheme_width).sum::<usize>();
        }
    }
    width
}
