//! What the input offers to complete: slash commands, their arguments and file paths (pi-tui
//! `CombinedAutocompleteProvider` and fuzzy.js). `@` mentions are off: they search with `fd`, which
//! sasacode does not ship, so they never offer anything.

use std::path::Path;

use crate::text::js_trim;
use crate::text::width::{is_cjk_punctuation, is_js_whitespace};
use crate::ui::select_list::SelectItem;

/// fuzzy.js `fuzzyMatch`: all query characters in order; lower scores are better.
pub fn fuzzy_match(query: &str, text: &str) -> Option<f64> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let run = |q: &[char]| -> Option<f64> {
        if q.is_empty() {
            return Some(0.0);
        }
        if q.len() > t.len() {
            return None;
        }
        let mut score = 0.0;
        let mut last: i64 = -1;
        let mut consecutive = 0.0;
        for &qc in q {
            let from = (last + 1) as usize;
            let i = t.get(from..)?.iter().position(|&c| c == qc)? + from;
            let boundary = i == 0 || matches!(t[i - 1], '-' | '_' | '.' | '/' | ':') || is_js_whitespace(t[i - 1]);
            if last == i as i64 - 1 {
                consecutive += 1.0;
                score -= consecutive * 5.0;
            } else {
                consecutive = 0.0;
                if last >= 0 {
                    score += (i as i64 - last - 1) as f64 * 2.0;
                }
            }
            if boundary {
                score -= 10.0;
            }
            score += i as f64 * 0.1;
            last = i as i64;
        }
        if q == t.as_slice() {
            score -= 100.0;
        }
        Some(score)
    };
    if let Some(s) = run(&q) {
        return Some(s);
    }
    // "abc12" also finds "12abc", and the other way round.
    let letters: String = q.iter().take_while(|c| c.is_ascii_lowercase()).collect();
    let digits: String = q.iter().take_while(|c| c.is_ascii_digit()).collect();
    let swapped: Vec<char> = if !letters.is_empty()
        && q[letters.len()..].iter().all(|c| c.is_ascii_digit())
        && q.len() > letters.len()
    {
        format!("{}{letters}", q[letters.len()..].iter().collect::<String>())
            .chars()
            .collect()
    } else if !digits.is_empty() && q[digits.len()..].iter().all(|c| c.is_ascii_lowercase()) && q.len() > digits.len() {
        format!("{}{digits}", q[digits.len()..].iter().collect::<String>())
            .chars()
            .collect()
    } else {
        return None;
    };
    run(&swapped).map(|s| s + 5.0)
}

/// fuzzy.js `fuzzyFilter`: items matching every whitespace- or slash-separated token, best first.
pub fn fuzzy_filter<T: Clone>(items: &[T], query: &str, text_of: impl Fn(&T) -> String) -> Vec<T> {
    let q = js_trim(query);
    if q.is_empty() {
        return items.to_vec();
    }
    let tokens: Vec<&str> = q
        .split(|c: char| is_js_whitespace(c) || c == '/')
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return items.to_vec();
    }
    let mut scored: Vec<(T, f64)> = Vec::new();
    'items: for item in items {
        let text = text_of(item);
        let mut total = 0.0;
        for tok in &tokens {
            match fuzzy_match(tok, &text) {
                Some(s) => total += s,
                None => continue 'items,
            }
        }
        scored.push((item.clone(), total));
    }
    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().map(|(i, _)| i).collect()
}

/// A command as the completion lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEntry {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    /// Whether it offers candidates for its argument (asked asynchronously).
    pub completes: bool,
}

/// What a request produced: items now, or an argument completion to ask the command for.
#[derive(Debug, Clone, PartialEq)]
pub enum Suggest {
    None,
    Items { items: Vec<SelectItem>, prefix: String },
    Argument { command: String, argument: String },
}

/// The new text and cursor after a completion is taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub lines: Vec<String>,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

const PATH_DELIMITERS: [char; 5] = [' ', '\t', '"', '\'', '='];

fn is_separator(c: char) -> bool {
    is_js_whitespace(c) || is_cjk_punctuation(c)
}

/// `autocompleteBoundaryRegex + "$"`: empty, or ending in whitespace or CJK punctuation.
fn ends_at_token_start(text: &str) -> bool {
    text.is_empty() || text.chars().last().is_some_and(is_separator)
}

fn find_last_delimiter(text: &str) -> Option<usize> {
    text.char_indices()
        .rev()
        .find(|(_, c)| PATH_DELIMITERS.contains(c) || is_separator(*c))
        .map(|(i, c)| i + c.len_utf8())
}

