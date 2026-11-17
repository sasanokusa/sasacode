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
    if WINDOWS {
        return win32_resolve(parts, &cwd_string(), |drive| std::env::var(format!("={drive}")).ok());
    }
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

/// Node's `path.win32.resolve`, given the working directory and the per-drive ones (`=C:` in a
/// Windows environment). A path rooted without a drive (`\tmp`) takes the drive of what it is
/// resolved against, as a drive-relative one (`C:x`) takes that drive's directory.
pub fn win32_resolve(parts: &[&str], cwd: &str, drive_cwd: impl Fn(&str) -> Option<String>) -> String {
    let is_sep = |c: char| c == '/' || c == '\\';
    let mut device = String::new();
    let mut tail = String::new();
    let mut absolute = false;
    let mut i = parts.len() as isize - 1;
    while i >= -1 {
        let path = if i >= 0 {
            parts[i as usize].to_string()
        } else if device.is_empty() {
            cwd.to_string()
        } else {
            let p = drive_cwd(&device)
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| cwd.to_string());
            let head: String = p.chars().take(2).collect();
            if head.to_lowercase() != device.to_lowercase() && p.chars().nth(2) == Some('\\') {
                format!("{device}\\")
            } else {
                p
            }
        };
        i -= 1;
        let c: Vec<char> = path.chars().collect();
        let len = c.len();
        if len == 0 {
            continue;
        }
        let (mut root_end, mut dev, mut is_abs) = (0, String::new(), false);
        if len == 1 {
            if is_sep(c[0]) {
                root_end = 1;
                is_abs = true;
            }
        } else if is_sep(c[0]) {
            is_abs = true;
            if is_sep(c[1]) {
                // \\server\share
                let mut j = 2;
                let mut last = j;
                while j < len && !is_sep(c[j]) {
                    j += 1;
                }
                if j < len && j != last {
                    let server: String = c[last..j].iter().collect();
                    last = j;
                    while j < len && is_sep(c[j]) {
                        j += 1;
                    }
                    if j < len && j != last {
                        last = j;
                        while j < len && !is_sep(c[j]) {
                            j += 1;
                        }
                        if j == len || j != last {
                            let share: String = c[last..j].iter().collect();
                            dev = format!("\\\\{server}\\{share}");
                            root_end = j;
                        }
                    }
                }
            } else {
                root_end = 1;
            }
        } else if c[0].is_ascii_alphabetic() && c[1] == ':' {
            dev = c[..2].iter().collect();
            root_end = 2;
            if len > 2 && is_sep(c[2]) {
                is_abs = true;
                root_end = 3;
            }
        }
        if !dev.is_empty() {
            if device.is_empty() {
                device = dev;
            } else if dev.to_lowercase() != device.to_lowercase() {
                continue; // another drive's path: not on the way to this one
            }
        }
        if absolute {
            if !device.is_empty() {
                break;
            }
        } else {
            let rest: String = c[root_end..].iter().collect();
            tail = format!("{rest}\\{tail}");
            absolute = is_abs;
            if is_abs && !device.is_empty() {
                break;
            }
        }
    }
    let mut out: Vec<&str> = Vec::new();
    for part in tail.split(is_sep) {
        match part {
            "" | "." => {}
            ".." if out.last().is_some_and(|l| *l != "..") => {
                out.pop();
            }
            ".." if !absolute => out.push(".."),
            ".." => {}
            x => out.push(x),
        }
    }
    let tail = out.join("\\");
    if absolute {
        format!("{device}\\{tail}")
    } else if device.is_empty() && tail.is_empty() {
        ".".into()
    } else {
        format!("{device}{tail}")
    }
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

/// `path.win32.resolve`, as Node answers it with D:\a\proj for its working directory (on every
/// system: the function takes the directory it resolves against).
#[cfg(test)]
mod win32_tests {
    use super::win32_resolve;

    #[test]
    fn resolves_as_node_win32_does() {
        let cases: serde_json::Value = serde_json::from_str(include_str!("win32_resolve_cases.json")).unwrap();
        let mut wrong = Vec::new();
        for case in cases.as_array().unwrap() {
            let parts: Vec<&str> = case[0]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().unwrap())
                .collect();
            let want = case[1].as_str().unwrap();
            let got = win32_resolve(&parts, "D:\\a\\proj", |_| None);
            if got != want {
                wrong.push(format!("{parts:?}: node {want:?}, here {got:?}"));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn a_drive_relative_path_takes_that_drives_own_directory() {
        let drive = |d: &str| (d == "E:").then(|| "E:\\envdir".to_string());
        assert_eq!(win32_resolve(&["E:rel"], "D:\\a\\proj", drive), "E:\\envdir\\rel");
        assert_eq!(win32_resolve(&["F:rel"], "D:\\a\\proj", drive), "F:\\rel");
    }
}
