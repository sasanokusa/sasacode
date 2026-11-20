//! The files tab, beyond reading: a file edited and saved, a Markdown file shown as it reads, and
//! the project's files searched by what they hold (`git grep`).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use iced::Task;
use iced::widget::text_editor;

use crate::app::{App, Msg};
use crate::transcript::Markdown;

/// How many lines a search lists.
pub const HITS: usize = 300;

/// A file being edited in the tab.
pub struct FileEdit {
    pub path: PathBuf,
    pub content: text_editor::Content,
    /// The file as it was read, to tell whether another wrote it since.
    pub original: String,
    /// What the editor held before anything was typed, to tell whether anything was.
    pub initial: String,
    /// When something was last typed and not yet saved by itself.
    pub typed_at: Option<Instant>,
    /// Saved by itself while it stays open: what the tab shows is read again when it closes.
    pub saved: bool,
}

/// How long after the last key the file is saved, when the settings save edits by themselves.
pub const AUTOSAVE_AFTER: Duration = Duration::from_secs(1);

impl std::fmt::Debug for FileEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileEdit")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl FileEdit {
    /// Something was typed.
    pub fn changed(&self) -> bool {
        self.content.text() != self.initial
    }
}

/// A line a search found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// From the folder.
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// What `git grep -n -z` printed: a file, its line's number, the line.
pub fn parse_hits(out: &str) -> Vec<Hit> {
    out.lines()
        .filter_map(|l| {
            let mut parts = l.splitn(3, '\0');
            let (path, line, text) = (parts.next()?, parts.next()?, parts.next()?);
            Some(Hit {
                path: path.to_owned(),
                line: line.parse().ok()?,
                text: text.trim().chars().take(240).collect(),
            })
        })
        .take(HITS)
        .collect()
}

/// The lines of the folder's files that hold `query`, as git finds them: those it tracks and
/// those it does not ignore, or, outside a repository, all. Case is told apart when the query
/// has a capital.
pub async fn grep(cwd: PathBuf, query: String) -> Result<Vec<Hit>, String> {
    let run = |outside: bool| {
        let mut c = crate::git::async_command();
        #[cfg(windows)]
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c.args(["grep", "-n", "-I", "-z", "--no-color", "-F"]);
        if !query.chars().any(char::is_uppercase) {
            c.arg("-i");
        }
        if outside {
            c.arg("--no-index");
        } else {
            c.arg("--untracked");
        }
        c.arg("-e")
            .arg(&query)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
    };
    let mut out = run(false).await.map_err(|e| format!("git: {e}"))?;
    if out.status.code() == Some(128) {
        out = run(true).await.map_err(|e| format!("git: {e}"))?;
    }
    match out.status.code() {
        Some(0) => Ok(parse_hits(&String::from_utf8_lossy(&out.stdout))),
        // Nothing found.
        Some(1) => Ok(Vec::new()),
        _ => Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()),
    }
}

/// `text` as it is written back over `original`: its line ends, and an end of line at its end
/// when the file had one.
pub fn as_written(text: &str, original: &str) -> String {
    let mut all = text.replace("\r\n", "\n");
    if original.ends_with('\n') && !all.ends_with('\n') {
        all.push('\n');
    }
    if original.contains("\r\n") {
        all = all.replace('\n', "\r\n");
    }
    all
}

/// Whether `path` names a Markdown file.
pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdx"))
}

impl App {
    /// What is typed in the file written to it, the editor left open: the settings save edits by
    /// themselves. Not written over what another wrote since it was read; told so instead.
    pub(crate) fn autosave(&mut self) -> Task<Msg> {
        let w = self.s.w();
        let Some(edit) = &mut self.file_edit else {
            return Task::none();
        };
        edit.typed_at = None;
        if !edit.changed() {
            return Task::none();
        }
        if std::fs::read_to_string(&edit.path).ok().as_deref() != Some(edit.original.as_str()) {
            self.toast = Some(w.edit_changed.to_owned());
            return Task::none();
        }
        let text = as_written(&edit.content.text(), &edit.original);
        match std::fs::write(&edit.path, &text) {
            Ok(()) => {
                edit.original = text;
                edit.initial = edit.content.text();
                edit.saved = true;
                // The git tab shows the change.
                self.files_asked = false;
                self.refresh_git()
            }
            Err(e) => {
                self.toast = Some(format!("{}: {e}", edit.path.display()));
                Task::none()
            }
        }
    }

