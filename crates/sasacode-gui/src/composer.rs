//! What the input offers while it is typed in: files after `@`, the window's commands after `/`,
//! and what was sent before. The list of what was sent is the TUI's (`history.jsonl` in sasacode's
//! home), so both remember the same.

use std::path::{Path, PathBuf};

/// How many of the files a list offers at once.
pub const SHOWN: usize = 8;
/// How many sent messages are remembered, as the TUI remembers them.
const HISTORY: usize = 200;
/// How many files a folder that is not a git repository is searched for, at most.
const WALKED: usize = 20_000;

/// The `@word` the cursor is at the end of, in the text of its line before it: where its `@` is,
/// and what follows it. An `@` counts at the start of the line or after a space, so an address
/// like `me@example.com` offers nothing.
pub fn mention(before: &str) -> Option<(usize, &str)> {
    let at = before.rfind('@')?;
    let query = &before[at + 1..];
    if query.chars().any(char::is_whitespace) || query.contains('"') {
        return None;
    }
    let lead = before[..at].chars().next_back();
    (lead.is_none() || lead.is_some_and(char::is_whitespace)).then_some((at, query))
}

/// The `/word` a message starts with while it is still being typed: the cursor on its first line,
/// with nothing but the word before it.
pub fn slash(line: usize, before: &str) -> Option<&str> {
    let word = before.strip_prefix('/')?;
    (line == 0 && !word.chars().any(char::is_whitespace)).then_some(word)
}

/// The files and folders that best match `query`, at most `max`: those whose name starts with it,
/// then those whose name has it, then those whose path has it, the shorter path first. Case is not
/// minded.
pub fn rank<'a>(files: &'a [String], query: &str, max: usize) -> Vec<&'a str> {
    let needle = query.to_lowercase();
    let mut found: Vec<(u8, usize, &str)> = files
        .iter()
        .filter_map(|path| {
            let lower = path.to_lowercase();
            let name = lower.trim_end_matches('/').rsplit('/').next().unwrap_or(&lower);
            let tier = if needle.is_empty() || name.starts_with(&needle) {
                0
            } else if name.contains(&needle) {
                1
            } else if lower.contains(&needle) {
                2
            } else {
                return None;
            };
            Some((tier, path.len(), path.as_str()))
        })
        .collect();
    found.sort();
    found.into_iter().take(max).map(|(_, _, path)| path).collect()
}

/// The files of the project at `cwd` and the folders they are in, relative to it, folders ending
/// in `/`: what git tracks or would track, else what a walk of the folder finds, hidden folders,
/// `node_modules` and `target` left out. Git is not asked unless `git`.
pub fn project_files(cwd: &Path, git: bool) -> Vec<String> {
    let mut files = git.then(|| git_files(cwd)).flatten().unwrap_or_else(|| walk(cwd));
    files.sort();
    let mut folders: Vec<String> = files
        .iter()
        .flat_map(|f| f.match_indices('/').map(move |(i, _)| f[..=i].to_owned()))
        .collect();
    folders.sort();
    folders.dedup();
    files.extend(folders);
    files
}

fn git_files(cwd: &Path) -> Option<Vec<String>> {
    let out = crate::git::command()
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let all = String::from_utf8_lossy(&out.stdout);
    Some(all.split('\0').filter(|f| !f.is_empty()).map(str::to_owned).collect())
}

fn walk(cwd: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut todo = vec![cwd.to_path_buf()];
    while let Some(dir) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !name.starts_with('.') && name != "node_modules" && name != "target" {
                    todo.push(path);
                }
            } else if let Ok(rel) = path.strip_prefix(cwd) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
                if out.len() >= WALKED {
                    return out;
                }
            }
        }
    }
    out
}

/// A path as the input takes it: `@` and the path relative to the project when it is inside it,
/// else the whole path, which the image plugin takes too. Quoted when it has a space. Inside also
/// when the two are written different ways to the same place, as git writes the repository's folder
/// without the links on the way to it (`/private/var/…` for `/var/…` on macOS).
pub fn path_ref(path: &Path, cwd: &Path) -> String {
    let inside = path.strip_prefix(cwd).ok().map(Path::to_path_buf).or_else(|| {
        let real_cwd = cwd.canonicalize().ok()?;
        let real = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        real.strip_prefix(&real_cwd).ok().map(Path::to_path_buf)
    });
    let shown = match inside {
        Some(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().replace('\\', "/"),
        _ => path.to_string_lossy().into_owned(),
    };
    let shown = if path.is_dir() && !shown.ends_with('/') {
        format!("{shown}/")
    } else {
        shown
    };
    at_ref(&shown)
}

/// The extensions of the images an `@` brings to the agent.
const IMAGE: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "bmp"];

