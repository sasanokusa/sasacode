//! Paths and diffs shared by the tools (`packages/tools/src/util.ts`), plus the JS string and
//! argument semantics the tools rely on.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use sasacode_plugin_api::JsonObject;

/// `resolvePath`: `~` and `~/...` expand to the home directory; an absolute path is returned as it
/// is (not normalised, like Node); a relative one is resolved against `cwd` and normalised.
pub fn resolve_path(p: &str, cwd: &Path) -> String {
    let p = if p == "~" || p.starts_with("~/") {
        format!("{}{}", sasacode_plugin_api::home_dir().display(), &p[1..])
    } else {
        p.to_string()
    };
    // Node's isAbsolute: a root, with or without a drive letter on Windows.
    if Path::new(&p).has_root() {
        return p;
    }
    let base = if cwd.has_root() {
        cwd.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(cwd)
    };
    normalize(&base.join(&p)).to_string_lossy().into_owned()
}

/// Lexical normalisation as Node's `path.resolve` does it: `.` dropped, `..` pops (never above the
/// root), no trailing separator.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => out.push(n),
        }
    }
    out
}

/// Compact line diff for display: common prefix/suffix trimmed, 3 lines of context (`lineDiff`).
pub fn line_diff(before: &str, after: &str) -> String {
    line_diff_with_context(before, after, 3)
}

/// `lineDiff(before, after, context)`: one `@@ -N +N @@` header, the context before, every removed
/// line, every added line, the context after.
pub fn line_diff_with_context(before: &str, after: &str, context: usize) -> String {
    let a: Vec<&str> = before.split('\n').collect();
    let b: Vec<&str> = after.split('\n').collect();
    let mut start = 0;
    while start < a.len() && start < b.len() && a[start] == b[start] {
        start += 1;
    }
    // Signed: the ends may sit one before `start`.
    let (mut end_a, mut end_b) = (a.len() as isize - 1, b.len() as isize - 1);
    let s = start as isize;
    while end_a >= s && end_b >= s && a[end_a as usize] == b[end_b as usize] {
        end_a -= 1;
        end_b -= 1;
    }
    let from = start.saturating_sub(context);
    let mut out = vec![format!("@@ -{} +{} @@", from + 1, from + 1)];
    out.extend(a[from..start].iter().map(|l| format!(" {l}")));
    out.extend((s..=end_a).map(|i| format!("-{}", a[i as usize])));
    out.extend((s..=end_b).map(|i| format!("+{}", b[i as usize])));
    let last = (a.len() as isize - 1).min(end_a + context as isize);
    out.extend((end_a + 1..=last).map(|i| format!(" {}", a[i as usize])));
    out.join("\n")
}

/// `splitLines`: split on `\n`; a final newline ends the last line, it does not start one.
pub fn split_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.len() > 1 && lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

// ── JS semantics ─────────────────────────────────────────────────────

/// A string's JS `length`: UTF-16 code units.
pub(crate) fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// The longest prefix of whole characters that is at most `max` UTF-16 units: `s.slice(0, max)`
/// without leaving half a surrogate pair (#88).
pub(crate) fn utf16_prefix(s: &str, max: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > max {
            return &s[..i];
        }
    }
    s
}

/// JS `\s` (and what `trim` removes): WhiteSpace, the BOM and the line terminators.
pub(crate) fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{B}' | '\u{C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// `s.trimEnd()`.
pub(crate) fn js_trim_end(s: &str) -> &str {
    s.trim_end_matches(is_js_space)
}

/// A JS number as text: `120`, `0.3`.
pub(crate) fn js_number_text(v: f64) -> String {
    sasacode_ai::json::js_number(v).to_string()
}

/// `String(a[key])` for labels: missing is "undefined", a number prints like JS.
pub(crate) fn js_string_arg(a: &JsonObject, key: &str) -> String {
    match a.get(key) {
        None => "undefined".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => js_number_text(n.as_f64().unwrap_or(0.0)),
        Some(Value::Null) => "null".into(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => "[object Object]".into(),
    }
}

/// A string argument; anything else (validation already refused it) is the empty string.
pub(crate) fn str_arg<'a>(a: &'a JsonObject, key: &str) -> &'a str {
    a.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A numeric argument, `2` and `2.0` alike. `None` when missing or null.
pub(crate) fn num_arg(a: &JsonObject, key: &str) -> Option<f64> {
    a.get(key).and_then(Value::as_f64)
}

/// JS truthiness of an argument.
pub(crate) fn truthy_arg(a: &JsonObject, key: &str) -> bool {
    match a.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => true,
    }
}

