//! The edit tool (`packages/tools/src/edit.ts`): exact replacement, and when old_string is not in
//! the file as given, the loose match of `loosely` (#65).
//!
//! The TS built regexes for the loose match; their shapes are fixed (literal lines joined by
//! `[ \t]*\r?\n`, optionally `^[ \t]*` before each line, `(?<![ \t])` before a blank first line),
//! and none of them can backtrack into a different match, so they are hand-coded here. `^` keeps
//! its JS multiline meaning: after `\n`, `\r`, U+2028 or U+2029.
//!
//! A failed edit says where to look: for an old_string not found, the file's lines most like it; for
//! one found in several places, the line each starts on.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use async_trait::async_trait;
use serde_json::{Value, json};

use sasacode_plugin_api::{JsonObject, Tool, ToolContext, ToolKind, ToolResult};

use crate::util::{fs_error, is_js_space, js_string_arg, line_diff, resolve_path, str_arg, truthy_arg};

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }
    fn description(&self) -> String {
        "Replace an exact string in a file. old_string must match the file exactly (including whitespace) and be unique unless replace_all is true.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, absolute or relative to the working directory" },
                "old_string": { "type": "string", "description": "Exact text to replace" },
                "new_string": { "type": "string", "description": "Replacement text" },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence (default false)" }
            },
            "required": ["path", "old_string", "new_string"],
            "additionalProperties": false
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }
    fn always_load(&self) -> bool {
        true
    }
    fn paths(&self, args: &JsonObject, cwd: &Path) -> Option<Vec<String>> {
        Some(vec![resolve_path(str_arg(args, "path"), cwd)])
    }
    fn summary(&self, args: &JsonObject) -> Option<String> {
        Some(js_string_arg(args, "path"))
    }
    async fn execute(&self, args: JsonObject, ctx: ToolContext) -> anyhow::Result<ToolResult> {
        let path = resolve_path(str_arg(&args, "path"), &ctx.cwd);
        let Ok(bytes) = tokio::fs::read(&path).await else {
            return Ok(ToolResult::error(format!(
                "File not found: {path}. Use write to create it."
            )));
        };
        // As UTF-8, Shift_JIS or Latin-1 bytes become U+FFFD: written back, lines the edit never
        // touched would be lost (#89).
        let Ok(before) = String::from_utf8(bytes) else {
            return Ok(ToolResult::error(format!(
                "{path} is not valid UTF-8 (e.g. Shift_JIS, EUC-JP or Latin-1); edit would corrupt the bytes it cannot decode, so nothing was changed. Edit it with bash instead (e.g. iconv to UTF-8 and back, or sed on its own encoding)."
            )));
        };
        let old = str_arg(&args, "old_string");
        let new = str_arg(&args, "new_string");
        let replace_all = truthy_arg(&args, "replace_all");
        if old.is_empty() {
            return Ok(ToolResult::error(
                "old_string is empty. Use write to replace the whole file.",
            ));
        }
        let count = before.matches(old).count();
        if count > 1 && !replace_all {
            let at = at_lines(&before, before.match_indices(old).map(|(i, _)| i), count > 10);
            return Ok(ToolResult::error(format!(
                "old_string appears {count} times in {path}{at}. Add surrounding context to make it unique, or set replace_all."
            )));
        }
        let (after, note) = if count == 0 {
            match loosely(&path, &before, old, new) {
                Ok(loose) => (loose.after, loose.note),
                Err(message) => return Ok(ToolResult::error(message)),
            }
        } else if replace_all {
            (before.replace(old, new), String::new())
        } else {
            (before.replacen(old, new, 1), String::new())
        };
        if after == before {
            return Ok(ToolResult::error(
                "old_string and new_string are identical, as matched in the file; nothing was changed.",
            ));
        }
        if ctx.signal.is_cancelled() {
            return Ok(ToolResult::error("Interrupted by the user; the file was not changed."));
        }
        tokio::fs::write(&path, &after)
            .await
            .map_err(|e| fs_error(&e, "open", &path))?;
        let diff = line_diff(&before, &after);
        let mut details = JsonObject::new();
        details.insert("path".into(), json!(path));
        details.insert("changed".into(), json!(true));
        details.insert("diff".into(), json!(diff));
        let plural = if count > 1 { "s" } else { "" };
        Ok(
            ToolResult::text(format!("{note}Edited {path} ({} replacement{plural}).", count.max(1)))
                .with_details(details),
        )
    }
}