/// The images `text` names that are there, each with how it is written in it: `@path`,
/// `@"path with space"`, or as a message read back from its log says one was sent,
/// `[Image 1: path]`. Relative to `cwd`.
pub fn image_refs(text: &str, cwd: &Path) -> Vec<(PathBuf, String)> {
    let mut found: Vec<(PathBuf, String)> = Vec::new();
    let mut put = |raw: &str, written: &str| {
        let path = Path::new(raw);
        let is_image = path
            .extension()
            .is_some_and(|e| IMAGE.contains(&e.to_string_lossy().to_lowercase().as_str()));
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        if is_image && path.is_file() && !found.iter().any(|(p, _)| *p == path) {
            found.push((path, written.to_owned()));
        }
    };
    let mut rest = text;
    while let Some(at) = rest.find('@') {
        let after = &rest[at + 1..];
        let before_ok = at == 0 || rest[..at].ends_with(char::is_whitespace);
        if let Some(quoted) = after.strip_prefix('"')
            && let Some(end) = quoted.find('"')
        {
            if before_ok {
                put(&quoted[..end], &rest[at..at + end + 3]);
            }
            rest = &quoted[end + 1..];
            continue;
        }
        let end = after.find(char::is_whitespace).unwrap_or(after.len());
        if before_ok && end > 0 {
            put(&after[..end], &rest[at..at + 1 + end]);
        }
        rest = &after[end..];
    }
    let mut rest = text;
    while let Some(at) = rest.find("[Image ") {
        let after = &rest[at..];
        let Some(close) = after.find(']') else { break };
        if let Some((_, path)) = after[..close].split_once(": ") {
            put(path.trim(), &after[..=close]);
        }
        rest = &after[close + 1..];
    }
    found
}

/// `@path`, quoted when the path has a space.
pub fn at_ref(path: &str) -> String {
    if path.chars().any(char::is_whitespace) {
        format!("@\"{path}\"")
    } else {
        format!("@{path}")
    }
}

/// What was sent before, oldest first: the last ones the file holds, one JSON string a line.
pub fn read_history(file: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let all: Vec<String> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<String>(line).ok())
        .collect();
    all[all.len().saturating_sub(HISTORY)..].to_vec()
}

/// Adds `sent` to the file, readable by its owner only, as the TUI writes it.
pub fn append_history(file: &Path, sent: &str) {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = options.open(file) {
        let _ = writeln!(f, "{}", serde_json::Value::String(sent.to_owned()));
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_images_a_message_names_are_found_with_how_they_are_written() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-images-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["a.png", "b c.jpg", "n.txt"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        let text = "見て @a.png と @\"b c.jpg\"、@n.txt と @gone.png、mail@a.png\n[Image 1: a.png]";
        let found = image_refs(text, &dir);
        assert_eq!(
            found,
            vec![
                (dir.join("a.png"), "@a.png".to_owned()),
                (dir.join("b c.jpg"), "@\"b c.jpg\"".to_owned()),
            ]
        );
        let logged = image_refs(&format!("[Image 1: {}] これ", dir.join("a.png").display()), &dir);
        assert_eq!(logged.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
    use super::*;

    #[test]
    fn a_mention_is_an_at_sign_at_a_word_start() {
        assert_eq!(mention("@"), Some((0, "")));
        assert_eq!(mention("見て @src/ap"), Some((7, "src/ap")));
        assert_eq!(mention("me@example.com"), None);
        assert_eq!(mention("@src/a.rs と"), None);
        assert_eq!(mention("@\"a b"), None);
        assert_eq!(slash(0, "/ne"), Some("ne"));
        assert_eq!(slash(0, "/new "), None);
        assert_eq!(slash(1, "/new"), None);
        assert_eq!(slash(0, "a/new"), None);
    }

    #[test]
    fn files_are_ranked_by_where_the_query_is() {
        let files: Vec<String> = [
            "src/app.rs",
            "src/view/apply.rs",
            "docs/app-notes.md",
            "src/",
            "README.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            rank(&files, "app", 8),
            vec!["src/app.rs", "docs/app-notes.md", "src/view/apply.rs"]
        );
        assert_eq!(rank(&files, "PLY", 2), vec!["src/view/apply.rs"]);
        assert_eq!(rank(&files, "notes", 8), vec!["docs/app-notes.md"]);
        assert_eq!(rank(&files, "view", 8), vec!["src/view/apply.rs"]);
        assert_eq!(rank(&files, "src", 1), vec!["src/"]);
    }

    #[test]
    fn a_path_is_given_relative_and_quoted_when_it_must_be() {
        let cwd = Path::new("/work/p");
        assert_eq!(path_ref(Path::new("/work/p/src/a.rs"), cwd), "@src/a.rs");
        assert_eq!(path_ref(Path::new("/tmp/shot 1.png"), cwd), "@\"/tmp/shot 1.png\"");
    }

    /// git names the repository's folder by where it really is; the window may have been opened on
    /// it through a link.
    #[cfg(unix)]
    #[test]
    fn a_path_is_relative_when_the_project_was_opened_through_a_link() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let real = dir.join("real");
        std::fs::create_dir_all(real.join("notes")).unwrap();
        std::fs::write(real.join("notes/a.md"), "a").unwrap();
        std::os::unix::fs::symlink(&real, dir.join("link")).unwrap();
        let real = real.canonicalize().unwrap();
        assert_eq!(path_ref(&real.join("notes/a.md"), &dir.join("link")), "@notes/a.md");
        // One that is gone (a deleted file in the diff) too.
        assert_eq!(
            path_ref(&real.join("notes/gone.md"), &dir.join("link")),
            "@notes/gone.md"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_was_sent_is_shared_with_the_tui() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("history.jsonl");
        assert!(read_history(&file).is_empty());
        append_history(&file, "一つ目");
        append_history(&file, "二行\nある");
        std::fs::write(&file, std::fs::read_to_string(&file).unwrap() + "not json\n").unwrap();
        assert_eq!(read_history(&file), vec!["一つ目", "二行\nある"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
