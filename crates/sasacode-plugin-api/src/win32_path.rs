//! Node's `path.win32.resolve`, which the path handling on Windows follows: a path rooted without
//! a drive (`\tmp`) takes the drive of what it is resolved against. Written for every system so its
//! tests run everywhere.

/// A path from `fs::canonicalize` as Node's realpath spells it: Windows' `\\?\C:\…` and
/// `\\?\UNC\server\…` without the verbatim prefix. Other paths are returned as they are.
pub fn without_verbatim(p: &str) -> String {
    if let Some(rest) = p.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = p.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        p.to_string()
    }
}

/// A path in the form Win32 opens it: without a `\\?\`, `\\.\` or `\\?\UNC\` prefix, and each name without the
/// dots and spaces that end it (`.git.` is `.git`) or `::$DATA`, the stream that is the file itself.
pub fn win32_open_form(p: &str) -> String {
    let lower = p.replace('/', "\\").to_ascii_lowercase();
    let p = match () {
        _ if lower.starts_with(r"\\?\unc\") || lower.starts_with(r"\\.\unc\") => format!(r"\\{}", &p[8..]),
        _ if lower.starts_with(r"\\?\") || lower.starts_with(r"\\.\") => p[4..].to_string(),
        _ => p.to_string(),
    };
    let name = |n: &str| {
        let i = n.len().saturating_sub(7);
        let n = if n.get(i..).is_some_and(|t| t.eq_ignore_ascii_case("::$DATA")) {
            &n[..i]
        } else {
            n
        };
        let t = n.trim_end_matches(['.', ' ']);
        if t.is_empty() { n.to_string() } else { t.to_string() }
    };
    p.split(['\\', '/']).map(name).collect::<Vec<_>>().join("\\")
}

/// `path.win32.resolve` against this process's working directory, and the per-drive ones Windows
/// keeps in the environment (`=C:`).
pub fn win32_resolve_here(parts: &[&str]) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    win32_resolve(parts, &cwd, |drive| std::env::var(format!("={drive}")).ok())
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

/// As Node answers it with D:\a\proj for its working directory (on every
/// system: the function takes the directory it resolves against).
#[cfg(test)]
mod win32_tests {
    use super::{win32_open_form, win32_resolve, without_verbatim};

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

    #[test]
    fn verbatim_prefixes_are_dropped_as_node_realpath_spells_paths() {
        assert_eq!(without_verbatim(r"\\?\C:\x"), r"C:\x");
        assert_eq!(without_verbatim(r"\\?\UNC\server\share\x"), r"\\server\share\x");
        assert_eq!(without_verbatim("/tmp/x"), "/tmp/x");
    }

    #[test]
    fn a_path_is_spelled_as_win32_opens_it() {
        assert_eq!(win32_open_form(r"\\?\C:\p\.git.\HEAD "), r"C:\p\.git\HEAD");
        assert_eq!(win32_open_form(r"\\.\C:\p\config::$DATA"), r"C:\p\config");
        assert_eq!(win32_open_form(r"\\?\UNC\srv\sh\a. . \b"), r"\\srv\sh\a\b");
        assert_eq!(win32_open_form("//?/C:/p/.sasacode./x"), r"C:\p\.sasacode\x");
        assert_eq!(win32_open_form(r"C:\p\..\.\a:s\é."), r"C:\p\..\.\a:s\é");
        assert_eq!(win32_open_form(r"C:\p\x::$data"), r"C:\p\x");
    }
}
