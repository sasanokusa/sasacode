//! Just enough of Rust's lexical grammar to read sources as text: where the string literals and the
//! comments are, and the value of a string literal. The checks read the crates this way instead of
//! depending on them, so they need no build of what they look at.

/// What a character of a Rust source is part of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Code,
    /// A string literal, quotes (and an `r#` prefix) included.
    Literal,
    /// A `//` or `/* */` comment.
    Comment,
}

/// The class of every character of `source`, in order. Char literals (`'"'`) and lifetimes are code;
/// a quote inside a comment or a char literal starts nothing.
pub fn classify(source: &str) -> Vec<(char, Class)> {
    #[derive(Clone, Copy)]
    enum State {
        Code,
        Str,
        Raw(usize),
        Line,
        Block(usize),
    }
    let chars: Vec<char> = source.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let mut out = Vec::with_capacity(chars.len());
    let mut state = State::Code;
    let mut i = 0;
    let push = |out: &mut Vec<(char, Class)>, from: usize, to: usize, class: Class| {
        for &c in &chars[from..to.min(chars.len())] {
            out.push((c, class));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        match state {
            State::Code => {
                if c == '/' && at(i + 1) == Some('/') {
                    state = State::Line;
                    continue;
                } else if c == '/' && at(i + 1) == Some('*') {
                    push(&mut out, i, i + 2, Class::Comment);
                    i += 2;
                    state = State::Block(1);
                    continue;
                } else if c == '"' {
                    push(&mut out, i, i + 1, Class::Literal);
                    i += 1;
                    state = State::Str;
                    continue;
                } else if let Some(hashes) = raw_string_start(&chars, i) {
                    push(&mut out, i, i + hashes + 2, Class::Literal);
                    i += hashes + 2;
                    state = State::Raw(hashes);
                    continue;
                } else if c == '\'' {
                    let len = char_literal_len(&chars, i);
                    push(&mut out, i, i + len, Class::Code);
                    i += len;
                    continue;
                }
                push(&mut out, i, i + 1, Class::Code);
                i += 1;
            }
            State::Str => {
                let len = match c {
                    '\\' => 2,
                    '"' => {
                        state = State::Code;
                        1
                    }
                    _ => 1,
                };
                push(&mut out, i, i + len, Class::Literal);
                i += len;
            }
            State::Raw(hashes) => {
                let closes = c == '"' && (1..=hashes).all(|k| at(i + k) == Some('#'));
                let len = if closes { hashes + 1 } else { 1 };
                if closes {
                    state = State::Code;
                }
                push(&mut out, i, i + len, Class::Literal);
                i += len;
            }
            State::Line => {
                if c == '\n' {
                    state = State::Code;
                    push(&mut out, i, i + 1, Class::Code);
                } else {
                    push(&mut out, i, i + 1, Class::Comment);
                }
                i += 1;
            }
            State::Block(depth) => {
                let two = (c, at(i + 1));
                let (len, next) = match two {
                    ('*', Some('/')) if depth == 1 => (2, State::Code),
                    ('*', Some('/')) => (2, State::Block(depth - 1)),
                    ('/', Some('*')) => (2, State::Block(depth + 1)),
                    _ => (1, state),
                };
                push(&mut out, i, i + len, Class::Comment);
                state = next;
                i += len;
            }
        }
    }
    out
}

/// At `r"`, `r#"`, `br"`…: the number of `#`s, when `chars[i]` starts a raw string literal.
fn raw_string_start(chars: &[char], i: usize) -> Option<usize> {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    if chars[i] != 'r' {
        return None;
    }
    // `r` must begin a token: nothing of an identifier before it, or just the `b`/`c` prefix.
    let before = i.checked_sub(1).map(|j| chars[j]);
    let starts_token = match before {
        None => true,
        Some('b' | 'c') => i < 2 || !ident(chars[i - 2]),
        Some(p) => !ident(p),
    };
    if !starts_token {
        return None;
    }
    let hashes = chars[i + 1..].iter().take_while(|c| **c == '#').count();
    (chars.get(i + 1 + hashes) == Some(&'"')).then_some(hashes)
}

/// At a `'`: the length of the char literal it starts (`'x'`, `'\''`, `'\u{1F600}'`), or 1 for a
/// lifetime or a label.
fn char_literal_len(chars: &[char], i: usize) -> usize {
    match chars.get(i + 1) {
        Some('\\') => {
            let mut j = i + 3;
            while j < chars.len() && chars[j] != '\'' && chars[j] != '\n' {
                j += 1;
            }
            j + 1 - i
        }
        Some(_) if chars.get(i + 2) == Some(&'\'') => 3,
        _ => 1,
    }
}

/// For each line of `source`, the character ranges `[start, end)` its string literals take up. A
/// literal that runs over several lines gives a range on each of them.
pub fn literal_spans(source: &str) -> Vec<Vec<(usize, usize)>> {
    let mut lines = Vec::new();
    let mut line = Vec::new();
    let mut column = 0;
    let mut open: Option<usize> = None;
    for (c, class) in classify(source) {
        if c == '\n' {
            if let Some(start) = open.take() {
                line.push((start, column));
            }
            lines.push(std::mem::take(&mut line));
            column = 0;
            // A literal still open goes on at the start of the next line.
            if class == Class::Literal {
                open = Some(0);
            }
            continue;
        }
        match (class == Class::Literal, open) {
            (true, None) => open = Some(column),
            (false, Some(start)) => {
                line.push((start, column));
                open = None;
            }
            _ => {}
        }
        column += 1;
    }
    if let Some(start) = open {
        line.push((start, column));
    }
    lines.push(line);
    lines
}

/// `source` with every comment blanked out (newlines kept), so a commented-out entry is not read.
pub fn without_comments(source: &str) -> String {
    classify(source)
        .into_iter()
        .map(|(c, class)| if class == Class::Comment { ' ' } else { c })
        .collect()
}

/// The value of the string literal `text` starts with (`"…"` with escapes, or `r#"…"#`).
pub fn string_value(text: &str) -> Option<String> {
    if let Some(raw) = text.strip_prefix('r') {
        let hashes = raw.chars().take_while(|c| *c == '#').count();
        let body = raw[hashes..].strip_prefix('"')?;
        let end = body.find(&format!("\"{}", "#".repeat(hashes)))?;
        return Some(body[..end].to_string());
    }
    let mut chars = text.strip_prefix('"')?.chars().peekable();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '0' => out.push('\0'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                // A backslash at the end of a line joins it to the next, without its indentation.
                '\r' | '\n' => {
                    while chars.peek().is_some_and(|c| c.is_whitespace()) {
                        chars.next();
                    }
                }
                'x' => {
                    let hex: String = chars.by_ref().take(2).collect();
                    out.push(char::from(u8::from_str_radix(&hex, 16).ok()?));
                }
                'u' => {
                    if chars.next()? != '{' {
                        return None;
                    }
                    let hex: String = chars.by_ref().take_while(|c| *c != '}').filter(|c| *c != '_').collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                _ => return None,
            },
            // A CRLF checkout: the literal's value has the LF only.
            '\r' if chars.peek() == Some(&'\n') => {}
            c => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(source: &str) -> Vec<Vec<(usize, usize)>> {
        literal_spans(source)
    }

    #[test]
    fn literals_comments_and_char_literals_are_told_apart() {
        assert_eq!(spans(r#"let a = "x"; // "not a literal""#), vec![vec![(8, 11)]]);
        assert_eq!(spans(r#"if c == '"' { f("ab") }"#), vec![vec![(16, 20)]]);
        assert_eq!(
            spans(r#"fn f<'a>(x: &'a str) -> &'a str { "q" }"#),
            vec![vec![(34, 37)]]
        );
        assert_eq!(spans(r#"let e = "a\"b"; let f = '\'';"#), vec![vec![(8, 14)]]);
        assert_eq!(
            spans("/* \"no\" */ let s = r#\"a \"quoted\" b\"#;"),
            vec![vec![(19, 36)]]
        );
        assert_eq!(spans(r#"let v = br"x"; let w = for_r"#), vec![vec![(9, 13)]]);
    }

    #[test]
    fn a_literal_over_several_lines_has_a_range_on_each() {
        let source = "let s = \"one\n  two\nthree\"; x\nnext";
        assert_eq!(spans(source), vec![vec![(8, 12)], vec![(0, 5)], vec![(0, 6)], vec![]]);
        let raw = "let s = r#\"a\n\"#;";
        assert_eq!(spans(raw), vec![vec![(8, 12)], vec![(0, 2)]]);
    }

    #[test]
    fn comments_are_blanked_and_nested_block_comments_close_at_the_end() {
        assert_eq!(
            without_comments("a /* b /* c */ d */ e // f\ng"),
            "a                   e     \ng"
        );
        assert_eq!(without_comments("let s = \"// kept\";"), "let s = \"// kept\";");
    }

    #[test]
    fn string_values_are_unescaped() {
        assert_eq!(
            string_value(r#""a \"b\" \\ c\n" rest"#).as_deref(),
            Some("a \"b\" \\ c\n")
        );
        assert_eq!(string_value("\"one \\\n    two\"").as_deref(), Some("one two"));
        assert_eq!(
            string_value(r##"r#"raw "x" \n"# rest"##).as_deref(),
            Some(r#"raw "x" \n"#)
        );
        assert_eq!(string_value(r#""\u{1F600}\x41""#).as_deref(), Some("😀A"));
        assert_eq!(string_value("\"unterminated"), None);
        assert_eq!(string_value("not a literal"), None);
    }
}