/// A file-system error worded as Node/Bun word it (`EACCES: permission denied, open '/x'`): the
/// text becomes "Tool failed: ..." for the model.
pub fn fs_error(e: &std::io::Error, syscall: &str, path: &str) -> anyhow::Error {
    match errno_text(e) {
        Some((code, desc)) => anyhow::anyhow!("{code}: {desc}, {syscall} '{path}'"),
        None => anyhow::anyhow!("{e}"),
    }
}

fn errno_text(e: &std::io::Error) -> Option<(&'static str, &'static str)> {
    #[cfg(unix)]
    if let Some(n) = e.raw_os_error() {
        let known = match n {
            libc::ENOENT => Some(("ENOENT", "no such file or directory")),
            libc::EACCES => Some(("EACCES", "permission denied")),
            libc::EPERM => Some(("EPERM", "operation not permitted")),
            libc::EISDIR => Some(("EISDIR", "illegal operation on a directory")),
            libc::ENOTDIR => Some(("ENOTDIR", "not a directory")),
            libc::EEXIST => Some(("EEXIST", "file already exists")),
            libc::EROFS => Some(("EROFS", "read-only file system")),
            libc::ENOSPC => Some(("ENOSPC", "no space left on device")),
            libc::ELOOP => Some(("ELOOP", "too many symbolic links encountered")),
            libc::ENAMETOOLONG => Some(("ENAMETOOLONG", "name too long")),
            libc::EMFILE => Some(("EMFILE", "too many open files")),
            libc::EBUSY => Some(("EBUSY", "resource busy or locked")),
            libc::ETXTBSY => Some(("ETXTBSY", "text file is busy")),
            _ => None,
        };
        if known.is_some() {
            return known;
        }
    }
    use std::io::ErrorKind as K;
    match e.kind() {
        K::NotFound => Some(("ENOENT", "no such file or directory")),
        // libuv reports a refused open on Windows as EPERM.
        K::PermissionDenied if cfg!(windows) => Some(("EPERM", "operation not permitted")),
        K::PermissionDenied => Some(("EACCES", "permission denied")),
        K::IsADirectory => Some(("EISDIR", "illegal operation on a directory")),
        K::NotADirectory => Some(("ENOTDIR", "not a directory")),
        K::AlreadyExists => Some(("EEXIST", "file already exists")),
        K::ReadOnlyFilesystem => Some(("EROFS", "read-only file system")),
        K::StorageFull => Some(("ENOSPC", "no space left on device")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_like_node() {
        let cwd = Path::new("/w/p");
        assert_eq!(resolve_path("a/../b/./c/", cwd), "/w/p/b/c");
        assert_eq!(resolve_path("../../../x", cwd), "/x");
        assert_eq!(resolve_path("", cwd), "/w/p");
        // Absolute paths and ~ expansions are taken as they are, `..` and all.
        assert_eq!(resolve_path("/a/../b", cwd), "/a/../b");
        let home = sasacode_plugin_api::home_dir().display().to_string();
        assert_eq!(resolve_path("~/x/../y", cwd), format!("{home}/x/../y"));
        assert_eq!(resolve_path("~", cwd), home);
        assert_eq!(resolve_path("~x", cwd), "/w/p/~x");
    }

    #[test]
    fn line_diff_trims_and_keeps_context() {
        assert_eq!(line_diff("a\nb\nc", "a\nB\nc"), "@@ -1 +1 @@\n a\n-b\n+B\n c");
        let before = "1\n2\n3\n4\n5\n6\n7\n8\n9";
        let after = "1\n2\n3\n4\nfive\n6\n7\n8\n9";
        assert_eq!(
            line_diff(before, after),
            "@@ -2 +2 @@\n 2\n 3\n 4\n-5\n+five\n 6\n 7\n 8"
        );
        assert_eq!(line_diff("x", "x\ny"), "@@ -1 +1 @@\n x\n+y");
        assert_eq!(line_diff("x\ny", "x"), "@@ -1 +1 @@\n x\n-y");
        assert_eq!(line_diff("", "a"), "@@ -1 +1 @@\n-\n+a");
    }

    #[test]
    fn utf16_helpers() {
        assert_eq!(utf16_len("a😀あ"), 4);
        assert_eq!(utf16_prefix("ab😀", 3), "ab");
        assert_eq!(utf16_prefix("ab😀", 4), "ab😀");
        assert_eq!(js_trim_end("x \u{FEFF}\n\u{3000}"), "x");
        assert_eq!(js_trim_end("x\u{85}"), "x\u{85}");
    }
}