    /// The editing, preview and search messages; any other comes back.
    pub(crate) fn update_edit(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let w = self.s.w();
        let task = match msg {
            Msg::FileEditStart => {
                let Some(path) = self.viewing.as_ref().map(|v| v.path.clone()) else {
                    return Ok(Task::none());
                };
                match std::fs::read_to_string(&path) {
                    Ok(original) => {
                        let content = text_editor::Content::with_text(&original);
                        let initial = content.text();
                        self.file_preview = None;
                        self.file_edit = Some(FileEdit {
                            path,
                            content,
                            original,
                            initial,
                            typed_at: None,
                            saved: false,
                        });
                    }
                    Err(e) => self.toast = Some(format!("{}: {e}", path.display())),
                }
                Task::none()
            }
            Msg::FileEditAction(action) => {
                if let Some(edit) = &mut self.file_edit {
                    if action.is_edit() {
                        edit.typed_at = Some(Instant::now());
                    }
                    edit.content.perform(action);
                }
                Task::none()
            }
            Msg::FileAutosave => {
                if self
                    .file_edit
                    .as_ref()
                    .and_then(|e| e.typed_at)
                    .is_some_and(|at| at.elapsed() >= AUTOSAVE_AFTER)
                {
                    return Ok(self.autosave());
                }
                Task::none()
            }
            Msg::FileEditCancel => {
                // Saved by itself, it is closed with what was typed last saved too, and read again.
                let saving = self.prefs.more.autosave;
                let task = if saving { self.autosave() } else { Task::none() };
                if saving && self.file_edit.as_ref().is_some_and(FileEdit::changed) {
                    // Not written (another wrote it since): left open, as told.
                    return Ok(task);
                }
                let Some(edit) = self.file_edit.take() else {
                    return Ok(task);
                };
                if edit.saved {
                    let line = self.viewing.as_ref().and_then(|v| v.line);
                    let now = crate::files::read(edit.path, line);
                    let shown = self.update(Msg::FileRead(Box::new(now)));
                    return Ok(Task::batch([task, shown]));
                }
                task
            }
            Msg::FileSave => {
                let Some(edit) = &self.file_edit else {
                    return Ok(Task::none());
                };
                if !edit.changed() {
                    // Closed, and read again when it was saved by itself.
                    return self.update_edit(Msg::FileEditCancel);
                }
                // Written by another since it was read: not written over.
                if std::fs::read_to_string(&edit.path).ok().as_deref() != Some(edit.original.as_str()) {
                    self.toast = Some(w.edit_changed.to_owned());
                    return Ok(Task::none());
                }
                let text = as_written(&edit.content.text(), &edit.original);
                let path = edit.path.clone();
                match std::fs::write(&path, text) {
                    Ok(()) => {
                        self.file_edit = None;
                        self.toast = Some(w.edit_saved.to_owned());
                        // Read again, as it is now (the same file is not read by `ViewFile`);
                        // the git tab shows the change.
                        self.files_asked = false;
                        let line = self.viewing.as_ref().and_then(|v| v.line);
                        let now = crate::files::read(path, line);
                        let shown = self.update(Msg::FileRead(Box::new(now)));
                        return Ok(Task::batch([shown, self.refresh_git()]));
                    }
                    Err(e) => {
                        self.toast = Some(format!("{}: {e}", path.display()));
                        Task::none()
                    }
                }
            }
            Msg::FilePreview(on) => {
                self.file_preview = None;
                if on
                    && let Some(v) = &self.viewing
                    && let Ok(text) = std::fs::read_to_string(&v.path)
                {
                    self.file_preview = Some(Markdown::new(&text));
                }
                Task::none()
            }
            Msg::FileGrepMode(on) => {
                self.file_grep = on;
                iced::widget::operation::focus(crate::files::FILES_FIELD)
            }
            Msg::FileGrep => {
                let query = self.file_query.trim().to_owned();
                if query.is_empty() {
                    self.grep_hits = None;
                    return Ok(Task::none());
                }
                self.grep_busy = true;
                let cwd = self.cwd.clone();
                let asked = query.clone();
                Task::perform(grep(cwd, query), move |found| Msg::Grepped(asked.clone(), found))
            }
            Msg::Grepped(query, found) => {
                // An answer to a search typed over since is not shown.
                if query == self.file_query.trim() {
                    self.grep_busy = false;
                    match found {
                        Ok(hits) => self.grep_hits = Some(hits),
                        Err(e) => {
                            self.grep_hits = None;
                            self.toast = Some(e);
                        }
                    }
                }
                Task::none()
            }
            other => return self.update_scripts(other),
        };
        Ok(task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_git_grep_prints_is_read_into_lines_found() {
        let out = ["src/a.rs", "12", "    let x = 1;\nsrc/b c.rs", "3", "fn f()\n"].join("\0");
        let hits = parse_hits(&out);
        assert_eq!(
            hits,
            [
                Hit {
                    path: "src/a.rs".into(),
                    line: 12,
                    text: "let x = 1;".into()
                },
                Hit {
                    path: "src/b c.rs".into(),
                    line: 3,
                    text: "fn f()".into()
                },
            ]
        );
    }

    #[test]
    fn an_edited_file_keeps_its_line_ends() {
        assert_eq!(as_written("a\nb", "a\nc\n"), "a\nb\n");
        assert_eq!(as_written("a\nb\n", "a\r\nc\r\n"), "a\r\nb\r\n");
        assert_eq!(as_written("a", "b"), "a");
        // The editor gives the text it was given back as it was.
        for original in ["a\nb\n", "a\nb", "", "\n"] {
            let content: text_editor::Content = text_editor::Content::with_text(original);
            assert_eq!(as_written(&content.text(), original), original, "{original:?}");
        }
    }

    #[tokio::test]
    async fn the_folders_files_are_searched_by_what_they_hold() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-grep-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn main() {}\nlet Needle = 1;\n").unwrap();
        std::fs::write(dir.join("b.txt"), "a needle here\n").unwrap();
        // Outside a repository, every file; no capital, any case.
        let mut hits = grep(dir.clone(), "needle".into()).await.unwrap();
        hits.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!((hits[1].path.as_str(), hits[1].line), ("src/a.rs", 2));
        // A capital tells case apart.
        let hits = grep(dir.clone(), "Needle".into()).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!(grep(dir.clone(), "nowhere-at-all".into()).await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
