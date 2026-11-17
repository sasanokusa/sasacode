//! What the text inputs share: the kill ring, word movement, paste markers and the editor's own
//! word wrap (pi-tui kill-ring.js, word-navigation.js and the helpers at the top of editor.js).
//!
//! Positions are byte offsets. Words are UAX #29 word boundaries (`Intl.Segmenter` uses ICU, whose
//! dictionary also splits Japanese into words; here kanji and hiragana move a character at a time).

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;

use crate::text::width::{is_cjk_break, is_js_whitespace, is_letter_or_number};
use crate::text::{graphemes, utf16_len, visible_width};

/// Killed text (ctrl+k, ctrl+u, ctrl+w), newest last, for yank and yank-pop.
#[derive(Debug, Clone, Default)]
pub struct KillRing {
    ring: Vec<String>,
}

impl KillRing {
    /// Add `text`; with `accumulate`, join it to the newest entry (consecutive kills).
    pub fn push(&mut self, text: &str, prepend: bool, accumulate: bool) {
        if text.is_empty() {
            return;
        }
        if accumulate && let Some(last) = self.ring.pop() {
            self.ring.push(if prepend {
                format!("{text}{last}")
            } else {
                format!("{last}{text}")
            });
        } else {
            self.ring.push(text.to_string());
        }
    }

    pub fn peek(&self) -> Option<&str> {
        self.ring.last().map(String::as_str)
    }

    pub fn rotate(&mut self) {
        if self.ring.len() > 1
            && let Some(last) = self.ring.pop()
        {
            self.ring.insert(0, last);
        }
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

/// The last thing done, for undo coalescing and consecutive kills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastAction {
    TypeWord,
    Kill,
    Yank,
}

/// pi-tui `PUNCTUATION_REGEX`.
fn is_word_punctuation(c: char) -> bool {
    "(){}[]<>.,;:'\"!?+-=*/\\|&%^$#@~`".contains(c)
}

/// `/\s/.test(s)`: any whitespace in it.
pub fn has_whitespace(s: &str) -> bool {
    s.chars().any(is_js_whitespace)
}

/// A piece of text for cursor movement: a grapheme or a word segment, or a whole paste marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seg<'a> {
    pub text: &'a str,
    pub index: usize,
    pub word_like: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegMode {
    Grapheme,
    Word,
}

fn base_segments(text: &str, mode: SegMode) -> Vec<Seg<'_>> {
    match mode {
        SegMode::Grapheme => text
            .grapheme_indices(true)
            .map(|(index, g)| Seg {
                text: g,
                index,
                word_like: false,
            })
            .collect(),
        SegMode::Word => text
            .split_word_bound_indices()
            .map(|(index, w)| Seg {
                text: w,
                index,
                word_like: w.chars().any(|c| is_letter_or_number(c) || c == '_'),
            })
            .collect(),
    }
}

fn marker_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // INFALLIBLE: a literal pattern.
    R.get_or_init(|| Regex::new(r"\[paste #(\d+)( (\+\d+ lines|\d+ chars))?\]").expect("valid regex"))
}

fn marker_single_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // INFALLIBLE: a literal pattern.
    R.get_or_init(|| Regex::new(r"^\[paste #(\d+)( (\+\d+ lines|\d+ chars))?\]$").expect("valid regex"))
}

/// The marker a folded paste is shown as (the sasacode patch to pi-tui): its line count past ten
/// lines, else its length (UTF-16 units, as JS counts).
pub fn paste_marker(id: u32, text: &str) -> String {
    let lines = text.split('\n').count();
    if lines > 10 {
        format!("[paste #{id} +{lines} lines]")
    } else {
        format!("[paste #{id} {} chars]", utf16_len(text))
    }
}

/// Whether a segment is a whole paste marker (one merged by [`segment_with_markers`]).
pub fn is_paste_marker(segment: &str) -> bool {
    segment.len() >= 10 && marker_single_regex().is_match(segment)
}