struct Loose {
    after: String,
    note: String,
}

/// old_string is not in the file as given (#65): look again without read's "N\t" line numbers
/// (refused if a bare line of digits, a number without its tab or text, is among them), then
/// ignoring trailing whitespace and CR (a trailing CR or indentation-only last line is the next
/// line's), then indentation; one place only, overlaps counted. Each indentation in new_string
/// becomes the file's for that old_string indentation, else the one shift all lines share if two
/// depths of old_string or unmoved lines show it, else refused. `Err`: the message for the model.
fn loosely(path: &str, file: &str, old: &str, neu: &str) -> Result<Loose, String> {
    // read pads its numbers to 6 columns: a TSV row's "123\t" is the file's text, not a line number
    // (#84); nor is one past the file's end (#130).
    let file_lines = file.matches('\n').count() + 1;
    let numbered = !ln_matches(old).is_empty()
        && old
            .split('\n')
            .all(|l| is_blank(l) || ln_prefix(l).is_some() || is_digits_line(l))
        && ln_matches(old)
            .iter()
            .map(|&(s, e)| ln_value(&old[s..e]))
            .fold(0.0, f64::max)
            <= file_lines as f64;
    let bare = if numbered { strip_ln(old) } else { old.to_string() };
    if numbered {
        let both = format!("{old}\n{neu}");
        if let Some((index, digits)) = first_digits_line(&both) {
            let which = if index > old.len() { "new" } else { "old" };
            return Err(format!(
                "{which}_string's line {} is only digits among read's line numbers: a blank line's number without its tab, or text, so the line-number form is ambiguous there. Nothing was changed; give old_string and new_string as the file's exact text, without line numbers.",
                serde_json::to_string(digits).unwrap_or_default()
            ));
        }
    }
    let neu_bare = if numbered { strip_ln(neu) } else { neu.to_string() };
    // From here on, as in the TS after `[old, neu] = ...`: both without trailing [ \t\r].
    let old_s = trim_end_blank_cr(&bare);
    let neu_s = trim_end_blank_cr(&neu_bare);
    let mut i = 0;
    let mut hits = literal_hits(file, old_s);
    while hits.is_empty() && i < 2 {
        let lines: Vec<&str> = old_s
            .split('\n')
            .map(|l| {
                if i == 1 {
                    trim_end_blank_cr(l.trim_start_matches([' ', '\t']))
                } else {
                    trim_end_blank_cr(l)
                }
            })
            .collect();
        hits = loose_hits(file, &lines, i == 1);
        i += 1;
    }
    // What was ignored to find it, in the order tried: each later step also ignores what the
    // earlier ones did.
    let steps = ["trailing whitespace and line endings", "indentation"];
    let taken = if i > 0 { i } else { usize::from(old_s != bare) };
    let mut what: Vec<&str> = Vec::new();
    if numbered {
        what.push("line-number prefixes");
    }
    what.extend(&steps[..taken]);
    let ignoring = format!("ignoring {}", what.join(", "));
    if hits.is_empty() {
        return Err(format!(
            "old_string not found in {path}, even {ignoring}. Re-read the file and match it exactly.{}",
            nearest(file, old_s)
        ));
    }
    if hits.len() > 1 {
        let at = at_lines(file, hits.iter().map(|h| h.0), hits.len() > 10);
        return Err(format!(
            "old_string appears more than once in {path}, {ignoring}{at}. Add surrounding context to make it unique."
        ));
    }
    let (index, end) = hits[0];
    let at = &file[index..end];
    let fl: Vec<&str> = at.split('\n').collect();
    // Per non-blank line pair: old_string's indentation = s1 + s2, the matched line's = s3 + s2.
    let sh: Vec<Shift> = old_s
        .split('\n')
        .enumerate()
        .filter(|(_, l)| !is_blank(l))
        .map(|(k, l)| Shift::of(ws(l), ws(fl.get(k).copied().unwrap_or(""))))
        .collect();
    let deep = sh.iter().map(Shift::old).collect::<HashSet<_>>().len() > 1;
    let to = |w: &str| -> Vec<String> {
        let own: Vec<&Shift> = sh.iter().filter(|s| s.old() == w).collect();
        let pool: Vec<&Shift> = if own.is_empty() {
            sh.iter().collect()
        } else {
            own.clone()
        };
        let mut out: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        for s in pool {
            let t = if w.starts_with(&s.s1) && (!own.is_empty() || s.s1 == s.s3 || deep) {
                format!("{}{}", s.s3, &w[s.s1.len()..])
            } else {
                "\n".to_string()
            };
            if seen.insert(t.clone()) {
                out.push(t);
            }
        }
        out
    };
    // Once per indentation, not per line: both strings may have thousands of lines.
    let mut map: HashMap<&str, Vec<String>> = HashMap::new();
    for (s, e) in indents(neu_s) {
        let w = &neu_s[s..e];
        if !map.contains_key(w) {
            map.insert(w, to(w));
        }
    }
    if map.values().any(|t| t.len() > 1 || t.iter().any(|x| x == "\n")) {
        return Err(format!(
            "old_string matched {path} {ignoring}, but new_string's indentation does not map onto the file's; use the file's exact indentation."
        ));
    }
    let mut mapped = String::with_capacity(neu_s.len());
    let mut last = 0;
    for (s, e) in indents(neu_s) {
        let w = &neu_s[s..e];
        mapped.push_str(&neu_s[last..s]);
        mapped.push_str(map.get(w).and_then(|t| t.first()).map_or(w, String::as_str));
        last = e;
    }
    mapped.push_str(&neu_s[last..]);
    let eol = if file.contains("\r\n") { "\r\n" } else { "\n" };
    let replacement = mapped.replace("\r\n", "\n").replace('\n', eol);
    Ok(Loose {
        after: format!("{}{replacement}{}", &file[..index], &file[end..]),
        note: format!("[harness matched old_string {ignoring}]\n"),
    })
}

