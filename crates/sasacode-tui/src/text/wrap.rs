//! Wrapping, truncating and slicing ANSI-styled lines by cell (pi-tui `wrapTextWithAnsi`,
//! `truncateToWidth`, `sliceWithWidth`, `extractSegments`).

use super::ansi::{AnsiCodeTracker, active_osc8_close};
use super::width::{grapheme_width, is_cjk_break, js_trim, js_trim_end, visible_width};
use super::{Piece, graphemes, pieces};

/// Split into tokens for wrapping: runs of spaces, runs of other text, and each CJK grapheme on its
/// own (a line may break between any two). Escape sequences stick to the next visible text.
fn split_into_tokens(text: &str) -> Vec<String> {
    #[derive(PartialEq, Clone, Copy)]
    enum Kind {
        Space,
        Word,
    }
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut kind: Option<Kind> = None;
    let mut pending = String::new();
    for p in pieces(text) {
        match p {
            Piece::Ansi(code) => pending.push_str(code),
            Piece::Text(t) => {
                for g in graphemes(t) {
                    let is_space = g == " ";
                    if !is_space && g.chars().any(is_cjk_break) {
                        if !current.is_empty() {
                            tokens.push(std::mem::take(&mut current));
                            kind = None;
                        }
                        tokens.push(std::mem::take(&mut pending) + g);
                        continue;
                    }
                    let k = if is_space { Kind::Space } else { Kind::Word };
                    if !current.is_empty() && kind != Some(k) {
                        tokens.push(std::mem::take(&mut current));
                    }
                    current.push_str(&pending);
                    pending.clear();
                    kind = Some(k);
                    current.push_str(g);
                }
            }
        }
    }
    if !pending.is_empty() {
        if !current.is_empty() {
            current.push_str(&pending);
        } else if let Some(last) = tokens.last_mut() {
            last.push_str(&pending);
        } else {
            current = pending;
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// A token wider than the line, broken by grapheme.
fn break_long_word(word: &str, width: usize, tracker: &mut AnsiCodeTracker) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = tracker.active_codes();
    let mut line_width = 0;
    for p in pieces(word) {
        match p {
            Piece::Ansi(code) => {
                line.push_str(code);
                tracker.process(code);
            }
            Piece::Text(t) => {
                for g in graphemes(t) {
                    let w = visible_width(g);
                    if line_width + w > width {
                        line.push_str(&tracker.line_end_reset());
                        lines.push(std::mem::replace(&mut line, tracker.active_codes()));
                        line_width = 0;
                    }
                    line.push_str(g);
                    line_width += w;
                }
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn wrap_single_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    if visible_width(line) <= width {
        return vec![line.to_string()];
    }
    let mut wrapped: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::new();
    let mut current = String::new();
    let mut current_width = 0;
    for token in split_into_tokens(line) {
        let token_width = visible_width(&token);
        let is_whitespace = js_trim(&token).is_empty();
        if token_width > width && !is_whitespace {
            if !current.is_empty() {
                current.push_str(&tracker.line_end_reset());
                wrapped.push(std::mem::take(&mut current));
            }
            let mut broken = break_long_word(&token, width, &mut tracker);
            current = broken.pop().unwrap_or_default();
            wrapped.extend(broken);
            current_width = visible_width(&current);
            continue;
        }
        if current_width + token_width > width && current_width > 0 {
            let mut to_wrap = js_trim_end(&current).to_string();
            to_wrap.push_str(&tracker.line_end_reset());
            wrapped.push(to_wrap);
            if is_whitespace {
                current = tracker.active_codes();
                current_width = 0;
            } else {
                current = tracker.active_codes() + &token;
                current_width = token_width;
            }
        } else {
            current.push_str(&token);
            current_width += token_width;
        }
        tracker.update_from_text(&token);
    }
    if !current.is_empty() {
        wrapped.push(current);
    }
    if wrapped.is_empty() {
        return vec![String::new()];
    }
    wrapped.into_iter().map(|l| js_trim_end(&l).to_string()).collect()
}

/// Word-wrap `text` to `width` cells. Lines split on `\r\n`, `\r` or `\n`; the style and link in
/// effect carry over to each next line. No padding.
pub fn wrap_text_with_ansi(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut result: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::new();
    for input_line in split_lines(text) {
        let prefix = if result.is_empty() {
            String::new()
        } else {
            tracker.active_codes()
        };
        result.extend(wrap_single_line(&(prefix + input_line), width));
        tracker.update_from_text(input_line);
    }
    if result.is_empty() {
        result.push(String::new());
    }
    result
}

/// JS `text.split(/\r\n|\r|\n/)`.
pub fn split_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\r' => {
                out.push(&text[start..i]);
                i += if b.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
                start = i;
            }
            b'\n' => {
                out.push(&text[start..i]);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    out.push(&text[start..]);
    out
}

/// `line` padded with spaces to `width` and wrapped in `bg`.
pub fn apply_background_to_line(line: &str, width: usize, bg: impl Fn(&str) -> String) -> String {
    let pad = width.saturating_sub(visible_width(line));
    bg(&format!("{line}{}", " ".repeat(pad)))
}

fn is_printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

/// The longest prefix of `text` that fits `max` cells, with its width (pi-tui
/// `truncateFragmentToWidth`, used for an ellipsis wider than the room).
fn truncate_fragment(text: &str, max: usize) -> (String, usize) {
    if max == 0 || text.is_empty() {
        return (String::new(), 0);
    }
    if is_printable_ascii(text) {
        let n = text.len().min(max);
        return (text[..n].to_string(), n);
    }
    let mut out = String::new();
    let mut width = 0;
    let mut pending = String::new();
    for p in pieces(text) {
        match p {
            Piece::Ansi(code) => pending.push_str(code),
            Piece::Text(t) => {
                for g in graphemes(t) {
                    let w = grapheme_width(g);
                    if width + w > max {
                        return (out, width);
                    }
                    out.push_str(&std::mem::take(&mut pending));
                    out.push_str(g);
                    width += w;
                }
            }
        }
    }
    (out, width)
}

fn finalize_truncated(
    prefix: &str,
    prefix_width: usize,
    ellipsis: &str,
    ellipsis_width: usize,
    max: usize,
    pad: bool,
) -> String {
    let close = active_osc8_close(prefix);
    let mut out = if ellipsis.is_empty() {
        format!("{prefix}{close}\x1b[0m")
    } else {
        format!("{prefix}{close}\x1b[0m{ellipsis}\x1b[0m")
    };
    if pad {
        out.push_str(&" ".repeat(max.saturating_sub(prefix_width + ellipsis_width)));
    }
    out
}

/// `text` cut to `max` cells with `ellipsis` at the end when it did not fit; with `pad`, spaces
/// fill it to exactly `max`.
pub fn truncate_to_width(text: &str, max: usize, ellipsis: &str, pad: bool) -> String {
    if max == 0 {
        return String::new();
    }
    if text.is_empty() {
        return if pad { " ".repeat(max) } else { String::new() };
    }
    let ellipsis_width = visible_width(ellipsis);
    if ellipsis_width >= max {
        let text_width = visible_width(text);
        if text_width <= max {
            return if pad {
                format!("{text}{}", " ".repeat(max - text_width))
            } else {
                text.to_string()
            };
        }
        let (clipped, clipped_width) = truncate_fragment(ellipsis, max);
        if clipped_width == 0 {
            return if pad { " ".repeat(max) } else { String::new() };
        }
        return finalize_truncated("", 0, &clipped, clipped_width, max, pad);
    }
    if is_printable_ascii(text) {
        if text.len() <= max {
            return if pad {
                format!("{text}{}", " ".repeat(max - text.len()))
            } else {
                text.to_string()
            };
        }
        let target = max - ellipsis_width;
        return finalize_truncated(&text[..target], target, ellipsis, ellipsis_width, max, pad);
    }
    let target = max - ellipsis_width;
    let mut result = String::new();
    let mut pending = String::new();
    let mut seen = 0;
    let mut kept = 0;
    let mut contiguous = true;
    let mut overflowed = false;
    'outer: for p in pieces(text) {
        match p {
            Piece::Ansi(code) => pending.push_str(code),
            Piece::Text(t) => {
                for g in graphemes(t) {
                    let w = grapheme_width(g);
                    if contiguous && kept + w <= target {
                        result.push_str(&std::mem::take(&mut pending));
                        result.push_str(g);
                        kept += w;
                    } else {
                        contiguous = false;
                        pending.clear();
                    }
                    seen += w;
                    if seen > max {
                        overflowed = true;
                        break 'outer;
                    }
                }
            }
        }
    }
    if !overflowed {
        return if pad {
            format!("{text}{}", " ".repeat(max.saturating_sub(seen)))
        } else {
            text.to_string()
        };
    }
    finalize_truncated(&result, kept, ellipsis, ellipsis_width, max, pad)
}

/// Columns `start..start+len` of `line`. With `strict`, a wide char that would cross the end is
/// left out.
pub fn slice_by_column(line: &str, start: usize, len: usize, strict: bool) -> String {
    slice_with_width(line, start, len, strict).0
}

/// [`slice_by_column`] with the width of what it returns.
pub fn slice_with_width(line: &str, start: usize, len: usize, strict: bool) -> (String, usize) {
    if len == 0 {
        return (String::new(), 0);
    }
    let end = start + len;
    let mut out = String::new();
    let mut out_width = 0;
    let mut col = 0;
    let mut pending = String::new();
    for p in pieces(line) {
        match p {
            Piece::Ansi(code) => {
                if col >= start && col < end {
                    out.push_str(code);
                } else if col < start {
                    pending.push_str(code);
                }
            }
            Piece::Text(t) => {
                for g in graphemes(t) {
                    let w = grapheme_width(g);
                    let in_range = col >= start && col < end;
                    let fits = !strict || col + w <= end;
                    if in_range && fits {
                        out.push_str(&std::mem::take(&mut pending));
                        out.push_str(g);
                        out_width += w;
                    }
                    col += w;
                    if col >= end {
                        break;
                    }
                }
                if col >= end {
                    break;
                }
            }
        }
    }
    (out, out_width)
}

/// The cells taken by the grapheme at `column`, if any (a wide char's second cell included).
pub fn get_grapheme_cell_range(line: &str, column: usize) -> Option<(usize, usize)> {
    let mut col = 0;
    for p in pieces(line) {
        if let Piece::Text(t) = p {
            for g in graphemes(t) {
                let w = grapheme_width(g);
                if w > 0 && column >= col && column < col + w {
                    return Some((col, col + w));
                }
                col += w;
            }
        }
    }
    None
}

/// What overlay compositing keeps of a line: the text before column `before_end` and the text in
/// `after_start..after_start+after_len`, the latter reopened with the style in effect where it
/// starts. Returns (before, before width, after, after width).
pub fn extract_segments(
    line: &str,
    before_end: usize,
    after_start: usize,
    after_len: usize,
    strict_after: bool,
) -> (String, usize, String, usize) {
    let mut before = String::new();
    let mut before_width = 0;
    let mut after = String::new();
    let mut after_width = 0;
    let mut col = 0;
    let mut pending_before = String::new();
    let mut after_started = false;
    let after_end = after_start + after_len;
    let mut tracker = AnsiCodeTracker::new();
    let done = |col: usize| {
        if after_len == 0 {
            col >= before_end
        } else {
            col >= after_end
        }
    };
    'outer: for p in pieces(line) {
        match p {
            Piece::Ansi(code) => {
                tracker.process(code);
                if col < before_end {
                    pending_before.push_str(code);
                } else if col >= after_start && col < after_end && after_started {
                    after.push_str(code);
                }
            }
            Piece::Text(t) => {
                for g in graphemes(t) {
                    let w = grapheme_width(g);
                    if col < before_end && col + w <= before_end {
                        before.push_str(&std::mem::take(&mut pending_before));
                        before.push_str(g);
                        before_width += w;
                    } else if col >= after_start && col < after_end {
                        let fits = !strict_after || col + w <= after_end;
                        if fits {
                            if !after_started {
                                after.push_str(&tracker.active_codes());
                                after_started = true;
                            }
                            after.push_str(g);
                            after_width += w;
                        }
                    }
                    col += w;
                    if done(col) {
                        break 'outer;
                    }
                }
            }
        }
    }
    (before, before_width, after, after_width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_lines_like_the_js_regex() {
        assert_eq!(split_lines("a\r\nb\rc\nd"), vec!["a", "b", "c", "d"]);
        assert_eq!(split_lines(""), vec![""]);
        assert_eq!(split_lines("a\n"), vec!["a", ""]);
    }
}
