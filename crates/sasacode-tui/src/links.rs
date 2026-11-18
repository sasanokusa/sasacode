//! Links a model writes to local files, made openable from the terminal (`links.ts`).

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    // INFALLIBLE: the patterns are literals checked by the tests.
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

/// `x.ts:42`, `Makefile:12`, `README.md:10#x`: a file in the folder with a line.
fn file_line(s: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?s)^[^:/]+:\d+(?::\d+)?(?:#.*)?$").is_match(s)
}

/// Schemes whose URL can be digits after the colon (`tel:5551234`): not a file with a line.
fn digit_scheme(s: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)^(?:tel|sms|fax|callto):").is_match(s)
}

fn has_scheme(s: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)^[a-z][a-z0-9+.-]*:").is_match(s)
}

/// `decodeURIComponent`, or `None` where it would throw (a stray `%`, bytes that are not UTF-8).
fn decode_uri_component(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = b.get(i + 1..i + 3)?;
            let h = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `path.resolve(cwd, p)`: absolute and normalized (`.` and `..` folded, no trailing slash).
pub fn resolve_path(cwd: &str, p: &str) -> PathBuf {
    let joined = if Path::new(p).is_absolute() {
        PathBuf::from(p)
    } else {
        Path::new(cwd).join(p)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Node's `pathToFileURL(p).href`.
pub fn file_url(p: &Path) -> String {
    match url::Url::from_file_path(p) {
        Ok(u) => u.to_string(),
        Err(()) => format!("file://{}", p.display()),
    }
}

fn home() -> String {
    dirs::home_dir()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Where a markdown link should point so the terminal can open it. A model writes local files as
/// `docs/a.md`, `src/x.ts:42`, `x.ts:42`, `README.md#setup` or `/abs/path#L3`; a terminal only
/// opens URLs, so a path becomes a file:// URL against `cwd` (a line suffix and a `#` heading are
/// dropped: a file URL has no place for a line, and an editor opens it at the top). URLs and
/// in-page anchors are left alone.
pub fn link_target(href: &str, cwd: &str) -> String {
    if href.is_empty() || href.starts_with('#') {
        return href.to_string();
    }
    let path = match href.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", home()),
        None => href.to_string(),
    };
    // `x.ts:42` and `Makefile:12` are files in the folder, not URLs (#115).
    let is_file_line = file_line(&path) && !digit_scheme(&path);
    if !Path::new(&path).is_absolute() && !is_file_line && has_scheme(&path) {
        return href.to_string();
    }
    // The fragment goes before decoding: a `%23` in a name is a real `#` of the file's (#115).
    let mut p = match path.find('#') {
        Some(i) => path[..i].to_string(),
        None => path.clone(),
    };
    static LINE: OnceLock<Regex> = OnceLock::new();
    p = re(&LINE, r":\d+(?::\d+)?$").replace(&p, "").into_owned();
    // decodeURIComponent, not decodeURI, which keeps %23 (#138).
    if let Some(d) = decode_uri_component(&p) {
        p = d;
    }
    file_url(&resolve_path(cwd, &p))
}

/// Rewrites the targets of inline links (`[text](target)`) with [`link_target`]; code is left as
/// written.
pub fn local_links(markdown: &str, cwd: &str) -> String {
    if !markdown.contains("](") {
        return markdown.to_string();
    }
    static OPEN: OnceLock<Regex> = OnceLock::new();
    static SPAN: OnceLock<Regex> = OnceLock::new();
    static LINK: OnceLock<Regex> = OnceLock::new();
    let open_re = re(&OPEN, r"^\s*(`{3,}|~{3,})");
    let span_re = re(&SPAN, r"`+[^`]*`+");
    let link_re = re(&LINK, r"\]\(\s*(<[^>\n]*>|[^)\s]+)");
    let mut fence: Option<String> = None;
    let mut out: Vec<String> = Vec::new();
    for line in markdown.split('\n') {
        let open = open_re
            .captures(line)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());
        if let Some(f) = &fence {
            if let Some(o) = &open
                && o.as_bytes()[0] == f.as_bytes()[0]
                && o.len() >= f.len()
            {
                fence = None;
            }
            out.push(line.to_string());
            continue;
        }
        if open.is_some() {
            fence = open;
            out.push(line.to_string());
            continue;
        }
        // Code spans keep their text; only the parts between them are links.
        let mut rebuilt = String::new();
        let mut last = 0;
        let rewrite = |part: &str| -> String {
            link_re
                .replace_all(part, |caps: &regex::Captures| {
                    let m = &caps[0];
                    let target = &caps[1];
                    let angle = target.starts_with('<');
                    let inner = if angle { &target[1..target.len() - 1] } else { target };
                    let to = link_target(inner, cwd);
                    let to = if angle { format!("<{to}>") } else { to };
                    m.replacen(target, &to, 1)
                })
                .into_owned()
        };
        for span in span_re.find_iter(line) {
            rebuilt.push_str(&rewrite(&line[last..span.start()]));
            rebuilt.push_str(span.as_str());
            last = span.end();
        }
        rebuilt.push_str(&rewrite(&line[last..]));
        out.push(rebuilt);
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    //! links.test.ts
    use super::*;

    fn cwd() -> String {
        format!("{}/proj", home())
    }

    fn file(parts: &[&str]) -> String {
        let mut p = PathBuf::from(cwd());
        for x in parts {
            p.push(x);
        }
        file_url(&p)
    }

    // "a path becomes a file URL the terminal can open; a line suffix is dropped"
    #[test]
    fn a_path_becomes_a_file_url() {
        let cwd = cwd();
        assert_eq!(link_target("docs/a.md", &cwd), file(&["docs", "a.md"]));
        assert_eq!(link_target("./src/x.ts:42", &cwd), file(&["src", "x.ts"]));
        assert_eq!(link_target("src/x.ts:42:7", &cwd), file(&["src", "x.ts"]));
        assert_eq!(link_target("src/x.ts#L3-L9", &cwd), file(&["src", "x.ts"]));
        assert_eq!(link_target("my%20notes.md", &cwd), file(&["my notes.md"]));
        assert_eq!(link_target("~/n.md", &cwd), file_url(&Path::new(&home()).join("n.md")));
        let abs = Path::new(&home()).join("other").join("b.md");
        assert_eq!(link_target(&abs.to_string_lossy(), &cwd), file_url(&abs));
        assert!(file(&["my notes.md"]).ends_with("/proj/my%20notes.md"));
    }

    // "URLs and anchors are left alone"
    #[test]
    fn urls_and_anchors_are_left_alone() {
        for href in [
            "https://example.com/a.md",
            "mailto:a@b.c",
            "file:///x.md",
            "#usage",
            "vscode://file/x",
        ] {
            assert_eq!(link_target(href, &cwd()), href);
        }
    }

    // "a Windows path is a path, not a scheme"
    #[cfg(windows)]
    #[test]
    fn a_windows_path_is_a_path() {
        assert_eq!(link_target("C:\\work\\a.md", &cwd()), "file:///C:/work/a.md");
    }

    // "only link targets change: link text, code spans and fenced code stay as written"
    #[test]
    fn only_link_targets_change() {
        let md = [
            "see [docs/a.md](docs/a.md) and [web](https://x.y)",
            "`[t](raw.md)` stays, [b](<b c.md>) too",
            "```md",
            "[in](fence.md)",
            "```",
        ]
        .join("\n");
        let got = local_links(&md, &cwd());
        let want = [
            format!("see [docs/a.md]({}) and [web](https://x.y)", file(&["docs", "a.md"])),
            format!("`[t](raw.md)` stays, [b](<{}>) too", file(&["b c.md"])),
            "```md".to_string(),
            "[in](fence.md)".to_string(),
            "```".to_string(),
        ];
        assert_eq!(got.split('\n').collect::<Vec<_>>(), want);
    }

    // "a file in the folder with a line is a file, a #heading is not part of the name, and $ in a
    // path stays as written (#115)"
    #[test]
    fn file_with_line_and_heading_and_dollar() {
        let cwd = cwd();
        assert_eq!(link_target("x.ts:42", &cwd), file(&["x.ts"]));
        assert_eq!(link_target("README.md:10", &cwd), file(&["README.md"]));
        assert_eq!(link_target("package.json:3:5", &cwd), file(&["package.json"]));
        assert_eq!(link_target("Makefile:12", &cwd), file(&["Makefile"]));
        assert_eq!(link_target("README.md#setup", &cwd), file(&["README.md"]));
        assert_eq!(link_target("docs/guide.md#install", &cwd), file(&["docs", "guide.md"]));
        for href in ["tel:5551234", "localhost:3000/x", "vscode:extension/a.b"] {
            assert_eq!(link_target(href, &cwd), href);
        }
        assert_eq!(
            local_links("see [a](a$$b.md) and [r](x$&y.md)", &cwd),
            format!("see [a]({}) and [r]({})", file(&["a$$b.md"]), file(&["x$&y.md"]))
        );
    }

    // "%23 in a link is the file's own # (#138)"
    #[test]
    fn percent_23_is_the_files_own_hash() {
        let cwd = cwd();
        assert_eq!(link_target("foo%23bar.md", &cwd), file(&["foo#bar.md"]));
        assert_eq!(link_target("a%20b.md:3", &cwd), file(&["a b.md"]));
        assert!(file(&["foo#bar.md"]).ends_with("foo%23bar.md"));
    }

    // "the rendered terminal link points at the file"
    #[test]
    fn the_rendered_terminal_link_points_at_the_file() {
        use crate::ui::Component;
        use crate::ui::markdown::{Markdown, set_hyperlinks_override};
        let _g = crate::testing::global_lock();
        set_hyperlinks_override(Some(true));
        let dir = cwd();
        let mut md = Markdown::new("[a](docs/a.md)", 0, 0, crate::theme::MARKDOWN_THEME)
            .with_transform(Box::new(move |m, _| local_links(m, &dir)));
        let out = md.render(200).join("");
        set_hyperlinks_override(None);
        assert!(
            out.contains(&format!("\x1b]8;;{}\x1b\\", file(&["docs", "a.md"]))),
            "{out:?}"
        );
    }
}