/// How many places of old_string are looked for: one more than the lines an error lists, to say "…".
const HITS: usize = 11;

/// " (lines 3, 17, …)": the 1-based lines where the first ten of `starts` (byte offsets) are.
fn at_lines(file: &str, starts: impl Iterator<Item = usize>, more: bool) -> String {
    let lines: Vec<String> = starts
        .take(HITS - 1)
        .map(|i| (file[..i].matches('\n').count() + 1).to_string())
        .collect();
    format!(" (lines {}{})", lines.join(", "), if more { ", …" } else { "" })
}

/// The file's lines most like old_string's first ten, with a line of context each side, numbered as
/// read numbers them (edit takes that form back): the window whose lines share the most character
/// pairs with old_string's, spaces aside. Nothing when the best shares under half.
fn nearest(file: &str, old: &str) -> String {
    let lines: Vec<&str> = file.split('\n').collect();
    let theirs: Vec<Vec<(char, char)>> = lines.iter().copied().map(pairs).collect();
    let ours: Vec<Vec<(char, char)>> = old.split('\n').take(HITS - 1).map(pairs).collect();
    let score = |w: usize| {
        let sum: f64 = ours.iter().zip(&theirs[w..]).map(|(a, b)| dice(a, b)).sum();
        sum / ours.len() as f64
    };
    // The first of the best: max_by would keep the last.
    let best = (0..lines.len())
        .map(|w| (w, score(w)))
        .fold((0, 0.0), |b, x| if x.1 > b.1 { x } else { b });
    if best.1 < 0.5 {
        return String::new();
    }
    let (from, to) = (best.0.saturating_sub(1), (best.0 + ours.len() + 1).min(lines.len()));
    let shown: String = (from..to)
        .map(|i| format!("\n{:>6}\t{}", i + 1, lines[i].trim_end_matches('\r')))
        .collect();
    format!(" The lines most like it:{shown}")
}

/// A line's adjacent character pairs, whitespace left out, sorted.
fn pairs(line: &str) -> Vec<(char, char)> {
    let chars: Vec<char> = line.chars().filter(|c| !c.is_whitespace()).collect();
    let mut p: Vec<(char, char)> = chars.windows(2).map(|w| (w[0], w[1])).collect();
    p.sort_unstable();
    p
}

/// Sørensen–Dice over two sorted pair lists: 1 for the same pairs, 0 for none shared.
fn dice(a: &[(char, char)], b: &[(char, char)]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return f64::from(u8::from(a.len() == b.len()));
    }
    let (mut i, mut j, mut shared) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => (i, j, shared) = (i + 1, j + 1, shared + 1),
        }
    }
    2.0 * f64::from(shared) / (a.len() + b.len()) as f64
}

/// How one line's indentation moved: old = s1 + s2, file = s3 + s2, s2 their longest common suffix
/// (the TS `/^(.*?)(.*)\n(.*?)\2$/`).
struct Shift {
    s1: String,
    s2: String,
    s3: String,
}

