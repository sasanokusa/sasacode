//! The lines a change shows: from the diff a finished `edit` or `write` reports, or, where there is
//! none (a call that waits for approval, a session read back from its log), from the call's own
//! arguments.

/// A change longer than this is cut: every line is a widget.
const MAX_LINES: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sign {
    Add,
    Del,
    Context,
    /// A `@@ -7 +7 @@` line.
    Hunk,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub sign: Sign,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diff {
    pub lines: Vec<Line>,
    pub added: usize,
    pub removed: usize,
    /// Lines left out of `lines` because the change is long. `added` and `removed` count them too.
    pub cut: usize,
}

impl Diff {
    /// The unified hunks in `details.diff`.
    pub fn parse(unified: &str) -> Self {
        let mut d = Diff::default();
        for raw in unified.lines() {
            let (sign, text) = if raw.starts_with("@@") {
                (Sign::Hunk, raw)
            } else if let Some(rest) = raw.strip_prefix('+') {
                (Sign::Add, rest)
            } else if let Some(rest) = raw.strip_prefix('-') {
                (Sign::Del, rest)
            } else {
                (Sign::Context, raw.strip_prefix(' ').unwrap_or(raw))
            };
            d.push(sign, text);
        }
        d
    }

    /// What an `edit` call asks for: its old text out, its new text in. Lines both share at the
    /// start and the end are shown once, as context.
    pub fn replace(old: &str, new: &str) -> Self {
        let (old, new): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
        let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let tail = old[head..]
            .iter()
            .rev()
            .zip(new[head..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let mut d = Diff::default();
        for line in &old[..head] {
            d.push(Sign::Context, line);
        }
        for line in &old[head..old.len() - tail] {
            d.push(Sign::Del, line);
        }
        for line in &new[head..new.len() - tail] {
            d.push(Sign::Add, line);
        }
        for line in &old[old.len() - tail..] {
            d.push(Sign::Context, line);
        }
        d
    }

    /// A file that did not exist: every line is new.
    pub fn create(content: &str) -> Self {
        let mut d = Diff::default();
        for line in content.lines() {
            d.push(Sign::Add, line);
        }
        d
    }

    fn push(&mut self, sign: Sign, text: &str) {
        match sign {
            Sign::Add => self.added += 1,
            Sign::Del => self.removed += 1,
            _ => {}
        }
        if self.lines.len() < MAX_LINES {
            self.lines.push(Line {
                sign,
                text: text.to_owned(),
            });
        } else {
            self.cut += 1;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signs(d: &Diff) -> String {
        d.lines
            .iter()
            .map(|l| match l.sign {
                Sign::Add => '+',
                Sign::Del => '-',
                Sign::Context => ' ',
                Sign::Hunk => '@',
            })
            .collect()
    }

    #[test]
    fn a_reported_diff_keeps_its_hunks_and_drops_the_sign_column() {
        let d = Diff::parse("@@ -7 +7 @@\n   if (a) {\n-    old();\n+    new();\n   }");
        assert_eq!(signs(&d), "@ -+ ");
        assert_eq!((d.added, d.removed), (1, 1));
        assert_eq!(d.lines[1].text, "  if (a) {");
        assert_eq!(d.lines[3].text, "    new();");
    }

    #[test]
    fn an_edit_from_its_arguments_shows_shared_lines_once() {
        let d = Diff::replace("a\nb\nc\nd", "a\nB\nB2\nd");
        assert_eq!(signs(&d), " --++ ");
        assert_eq!((d.added, d.removed), (2, 2));
        let same = Diff::replace("a", "a");
        assert_eq!((signs(&same).as_str(), same.added, same.removed), (" ", 0, 0));
        let grown = Diff::replace("a", "a\na");
        assert_eq!(signs(&grown), " +");
    }

    #[test]
    fn a_long_new_file_is_cut_but_counted_whole() {
        let d = Diff::create(&"x\n".repeat(MAX_LINES + 25));
        assert_eq!((d.lines.len(), d.cut, d.added), (MAX_LINES, 25, MAX_LINES + 25));
    }
}
