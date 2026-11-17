//! The files tab: the project's files, found by what is typed, and one of them read with its syntax
//! colored, a line of it commented on as a line of a diff is. And the colors of code, which the
//! code blocks of replies are drawn in too.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use iced::highlighter::{Settings, Stream, Theme};
use iced::widget::operation;
use iced::{Color, Task};

use crate::app::{App, Msg, Tab};

/// The inspector's body, scrolled to a file's line.
pub const INSPECTOR: &str = "inspector";
/// The field that finds a file.
pub const FILES_FIELD: &str = "files-field";
/// How many files the tab lists at once, and how many lines of one it shows.
pub const LISTED: usize = 300;
pub const SHOWN_LINES: usize = 3000;
/// A file larger than this is not read.
const READ_MAX: u64 = 4 * 1024 * 1024;
/// How many code blocks keep their colors, until they are colored again.
const KEPT: usize = 128;

/// The colored stretches of a line: bytes of it, and their color if the syntax gives one.
pub type Spans = Vec<(Range<usize>, Option<Color>)>;

/// A file being read in the tab.
#[derive(Debug, Clone)]
pub struct Viewing {
    pub path: PathBuf,
    pub lines: Vec<String>,
    /// The lines' colors on a light ground and on a dark one.
    pub light: Vec<Spans>,
    pub dark: Vec<Spans>,
    /// How many lines it has, of which [`SHOWN_LINES`] are shown.
    pub total: usize,
    /// Why it is not shown: too large, not text, or not readable.
    pub error: Option<String>,
    /// A picture, shown as one.
    pub image: bool,
    /// The line to bring into view, if one was asked for.
    pub line: Option<usize>,
}

/// `lines` colored as the syntax of `token` (a file's extension, or a language's name) says,
/// for a dark ground or a light one.
pub fn highlight<S: AsRef<str>>(lines: &[S], token: &str, dark: bool) -> Vec<Spans> {
    let mut stream = Stream::new(&Settings {
        theme: if dark {
            Theme::Base16Ocean
        } else {
            Theme::InspiredGitHub
        },
        token: token.to_owned(),
    });
    lines
        .iter()
        .map(|line| {
            let line = line.as_ref();
            // The grammar reads lines with their ends.
            let with_end = format!("{line}\n");
            let spans: Spans = stream
                .highlight_line(&with_end)
                .map(|(range, h)| (range.start.min(line.len())..range.end.min(line.len()), h.color()))
                .filter(|(range, _)| !range.is_empty())
                .collect();
            stream.commit();
            spans
        })
        .collect()
}

type Colored = HashMap<(bool, String, String), Arc<Vec<Spans>>>;

/// A code block's lines colored, once: by its language, for the ground it is drawn on.
pub fn colored(code: &str, language: &str, dark: bool) -> Arc<Vec<Spans>> {
    static DONE: OnceLock<Mutex<Colored>> = OnceLock::new();
    let done = DONE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (dark, language.to_owned(), code.to_owned());
    if let Some(spans) = done.lock().ok().and_then(|d| d.get(&key).cloned()) {
        return spans;
    }
    let lines: Vec<&str> = code.lines().collect();
    let spans = Arc::new(highlight(&lines, language, dark));
    if let Ok(mut d) = done.lock() {
        if d.len() >= KEPT {
            d.clear();
        }
        d.insert(key, spans.clone());
    }
    spans
}

