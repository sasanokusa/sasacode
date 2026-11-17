//! Text as the terminal sees it: cell widths, ANSI-styled strings, wrapping and slicing (pi-tui's
//! utils.js). Lines are ANSI strings throughout the UI, as in pi-tui; these functions are the only
//! place that knows how wide one is.
//!
//! Positions are byte offsets on char boundaries (JS used UTF-16 indices); escape sequences are
//! found by their ASCII bytes, which never occur inside a multi-byte UTF-8 char.

pub mod ansi;
#[rustfmt::skip]
pub mod tables;
pub mod width;
pub mod wrap;

pub use ansi::{AnsiCodeTracker, extract_ansi_code, strip_terminal_sequences};
pub use width::{
    grapheme_width, is_cjk_break, is_js_whitespace, js_trim, js_trim_end, set_ambiguous_width, visible_width,
};
pub use wrap::{
    apply_background_to_line, extract_segments, get_grapheme_cell_range, slice_by_column, slice_with_width,
    truncate_to_width, wrap_text_with_ansi,
};

use unicode_segmentation::UnicodeSegmentation;

/// The graphemes of `s` (extended clusters, as `Intl.Segmenter` gives them).
pub fn graphemes(s: &str) -> impl Iterator<Item = &str> {
    s.graphemes(true)
}

/// Zero-width marker a focused component puts where the hardware cursor belongs (an APC sequence
/// terminals ignore). The renderer finds it, removes it and moves the cursor there (IME candidates).
pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";

/// What each rendered line ends with: no style or link bleeds into the next line or the margin.
pub const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

/// A piece of an ANSI string: an escape sequence or a run of text between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece<'a> {
    Ansi(&'a str),
    Text(&'a str),
}

/// `s` split into escape sequences and the text runs between them, in order. An ESC that starts no
/// recognised sequence stays in the text (pi-tui counts it as a zero-width control).
pub fn pieces(s: &str) -> Vec<Piece<'_>> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut text_start = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b
            && let Some(len) = extract_ansi_code(s, i)
        {
            if text_start < i {
                out.push(Piece::Text(&s[text_start..i]));
            }
            out.push(Piece::Ansi(&s[i..i + len]));
            i += len;
            text_start = i;
            continue;
        }
        i += 1;
    }
    if text_start < bytes.len() {
        out.push(Piece::Text(&s[text_start..]));
    }
    out
}

/// JS `s.length`: UTF-16 code units.
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}
