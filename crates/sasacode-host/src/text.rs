//! Text from config files, models, tools or servers, made safe to print on a terminal.

/// The terminal must not act on control sequences in the text (hidden text in an approval prompt, a
/// rewritten clipboard, a fake link), nor reorder it. ESC shows as ␛ so the attempt stays visible;
/// other controls (C0 but newline and tab, DEL, C1) and the bidirectional controls are dropped.
pub fn plain(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\x1b' => Some('␛'),
            '\n' | '\t' => Some(c),
            c if is_control(c) => None,
            c => Some(c),
        })
        .collect()
}

/// [`plain`] for text shown on one line of a list: a newline or tab shows as `\n` or `\t`, so a
/// value cannot start a line of its own.
pub fn plain_line(s: &str) -> String {
    plain(s).replace('\n', "\\n").replace('\t', "\\t")
}

/// [`plain_line`], cut to `max` characters with `…` when longer.
pub fn plain_clipped(s: &str, max: usize) -> String {
    let line = plain_line(s);
    if line.chars().count() <= max {
        return line;
    }
    let mut cut: String = line.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

fn is_control(c: char) -> bool {
    matches!(
        c,
        '\x00'..='\x1f'
            | '\x7f'..='\u{9f}'
            | '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_shows_and_other_controls_go() {
        assert_eq!(plain("a\x1b[2Kb\rc\x07d\x7fe\u{85}f"), "a␛[2Kbcdef");
        assert_eq!(plain("x\ny\tz"), "x\ny\tz");
    }

    #[test]
    fn bidirectional_controls_go() {
        assert_eq!(plain("a\u{202e}b\u{2066}c\u{2069}d\u{200f}e\u{061c}f"), "abcdef");
    }

    #[test]
    fn one_line_keeps_newlines_and_tabs_visible() {
        assert_eq!(plain_line("docs\n  - maxRetries 9\tx"), "docs\\n  - maxRetries 9\\tx");
    }

    #[test]
    fn clipped_ends_with_an_ellipsis() {
        assert_eq!(plain_clipped("abcdef", 4), "abc…");
        assert_eq!(plain_clipped("abcd", 4), "abcd");
    }
}