impl Shift {
    fn of(old: &str, file: &str) -> Shift {
        let common = old
            .bytes()
            .rev()
            .zip(file.bytes().rev())
            .take_while(|(a, b)| a == b)
            .count();
        Shift {
            s1: old[..old.len() - common].to_string(),
            s2: old[old.len() - common..].to_string(),
            s3: file[..file.len() - common].to_string(),
        }
    }
    fn old(&self) -> String {
        format!("{}{}", self.s1, self.s2)
    }
}

/// `/^[ \t]*/.exec(l)[0]`.
fn ws(l: &str) -> &str {
    &l[..l.len() - l.trim_start_matches([' ', '\t']).len()]
}

/// `!l.trim()`.
fn is_blank(l: &str) -> bool {
    l.chars().all(is_js_space)
}

/// `s.replace(/[ \t\r]+$/, "")`.
fn trim_end_blank_cr(s: &str) -> &str {
    s.trim_end_matches([' ', '\t', '\r'])
}

/// A JS multiline `^`: the start, or just after `\n`, `\r`, U+2028 or U+2029.
fn is_js_line_start(s: &str, pos: usize) -> bool {
    pos == 0
        || s[..pos]
            .chars()
            .next_back()
            .is_some_and(|c| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
}

/// The positions where a JS multiline `^` matches.
fn js_line_starts(s: &str) -> impl Iterator<Item = usize> + '_ {
    std::iter::once(0).chain(
        s.char_indices()
            .filter(|(_, c)| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
            .map(|(i, c)| i + c.len_utf8()),
    )
}

/// LN at the start of `s`: `(?:(?=[ \d]{6}\t) *\d+|\d{7,})\t`, i.e. spaces then ASCII digits
/// filling exactly 6 columns, or 7 or more digits, then a tab. The length matched.
fn ln_prefix(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() >= 7 && b[6] == b'\t' {
        let spaces = b[..6].iter().take_while(|&&c| c == b' ').count();
        if spaces < 6 && b[spaces..6].iter().all(u8::is_ascii_digit) {
            return Some(7);
        }
    }
    let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    (digits >= 7 && b.get(digits) == Some(&b'\t')).then_some(digits + 1)
}

/// Every match of the global multiline LN regex: (start, end).
fn ln_matches(s: &str) -> Vec<(usize, usize)> {
    js_line_starts(s)
        .filter_map(|p| ln_prefix(&s[p..]).map(|n| (p, p + n)))
        .collect()
}

/// `+n` of a matched line number ("     12\t" is 12).
fn ln_value(m: &str) -> f64 {
    m.trim_matches([' ', '\t']).parse::<f64>().unwrap_or(f64::NAN)
}

/// `s.replace(ln, "")`.
fn strip_ln(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for (start, end) in ln_matches(s) {
        out.push_str(&s[last..start]);
        last = end;
    }
    out.push_str(&s[last..]);
    out
}

/// `/^ *\d+ *\r?$/.test(l)` on one line (no `m`: the whole of it).
fn is_digits_line(l: &str) -> bool {
    let t = l.strip_suffix('\r').unwrap_or(l).trim_matches(' ');
    !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit())
}

/// `/^ *\d+ *\r?$/m.exec(s)`: the first line of only digits (and spaces), its index and text.
fn first_digits_line(s: &str) -> Option<(usize, &str)> {
    let b = s.as_bytes();
    let ends_line = |pos: usize| pos == s.len() || s[pos..].starts_with(['\n', '\r', '\u{2028}', '\u{2029}']);
    js_line_starts(s).find_map(|p| {
        let mut q = p;
        while b.get(q) == Some(&b' ') {
            q += 1;
        }
        let digits = q;
        while b.get(q).is_some_and(u8::is_ascii_digit) {
            q += 1;
        }
        if q == digits {
            return None;
        }
        while b.get(q) == Some(&b' ') {
            q += 1;
        }
        if b.get(q) == Some(&b'\r') && ends_line(q + 1) {
            return Some((p, &s[p..q + 1]));
        }
        ends_line(q).then(|| (p, &s[p..q]))
    })
}

/// The first [`HITS`] places `needle` starts in `file`, overlaps counted (`(?=(needle))` with `g`).
fn literal_hits(file: &str, needle: &str) -> Vec<(usize, usize)> {
    let mut hits = Vec::new();
    let mut from = 0;
    while hits.len() < HITS && from <= file.len() {
        let Some(i) = file[from..].find(needle) else { break };
        let p = from + i;
        hits.push((p, p + needle.len()));
        from = p + file[p..].chars().next().map_or(1, char::len_utf8);
    }
    hits
}