/// The id of a whole paste marker.
pub fn paste_marker_id(segment: &str) -> Option<u32> {
    marker_single_regex()
        .captures(segment)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

/// Every marker in `text`: (start, end, id, suffix like " +12 lines").
pub fn find_markers(text: &str) -> Vec<(usize, usize, u32, String)> {
    marker_regex()
        .captures_iter(text)
        .filter_map(|c| {
            let m = c.get(0)?;
            let id = c.get(1)?.as_str().parse().ok()?;
            let suffix = c.get(2).map(|s| s.as_str().to_string()).unwrap_or_default();
            Some((m.start(), m.end(), id, suffix))
        })
        .collect()
}

/// Rewrite every marker in `text` with `f(whole, id, suffix)`.
pub fn replace_markers(text: &str, mut f: impl FnMut(&str, u32, &str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, end, id, suffix) in find_markers(text) {
        out.push_str(&text[last..start]);
        out.push_str(&f(&text[start..end], id, &suffix));
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

/// Segments of `text` with each marker of a live paste (its id in `valid`) as one atomic segment, so
/// the cursor, deletion and wrapping treat it as a single unit.
pub fn segment_with_markers<'a>(text: &'a str, mode: SegMode, valid: &BTreeSet<u32>) -> Vec<Seg<'a>> {
    if valid.is_empty() || !text.contains("[paste #") {
        return base_segments(text, mode);
    }
    let markers: Vec<(usize, usize)> = find_markers(text)
        .into_iter()
        .filter(|(_, _, id, _)| valid.contains(id))
        .map(|(s, e, _, _)| (s, e))
        .collect();
    if markers.is_empty() {
        return base_segments(text, mode);
    }
    let mut out = Vec::new();
    let mut k = 0;
    for seg in base_segments(text, mode) {
        while k < markers.len() && markers[k].1 <= seg.index {
            k += 1;
        }
        match markers.get(k) {
            Some(&(start, end)) if seg.index >= start && seg.index < end => {
                if seg.index == start {
                    out.push(Seg {
                        text: &text[start..end],
                        index: start,
                        word_like: false,
                    });
                }
            }
            _ => out.push(seg),
        }
    }
    out
}

/// Where alt+left / ctrl+w go from `cursor`: back over spaces, then over a word (stopping after its
/// last punctuation) or a run of punctuation (pi-tui `findWordBackward`).
pub fn find_word_backward(
    text: &str,
    cursor: usize,
    segs: &dyn Fn(&str) -> Vec<Seg<'_>>,
    atomic: &dyn Fn(&str) -> bool,
) -> usize {
    if cursor == 0 {
        return 0;
    }
    let mut segments = segs(&text[..cursor]);
    let mut pos = cursor;
    while let Some(last) = segments.last() {
        if atomic(last.text) || !has_whitespace(last.text) {
            break;
        }
        pos -= last.text.len();
        segments.pop();
    }
    let Some(last) = segments.last().copied() else {
        return pos;
    };
    if atomic(last.text) {
        pos -= last.text.len();
    } else if last.word_like {
        match last.text.char_indices().rev().find(|(_, c)| is_word_punctuation(*c)) {
            None => pos -= last.text.len(),
            Some((i, c)) => pos -= last.text.len() - (i + c.len_utf8()),
        }
    } else {
        while let Some(last) = segments.last() {
            if atomic(last.text) || last.word_like || has_whitespace(last.text) {
                break;
            }
            pos -= last.text.len();
            segments.pop();
        }
    }
    pos
}

/// Where alt+right / alt+d go from `cursor` (pi-tui `findWordForward`).
pub fn find_word_forward(
    text: &str,
    cursor: usize,
    segs: &dyn Fn(&str) -> Vec<Seg<'_>>,
    atomic: &dyn Fn(&str) -> bool,
) -> usize {
    if cursor >= text.len() {
        return text.len();
    }
    let segments = segs(&text[cursor..]);
    let mut it = segments.iter().peekable();
    let mut pos = cursor;
    while let Some(s) = it.peek() {
        if atomic(s.text) || !has_whitespace(s.text) {
            break;
        }
        pos += s.text.len();
        it.next();
    }
    let Some(s) = it.peek().copied() else { return pos };
    if atomic(s.text) {
        pos += s.text.len();
    } else if s.word_like {
        pos += s
            .text
            .char_indices()
            .find(|(_, c)| is_word_punctuation(*c))
            .map(|(i, _)| i)
            .unwrap_or(s.text.len());
    } else {
        while let Some(s) = it.peek() {
            if atomic(s.text) || s.word_like || has_whitespace(s.text) {
                break;
            }
            pos += s.text.len();
            it.next();
        }
    }
    pos
}

/// Plain word segments with no atomic pieces.
pub fn word_segments(text: &str) -> Vec<Seg<'_>> {
    base_segments(text, SegMode::Word)
}

/// The previous grapheme boundary before `cursor` in `text`.
pub fn prev_grapheme(text: &str, cursor: usize) -> usize {
    graphemes(&text[..cursor]).last().map_or(0, |g| cursor - g.len())
}

/// The next grapheme boundary after `cursor` in `text`.
pub fn next_grapheme(text: &str, cursor: usize) -> usize {
    graphemes(&text[cursor..])
        .next()
        .map_or(text.len(), |g| cursor + g.len())
}