/// Reads `path` to be shown, its colors made for both grounds.
pub fn read(path: PathBuf, line: Option<usize>) -> Viewing {
    let mut viewing = Viewing {
        path,
        lines: Vec::new(),
        light: Vec::new(),
        dark: Vec::new(),
        total: 0,
        error: None,
        image: false,
        line,
    };
    if is_picture(&viewing.path) {
        viewing.image = true;
        return viewing;
    }
    let bytes = match std::fs::metadata(&viewing.path) {
        Ok(m) if m.len() > READ_MAX => Err(format!("{} MB", m.len() / 1024 / 1024)),
        Ok(_) => std::fs::read(&viewing.path).map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    let text = match bytes {
        Ok(bytes) if bytes.contains(&0) => Err("binary".to_owned()),
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Err(e) => Err(e),
    };
    match text {
        Ok(text) => {
            let all: Vec<&str> = text.lines().collect();
            viewing.total = all.len();
            viewing.lines = all.into_iter().take(SHOWN_LINES).map(str::to_owned).collect();
            let token = token_of(&viewing.path);
            viewing.light = highlight(&viewing.lines, &token, false);
            viewing.dark = highlight(&viewing.lines, &token, true);
        }
        Err(e) => viewing.error = Some(e),
    }
    viewing
}

/// How far down the tab line `n` of `lines` is, about: a little above it, so it is not at the edge.
fn at_line(n: usize, lines: usize) -> f32 {
    if lines < 2 {
        return 0.0;
    }
    (n.saturating_sub(6) as f32 / lines as f32).clamp(0.0, 1.0)
}

/// What names a file's syntax: its extension, else its name (`Makefile`, `Dockerfile`).
fn token_of(path: &Path) -> String {
    path.extension()
        .or_else(|| path.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The file a link in a reply names, and its line: a path in the project, `file://`, with `:12` or
/// `#L12` after it. None for a web address, or for what is not a file.
/// The picture a Markdown image names, when it is a file here: a path, absolute or from the
/// folder, or a `file://` URL. Nothing is fetched from the network.
pub fn local_image(url: &str, cwd: &Path) -> Option<PathBuf> {
    if url.contains("://") && !url.starts_with("file://") {
        return None;
    }
    let raw = url.strip_prefix("file://").unwrap_or(url);
    let path = Path::new(raw);
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    };
    (is_picture(&path) && path.is_file()).then_some(path)
}

/// Whether `path` names a picture the window can draw.
pub fn is_picture(path: &Path) -> bool {
    const PICTURES: [&str; 7] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"];
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| PICTURES.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn linked_file(uri: &str, cwd: &Path) -> Option<(PathBuf, Option<usize>)> {
    if uri.contains("://") && !uri.starts_with("file://") {
        return None;
    }
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let (raw, line) = match raw.rsplit_once("#L").or_else(|| raw.rsplit_once(':')) {
        Some((p, n)) if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => (p, n.parse().ok()),
        _ => (raw, None),
    };
    let raw = raw.replace("%20", " ");
    let path = Path::new(&raw);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    path.is_file().then_some((path, line))
}

impl App {
    /// The files tab's messages; any other comes back.
    pub(crate) fn update_files(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let task = match msg {
            Msg::ViewFile(path, line) => {
                // What is typed in a file is not left unsaved by opening another.
                if self.file_edit.as_ref().is_some_and(|e| e.changed() && e.path != path) {
                    self.toast = Some(self.s.w().edit_unsaved.to_owned());
                    return Ok(Task::none());
                }
                self.tab = Tab::Files;
                self.insp = true;
                if let Some(v) = &mut self.viewing
                    && v.path == path
                {
                    v.line = line;
                    let y = line.map_or(0.0, |n| at_line(n, v.lines.len()));
                    return Ok(operation::snap_to(INSPECTOR, operation::RelativeOffset { x: 0.0, y }));
                }
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || read(path, line))
                            .await
                            .ok()
                            .map(Box::new)
                    },
                    |v| match v {
                        Some(v) => Msg::FileRead(v),
                        None => Msg::CloseFile,
                    },
                )
            }
            Msg::FileRead(viewing) => {
                let at = viewing.line.map(|n| at_line(n, viewing.lines.len()));
                self.file_preview = None;
                self.file_edit = None;
                self.viewing = Some(*viewing);
                match at {
                    Some(y) => operation::snap_to(INSPECTOR, operation::RelativeOffset { x: 0.0, y }),
                    None => operation::snap_to(INSPECTOR, operation::RelativeOffset::START),
                }
            }
            Msg::CloseFile => {
                if self.file_edit.as_ref().is_some_and(crate::edit::FileEdit::changed) {
                    self.toast = Some(self.s.w().edit_unsaved.to_owned());
                    return Ok(Task::none());
                }
                self.viewing = None;
                self.file_preview = None;
                self.file_edit = None;
                operation::focus(FILES_FIELD)
            }
            Msg::FileQuery(query) => {
                self.file_query = query;
                // What was found was for what was typed before.
                if self.file_grep {
                    self.grep_hits = None;
                    self.grep_busy = false;
                }
                Task::none()
            }
            other => return self.update_edit(other),
        };
        Ok(task)
    }

    /// The project's files, asked for once, for the files tab.
    pub(crate) fn list_files(&mut self) -> Task<Msg> {
        if self.files_asked {
            return Task::none();
        }
        self.files_asked = true;
        let cwd = self.cwd.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || crate::composer::project_files(&cwd))
                    .await
                    .unwrap_or_default()
            },
            Msg::Files,
        )
    }

    /// The project's files that have every word typed, folders left out.
    pub(crate) fn found_files(&self) -> impl Iterator<Item = &String> {
        let words: Vec<String> = self.file_query.split_whitespace().map(str::to_lowercase).collect();
        self.files.iter().filter(|f| !f.ends_with('/')).filter(move |f| {
            let f = f.to_lowercase();
            words.iter().all(|w| f.contains(w))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_colored_by_its_syntax_and_the_colors_cover_it() {
        let lines = ["fn main() {", "    let x = \"文字\";", "}"];
        for dark in [false, true] {
            let spans = highlight(&lines, "rs", dark);
            assert_eq!(spans.len(), 3);
            for (line, spans) in lines.iter().zip(&spans) {
                let covered: usize = spans.iter().map(|(r, _)| r.len()).sum();
                assert_eq!(covered, line.len());
                assert!(spans.iter().all(|(r, _)| line.get(r.clone()).is_some()));
            }
            // A keyword and a string are not the same color.
            let color_at = |line: usize, byte: usize| spans[line].iter().find(|(r, _)| r.contains(&byte)).unwrap().1;
            assert_ne!(color_at(0, 0), color_at(1, 13));
        }
    }

    #[test]
    fn a_link_names_a_file_with_its_line_but_not_a_web_address() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-files-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "x").unwrap();
        assert_eq!(linked_file("src/a.rs", &dir), Some((dir.join("src/a.rs"), None)));
        assert_eq!(linked_file("src/a.rs:12", &dir), Some((dir.join("src/a.rs"), Some(12))));
        let abs = format!("file://{}#L3", dir.join("src/a.rs").display());
        assert_eq!(linked_file(&abs, &dir), Some((dir.join("src/a.rs"), Some(3))));
        assert_eq!(linked_file("https://example.com/src/a.rs", &dir), None);
        assert_eq!(linked_file("src/none.rs", &dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_is_not_text_or_too_large_says_why() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.bin"), [0u8, 1, 2]).unwrap();
        std::fs::write(dir.join("t.py"), "def f():\n    return 1\n").unwrap();
        assert_eq!(read(dir.join("b.bin"), None).error.as_deref(), Some("binary"));
        let t = read(dir.join("t.py"), Some(2));
        assert_eq!((t.total, t.lines.len(), t.light.len(), t.dark.len()), (2, 2, 2, 2));
        assert!(read(dir.join("missing"), None).error.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