/// The first [`HITS`] places the loose pattern for `lines` matches, overlaps counted. The pattern:
/// `(?<![ \t])` if the first line is blank; each line literal, preceded by `^[ \t]*` in `indent`
/// mode when not blank; lines joined by `[ \t]*\r?\n`.
fn loose_hits(file: &str, lines: &[&str], indent: bool) -> Vec<(usize, usize)> {
    let b = file.as_bytes();
    let first = lines.first().copied().unwrap_or("");
    let mut hits = Vec::new();
    let mut p = 0;
    while hits.len() < HITS && p <= file.len() {
        let candidate = if first.is_empty() {
            // A blank first line may start only where a run of blanks does: scanning the run from
            // each of its places is quadratic.
            p == 0 || !matches!(b[p - 1], b' ' | b'\t')
        } else if indent {
            is_js_line_start(file, p)
        } else {
            match file[p..].find(first) {
                Some(i) => {
                    p += i;
                    true
                }
                None => break,
            }
        };
        if candidate && let Some(end) = match_at(file, p, lines, indent) {
            hits.push((p, end));
        }
        p += file[p..].chars().next().map_or(1, char::len_utf8);
    }
    hits
}

/// The loose pattern anchored at `p`; the end of the match.
fn match_at(file: &str, p: usize, lines: &[&str], indent: bool) -> Option<usize> {
    let b = file.as_bytes();
    let blanks = |mut q: usize| {
        while q < b.len() && matches!(b[q], b' ' | b'\t') {
            q += 1;
        }
        q
    };
    let mut q = p;
    for (k, l) in lines.iter().enumerate() {
        if k > 0 {
            // [ \t]*\r?\n
            q = blanks(q);
            if b.get(q) == Some(&b'\r') && b.get(q + 1) == Some(&b'\n') {
                q += 1;
            }
            if b.get(q) != Some(&b'\n') {
                return None;
            }
            q += 1;
        }
        if indent && !l.is_empty() {
            // ^[ \t]*
            if !is_js_line_start(file, q) {
                return None;
            }
            q = blanks(q);
        }
        if !file[q..].starts_with(l) {
            return None;
        }
        q += l.len();
    }
    Some(q)
}

/// `/^[ \t]*(?=\S)/gm` over `s`: each line's indentation that some text follows.
fn indents(s: &str) -> Vec<(usize, usize)> {
    js_line_starts(s)
        .filter_map(|p| {
            let e = p + ws(&s[p..]).len();
            s[e..].chars().next().filter(|&c| !is_js_space(c)).map(|_| (p, e))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_number_prefixes() {
        assert_eq!(ln_prefix("     1\tx"), Some(7));
        assert_eq!(ln_prefix("123456\tx"), Some(7));
        assert_eq!(ln_prefix("1234567\tx"), Some(8));
        assert_eq!(ln_prefix("123\tfoo"), None);
        assert_eq!(ln_prefix("1 2   \tx"), None);
        assert_eq!(ln_prefix("      \tx"), None);
        assert_eq!(strip_ln("     1\ta\r     2\tb\n     3\tc"), "a\rb\nc");
        assert_eq!(ln_value("     12\t"), 12.0);
    }

    #[test]
    fn digits_lines() {
        assert!(is_digits_line("  12 \r"));
        assert!(!is_digits_line("12\r\r"));
        assert!(!is_digits_line(" "));
        assert_eq!(first_digits_line("     1\ta\n0\na"), Some((9, "0")));
        assert_eq!(first_digits_line("x\n 42\r\ny"), Some((2, " 42\r")));
        assert_eq!(first_digits_line("x\n42\rz"), Some((2, "42")));
        assert_eq!(first_digits_line("x\n4 2"), None);
    }

    #[test]
    fn shifts_are_longest_common_suffixes() {
        let s = Shift::of("\t", "\t\t");
        assert_eq!((s.s1.as_str(), s.s2.as_str(), s.s3.as_str()), ("", "\t", "\t"));
        let s = Shift::of("    ", "\t");
        assert_eq!((s.s1.as_str(), s.s2.as_str(), s.s3.as_str()), ("    ", "", "\t"));
    }

    #[test]
    fn indentation_positions() {
        assert_eq!(indents("a\n  b\n   \n\tc\r\n"), [(0, 0), (2, 4), (10, 11)]);
    }
}