/// A piece of a wrapped editor line: the text and its byte range in the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// The editor's own wrap (pi-tui `wordWrapLine`): break after the last whitespace that fits, or
/// between CJK characters, else mid-word; trailing spaces stay on the line they end.
pub fn word_wrap_line(line: &str, max_width: usize, segments: Option<Vec<Seg<'_>>>) -> Vec<Chunk> {
    if line.is_empty() || max_width == 0 {
        return vec![Chunk {
            text: String::new(),
            start: 0,
            end: 0,
        }];
    }
    if visible_width(line) <= max_width {
        return vec![Chunk {
            text: line.to_string(),
            start: 0,
            end: line.len(),
        }];
    }
    let segs = segments.unwrap_or_else(|| base_segments(line, SegMode::Grapheme));
    let mut chunks = Vec::new();
    let mut current_width = 0usize;
    let mut chunk_start = 0usize;
    let mut wrap_opp: Option<usize> = None;
    let mut wrap_opp_width = 0usize;
    for (i, seg) in segs.iter().enumerate() {
        let g = seg.text;
        let g_width = visible_width(g);
        let index = seg.index;
        let is_ws = !is_paste_marker(g) && has_whitespace(g);
        if current_width + g_width > max_width {
            match wrap_opp {
                Some(opp) if current_width - wrap_opp_width + g_width <= max_width => {
                    chunks.push(Chunk {
                        text: line[chunk_start..opp].to_string(),
                        start: chunk_start,
                        end: opp,
                    });
                    chunk_start = opp;
                    current_width -= wrap_opp_width;
                }
                _ if chunk_start < index => {
                    chunks.push(Chunk {
                        text: line[chunk_start..index].to_string(),
                        start: chunk_start,
                        end: index,
                    });
                    chunk_start = index;
                    current_width = 0;
                }
                _ => {}
            }
            wrap_opp = None;
        }
        if g_width > max_width {
            let sub = word_wrap_line(g, max_width, None);
            for sc in &sub[..sub.len() - 1] {
                chunks.push(Chunk {
                    text: sc.text.clone(),
                    start: index + sc.start,
                    end: index + sc.end,
                });
            }
            let last = &sub[sub.len() - 1];
            chunk_start = index + last.start;
            current_width = visible_width(&last.text);
            wrap_opp = None;
            continue;
        }
        current_width += g_width;
        if let Some(next) = segs.get(i + 1) {
            if is_ws && (is_paste_marker(next.text) || !has_whitespace(next.text)) {
                wrap_opp = Some(next.index);
                wrap_opp_width = current_width;
            } else if !is_ws && !has_whitespace(next.text) {
                let cjk = !is_paste_marker(g) && g.chars().any(is_cjk_break);
                let next_cjk = !is_paste_marker(next.text) && next.text.chars().any(is_cjk_break);
                if cjk || next_cjk {
                    wrap_opp = Some(next.index);
                    wrap_opp_width = current_width;
                }
            }
        }
    }
    chunks.push(Chunk {
        text: line[chunk_start..].to_string(),
        start: chunk_start,
        end: line.len(),
    });
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(t: &str) -> Vec<Seg<'_>> {
        word_segments(t)
    }
    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn word_movement_stops_at_punctuation_inside_words() {
        assert_eq!(find_word_backward("hello world", 11, &segs, &none), 6);
        assert_eq!(find_word_backward("hello world  ", 13, &segs, &none), 6);
        assert_eq!(find_word_backward("foo.bar", 7, &segs, &none), 4);
        assert_eq!(find_word_forward("hello world", 0, &segs, &none), 5);
        assert_eq!(find_word_forward("hello world", 5, &segs, &none), 11);
        assert_eq!(find_word_forward("a+++b", 1, &segs, &none), 4);
    }

    #[test]
    fn paste_markers_count_lines_past_ten_and_utf16_units_below() {
        assert_eq!(paste_marker(1, &"x\n".repeat(11)), "[paste #1 +12 lines]");
        assert_eq!(paste_marker(2, "😀ab"), "[paste #2 4 chars]");
        assert!(is_paste_marker("[paste #1 +12 lines]"));
        assert!(!is_paste_marker("[paste #1 +12 lines] "));
    }

    #[test]
    fn only_live_markers_are_atomic() {
        let text = "a[paste #1 3 chars]b[paste #2 3 chars]";
        let valid: BTreeSet<u32> = [1].into_iter().collect();
        let s = segment_with_markers(text, SegMode::Grapheme, &valid);
        assert_eq!(s[1].text, "[paste #1 3 chars]");
        assert_eq!(s[2].text, "b");
        assert_eq!(s[3].text, "[");
    }
}