fn find_unclosed_quote_start(text: &str) -> Option<usize> {
    let mut open = None;
    for (i, b) in text.bytes().enumerate() {
        if b == b'"' {
            open = if open.is_some() { None } else { Some(i) };
        }
    }
    open
}

fn is_token_start(text: &str, index: usize) -> bool {
    let before = &text[..index];
    before.chars().last().is_some_and(|c| PATH_DELIMITERS.contains(&c)) || ends_at_token_start(before)
}

fn extract_quoted_prefix(text: &str) -> Option<&str> {
    let q = find_unclosed_quote_start(text)?;
    if q > 0 && text.as_bytes()[q - 1] == b'@' {
        return is_token_start(text, q - 1).then(|| &text[q - 1..]);
    }
    is_token_start(text, q).then(|| &text[q..])
}

struct PathPrefix<'a> {
    raw: &'a str,
    at: bool,
    quoted: bool,
}

fn parse_path_prefix(prefix: &str) -> PathPrefix<'_> {
    if let Some(r) = prefix.strip_prefix("@\"") {
        PathPrefix {
            raw: r,
            at: true,
            quoted: true,
        }
    } else if let Some(r) = prefix.strip_prefix('"') {
        PathPrefix {
            raw: r,
            at: false,
            quoted: true,
        }
    } else if let Some(r) = prefix.strip_prefix('@') {
        PathPrefix {
            raw: r,
            at: true,
            quoted: false,
        }
    } else {
        PathPrefix {
            raw: prefix,
            at: false,
            quoted: false,
        }
    }
}

fn completion_value(path: &str, at: bool, quoted: bool) -> String {
    let needs_quotes = quoted || path.chars().any(is_separator);
    let prefix = if at { "@" } else { "" };
    if needs_quotes {
        format!("{prefix}\"{path}\"")
    } else {
        format!("{prefix}{path}")
    }
}

/// Node's `path.dirname` (POSIX).
fn dirname(p: &str) -> String {
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        return if p.starts_with('/') { "/".into() } else { ".".into() };
    }
    match trimmed.rfind('/') {
        Some(0) => "/".into(),
        Some(i) => trimmed[..i].trim_end_matches('/').to_string(),
        None => ".".into(),
    }
}

/// Node's `path.basename` (POSIX).
fn basename(p: &str) -> String {
    let trimmed = p.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or("").to_string()
}

/// Node's `path.join` (POSIX): joined and normalized.
fn join(a: &str, b: &str) -> String {
    let joined = if a.is_empty() {
        b.to_string()
    } else if b.is_empty() {
        a.to_string()
    } else {
        format!("{a}/{b}")
    };
    normalize(&joined)
}

fn normalize(p: &str) -> String {
    if p.is_empty() {
        return ".".into();
    }
    let absolute = p.starts_with('/');
    let trailing = p.ends_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|l| *l != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            s => parts.push(s),
        }
    }
    let mut out = parts.join("/");
    if absolute {
        out = format!("/{out}");
    }
    if out.is_empty() {
        out = ".".into();
    }
    if trailing && !out.ends_with('/') {
        out.push('/');
    }
    out
}

