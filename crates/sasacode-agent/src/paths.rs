//! Node's `path` functions the permission code relies on (`resolve`, `relative`, `dirname`,
//! `isAbsolute`, `parse().root`), on strings, with Node's lexical normalisation: `Path::join` in
//! Rust does not collapse `..`, Node's `resolve` does.

pub const WINDOWS: bool = cfg!(windows);

pub fn is_sep(c: char) -> bool {
    c == '/' || (WINDOWS && c == '\\')
}

pub fn sep() -> char {
    if WINDOWS { '\\' } else { '/' }
}

/// `path.parse(p).root`: "/" for an absolute POSIX path; on Windows "C:\", "C:", "\\server\share\" or "\".
pub fn root_of_parse(p: &str) -> String {
    if !WINDOWS {
        return if p.starts_with('/') { "/".into() } else { String::new() };
    }
    let b: Vec<char> = p.chars().collect();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == ':' {
        if b.len() >= 3 && is_sep(b[2]) {
            return format!("{}{}", &p[..2], b[2]);
        }
        return p[..2].to_string();
    }
    if b.len() >= 2 && is_sep(b[0]) && is_sep(b[1]) {
        // \\server\share\
        let rest: String = b[2..].iter().collect();
        let mut parts = rest.splitn(3, is_sep);
        let server = parts.next().unwrap_or("");
        let share = parts.next().unwrap_or("");
        return format!("\\\\{server}\\{share}\\");
    }
    if !b.is_empty() && is_sep(b[0]) {
        return b[0].to_string();
    }
    String::new()
}

pub fn is_absolute(p: &str) -> bool {
    if !WINDOWS {
        return p.starts_with('/');
    }
    let b: Vec<char> = p.chars().take(3).collect();
    (!b.is_empty() && is_sep(b[0])) || (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == ':' && is_sep(b[2]))
}

fn cwd_string() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".into())
}

/// Node's `path.resolve(...parts)`: right to left until an absolute path, then normalised, no trailing separator.
pub fn resolve(parts: &[&str]) -> String {
    let mut acc: Vec<&str> = Vec::new();
    for p in parts.iter().rev() {
        if p.is_empty() {
            continue;
        }
        acc.push(p);
        if is_absolute(p) {
            break;
        }
    }
    acc.reverse();
    let cwd;
    if acc.first().is_none_or(|p| !is_absolute(p)) {
        cwd = cwd_string();
        acc.insert(0, &cwd);
    }
    let joined = acc.join(&sep().to_string());
    normalize_abs(&joined)
}

/// `path.resolve(base, p)`.
pub fn resolve2(base: &str, p: &str) -> String {
    resolve(&[base, p])
}

/// Collapse `.`, `..` and repeated separators of an absolute path.
pub fn normalize_abs(p: &str) -> String {
    let root = root_of_parse(p);
    let rest = &p[root.len()..];
    let mut out: Vec<&str> = Vec::new();
    for part in rest.split(is_sep) {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            x => out.push(x),
        }
    }
    let root = if WINDOWS { root.replace('/', "\\") } else { root };
    let s = sep().to_string();
    if root.is_empty() {
        out.join(&s)
    } else {
        format!("{root}{}", out.join(&s))
    }
}

/// `path.dirname(p)` of an absolute, normalised path.
pub fn dirname(p: &str) -> String {
    let root = root_of_parse(p);
    let rest = p[root.len()..].trim_end_matches(is_sep);
    match rest.rfind(is_sep) {
        Some(i) => format!("{root}{}", &rest[..i]),
        None => {
            if root.is_empty() {
                ".".into()
            } else {
                root
            }
        }
    }
}

/// `path.join(a, b)`, normalised.
pub fn join(a: &str, b: &str) -> String {
    if a.is_empty() {
        return b.to_string();
    }
    let s = sep();
    let joined = if a.ends_with(is_sep) {
        format!("{a}{b}")
    } else {
        format!("{a}{s}{b}")
    };
    if is_absolute(&joined) {
        normalize_abs(&joined)
    } else {
        joined
    }
}

/// `path.relative(from, to)` for paths that resolve() normalises.
pub fn relative(from: &str, to: &str) -> String {
    let from = resolve(&[from]);
    let to = resolve(&[to]);
    let eq = |a: &str, b: &str| if WINDOWS { a.eq_ignore_ascii_case(b) } else { a == b };
    if eq(&from, &to) {
        return String::new();
    }
    let fr = root_of_parse(&from);
    let tr = root_of_parse(&to);
    if WINDOWS && !eq(&fr, &tr) {
        return to;
    }
    let fp: Vec<&str> = from[fr.len()..].split(is_sep).filter(|s| !s.is_empty()).collect();
    let tp: Vec<&str> = to[tr.len()..].split(is_sep).filter(|s| !s.is_empty()).collect();
    let mut i = 0;
    while i < fp.len() && i < tp.len() && eq(fp[i], tp[i]) {
        i += 1;
    }
    let mut parts: Vec<&str> = vec![".."; fp.len() - i];
    parts.extend_from_slice(&tp[i..]);
    parts.join(&sep().to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn node_like() {
        assert_eq!(resolve(&["/work/proj", "src/a.ts"]), "/work/proj/src/a.ts");
        assert_eq!(resolve(&["/work/proj", "../x"]), "/work/x");
        assert_eq!(resolve(&["/work/proj", "/etc/passwd"]), "/etc/passwd");
        assert_eq!(resolve(&["/work/proj/"]), "/work/proj");
        assert_eq!(relative("/work/proj", "/work/proj/.git/config"), ".git/config");
        assert_eq!(relative("/work/proj", "/work/x"), "../x");
        assert_eq!(relative("/work/proj", "/work/proj"), "");
        assert_eq!(dirname("/work/proj"), "/work");
        assert_eq!(dirname("/work"), "/");
        assert_eq!(dirname("/"), "/");
        assert_eq!(join("/", "a"), "/a");
    }
}
