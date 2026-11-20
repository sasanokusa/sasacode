//! Text from config files and models, made safe to show: the same as `sasacode-host`'s
//! `text::plain` (a test compares the two). ESC shows as ␛; other controls (C0 but newline and
//! tab, DEL, C1) and the bidirectional controls, which would reorder what is read, are dropped.

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

/// [`plain`] on one line: a newline or tab shows as `\n` or `\t`.
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

    /// The source of `fn <name>` in `text`, to its closing brace at the start of a line.
    fn source<'a>(text: &'a str, name: &str) -> &'a str {
        let at = text.find(&format!("fn {name}(")).expect("the function is there");
        let end = text[at..].find("\n}\n").expect("it ends") + at;
        &text[at..end]
    }

    #[test]
    fn it_is_what_the_cli_shows() {
        let host = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../sasacode-host/src/text.rs"));
        let ours = include_str!("plain.rs");
        for name in ["plain", "plain_line", "plain_clipped", "is_control"] {
            assert_eq!(source(ours, name), source(host, name), "{name}");
        }
    }

    #[test]
    fn controls_and_bidirectional_controls_go_and_esc_shows() {
        assert_eq!(plain("a\x1b[2Kb\rc\x07d\x7fe\u{85}f"), "a␛[2Kbcdef");
        assert_eq!(plain("a\u{202e}b\u{2066}c\u{2069}d\u{200f}e\u{061c}f"), "abcdef");
        assert_eq!(plain_line("x\ny\tz"), "x\\ny\\tz");
        assert_eq!(plain_clipped("abcdef", 4), "abc…");
    }
}