fn home() -> String {
    dirs::home_dir()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub struct AutocompleteProvider {
    pub commands: Vec<CommandEntry>,
    pub base_path: String,
}

impl AutocompleteProvider {
    pub fn new(commands: Vec<CommandEntry>, base_path: impl Into<String>) -> Self {
        Self {
            commands,
            base_path: base_path.into(),
        }
    }

    /// What to offer for the text before the cursor (pi-tui `getSuggestions`).
    pub fn suggestions(&self, lines: &[String], cursor_line: usize, cursor_col: usize, force: bool) -> Suggest {
        let line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let before = &line[..cursor_col.min(line.len())];
        if self.extract_at_prefix(before).is_some() {
            // `@` mentions search with fd: off.
            return Suggest::None;
        }
        if !force && before.starts_with('/') {
            let Some(space) = before.find(' ') else {
                let prefix = &before[1..];
                let entries: Vec<(String, Option<String>)> = self
                    .commands
                    .iter()
                    .map(|c| {
                        let full = match &c.argument_hint {
                            Some(h) if !h.is_empty() => {
                                if c.description.is_empty() {
                                    h.clone()
                                } else {
                                    format!("{h} — {}", c.description)
                                }
                            }
                            _ => c.description.clone(),
                        };
                        (c.name.clone(), (!full.is_empty()).then_some(full))
                    })
                    .collect();
                let filtered = fuzzy_filter(&entries, prefix, |(name, _)| {
                    if !prefix.starts_with("skill:") && name.starts_with("skill:") {
                        name["skill:".len()..].to_string()
                    } else {
                        name.clone()
                    }
                });
                if filtered.is_empty() {
                    return Suggest::None;
                }
                let items = filtered
                    .into_iter()
                    .map(|(name, desc)| SelectItem::new(name.clone(), name, desc))
                    .collect();
                return Suggest::Items {
                    items,
                    prefix: before.to_string(),
                };
            };
            let name = &before[1..space];
            let argument = &before[space + 1..];
            return match self.commands.iter().find(|c| c.name == name) {
                Some(c) if c.completes => Suggest::Argument {
                    command: name.to_string(),
                    argument: argument.to_string(),
                },
                _ => Suggest::None,
            };
        }
        let Some(path) = self.extract_path_prefix(before, force) else {
            return Suggest::None;
        };
        let items = self.file_suggestions(&path);
        if items.is_empty() {
            return Suggest::None;
        }
        Suggest::Items { items, prefix: path }
    }

    /// The text after taking `item` for `prefix` (pi-tui `applyCompletion`).
    pub fn apply_completion(
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &SelectItem,
        prefix: &str,
    ) -> Applied {
        let line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let col = cursor_col.min(line.len());
        let before_prefix = &line[..col.saturating_sub(prefix.len())];
        let after = &line[col..];
        let quoted_prefix = prefix.starts_with('"') || prefix.starts_with("@\"");
        let after = if quoted_prefix && item.value.ends_with('"') && after.starts_with('"') {
            &after[1..]
        } else {
            after
        };
        let mut new_lines = lines.to_vec();
        let set = |new_lines: &mut Vec<String>, text: String| {
            if let Some(l) = new_lines.get_mut(cursor_line) {
                *l = text;
            }
        };
        let is_dir = item.label.ends_with('/');
        let trailing_quote = item.value.ends_with('"');
        let offset = if is_dir && trailing_quote {
            item.value.len() - 1
        } else {
            item.value.len()
        };
        if prefix.starts_with('/') && js_trim(before_prefix).is_empty() && !prefix[1..].contains('/') {
            set(&mut new_lines, format!("{before_prefix}/{} {after}", item.value));
            return Applied {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix.len() + item.value.len() + 2,
            };
        }
        if prefix.starts_with('@') {
            let suffix = if is_dir { "" } else { " " };
            set(&mut new_lines, format!("{before_prefix}{}{suffix}{after}", item.value));
            return Applied {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix.len() + offset + suffix.len(),
            };
        }
        set(&mut new_lines, format!("{before_prefix}{}{after}", item.value));
        Applied {
            lines: new_lines,
            cursor_line,
            cursor_col: before_prefix.len() + offset,
        }
    }

    /// Whether Tab may look for files here (not while typing a command name).
    pub fn should_trigger_file_completion(lines: &[String], cursor_line: usize, cursor_col: usize) -> bool {
        let line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let before = js_trim(&line[..cursor_col.min(line.len())]);
        !(before.starts_with('/') && !before.contains(' '))
    }

    fn extract_at_prefix<'a>(&self, text: &'a str) -> Option<&'a str> {
        if let Some(q) = extract_quoted_prefix(text)
            && q.starts_with("@\"")
        {
            return Some(q);
        }
        let start = find_last_delimiter(text).unwrap_or(0);
        text[start..].starts_with('@').then(|| &text[start..])
    }

    fn extract_path_prefix(&self, text: &str, force: bool) -> Option<String> {
        if let Some(q) = extract_quoted_prefix(text) {
            return Some(q.to_string());
        }
        let prefix = match find_last_delimiter(text) {
            Some(i) => &text[i..],
            None => text,
        };
        if force {
            return Some(prefix.to_string());
        }
        if prefix.contains('/') || prefix.starts_with('.') || prefix.starts_with("~/") {
            return Some(prefix.to_string());
        }
        if prefix.is_empty() && !text.is_empty() && ends_at_token_start(text) {
            return Some(String::new());
        }
        None
    }

    fn expand_home(path: &str) -> String {
        if let Some(rest) = path.strip_prefix("~/") {
            let expanded = join(&home(), rest);
            if path.ends_with('/') && !expanded.ends_with('/') {
                return format!("{expanded}/");
            }
            return expanded;
        }
        if path == "~" {
            return home();
        }
        path.to_string()
    }

    /// Entries of the folder the prefix names, folders first (pi-tui `getFileSuggestions`).
    fn file_suggestions(&self, prefix: &str) -> Vec<SelectItem> {
        let p = parse_path_prefix(prefix);
        let raw = p.raw;
        let expanded = if raw.starts_with('~') {
            Self::expand_home(raw)
        } else {
            raw.to_string()
        };
        let root_like = matches!(raw, "" | "./" | "../" | "~" | "~/" | "/") || (p.at && raw.is_empty());
        let absolute = raw.starts_with('~') || expanded.starts_with('/');
        let (dir, search) = if root_like || raw.ends_with('/') {
            let d = if absolute {
                expanded.clone()
            } else {
                join(&self.base_path, &expanded)
            };
            (d, String::new())
        } else {
            let d = dirname(&expanded);
            let f = basename(&expanded);
            let d = if absolute { d } else { join(&self.base_path, &d) };
            (d, f)
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let search_lower = search.to_lowercase();
        let mut out: Vec<SelectItem> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.to_lowercase().starts_with(&search_lower) {
                continue;
            }
            let ft = entry.file_type().ok();
            let mut is_dir = ft.is_some_and(|t| t.is_dir());
            if !is_dir && ft.is_some_and(|t| t.is_symlink()) {
                is_dir = Path::new(&dir).join(&name).is_dir();
            }
            let relative = if raw.ends_with('/') {
                format!("{raw}{name}")
            } else if raw.contains('/') || raw.contains('\\') {
                if let Some(home_rel) = raw.strip_prefix("~/") {
                    let d = dirname(home_rel);
                    if d == "." {
                        format!("~/{name}")
                    } else {
                        format!("~/{}", join(&d, &name))
                    }
                } else if raw.starts_with('/') {
                    let d = dirname(raw);
                    if d == "/" {
                        format!("/{name}")
                    } else {
                        format!("{d}/{name}")
                    }
                } else {
                    let r = join(&dirname(raw), &name);
                    if raw.starts_with("./") && !r.starts_with("./") {
                        format!("./{r}")
                    } else {
                        r
                    }
                }
            } else if raw.starts_with('~') {
                format!("~/{name}")
            } else {
                name.clone()
            };
            let relative = relative.replace('\\', "/");
            let path_value = if is_dir { format!("{relative}/") } else { relative };
            out.push(SelectItem::new(
                completion_value(&path_value, p.at, p.quoted),
                format!("{name}{}", if is_dir { "/" } else { "" }),
                None,
            ));
        }
        out.sort_by(|a, b| {
            let ad = a.label.ends_with('/');
            let bd = b.label.ends_with('/');
            bd.cmp(&ad)
                .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
                .then_with(|| a.label.cmp(&b.label))
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> CommandEntry {
        CommandEntry {
            name: name.into(),
            description: format!("{name} desc"),
            argument_hint: None,
            completes: false,
        }
    }

    #[test]
    fn slash_names_filter_fuzzily_best_first() {
        let p = AutocompleteProvider::new(
            vec![entry("model"), entry("resume"), entry("permission"), entry("exit")],
            "/",
        );
        let lines = vec!["/mo".to_string()];
        match p.suggestions(&lines, 0, 3, false) {
            Suggest::Items { items, prefix } => {
                assert_eq!(prefix, "/mo");
                assert_eq!(items[0].value, "model");
            }
            other => panic!("unexpected {other:?}"),
        }
        let lines = vec!["/model x".to_string()];
        assert_eq!(p.suggestions(&lines, 0, 8, false), Suggest::None);
    }

    #[test]
    fn taking_a_command_adds_a_space_and_an_argument_replaces_the_prefix() {
        let item = SelectItem::new("model", "model", None);
        let a = AutocompleteProvider::apply_completion(&["/mo".into()], 0, 3, &item, "/mo");
        assert_eq!(a.lines, vec!["/model ".to_string()]);
        assert_eq!(a.cursor_col, 7);
        let item = SelectItem::new("high", "high", None);
        let a = AutocompleteProvider::apply_completion(&["/effort hi".into()], 0, 10, &item, "hi");
        assert_eq!(a.lines, vec!["/effort high".to_string()]);
        assert_eq!(a.cursor_col, 12);
    }

    #[test]
    fn node_paths() {
        assert_eq!(dirname("a/b/c"), "a/b");
        assert_eq!(dirname("a"), ".");
        assert_eq!(dirname("/a"), "/");
        assert_eq!(basename("a/b/"), "b");
        assert_eq!(join("/x", "."), "/x");
        assert_eq!(join("/x", "./src/"), "/x/src/");
        assert_eq!(join("a", "../b"), "b");
    }
}
