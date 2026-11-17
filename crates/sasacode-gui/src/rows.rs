//! What the list does to many conversations at once, and takes back: the menu a right click opens
//! on a row or a file, rows chosen together, which rows are shown and in which order, logs deleted
//! into a trash they come back from, and conversations archived for their age.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use iced::widget::operation;
use iced::{Point, Task};
use sasacode_protocol::SessionSummary;

use crate::app::{App, Msg, Overlay};
use crate::i18n::fill;
use crate::layout::{Prefs, Show};

/// Where the pointer last was over the window: a menu opens there.
static POINTER: Mutex<Point> = Mutex::new(Point::ORIGIN);

/// Keeps where the pointer is, and says nothing of it: the window is not drawn again as it moves.
pub(crate) fn pointer(event: iced::Event, _status: iced::event::Status, _window: iced::window::Id) -> Option<Msg> {
    if let iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) = event
        && let Ok(mut at) = POINTER.lock()
    {
        *at = position;
    }
    None
}

pub(crate) fn pointer_at() -> Point {
    POINTER.lock().map(|at| *at).unwrap_or(Point::ORIGIN)
}

/// What a menu was opened on.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuFor {
    /// A conversation's row, by session id.
    Row(String),
    /// A file of the folder: in the files tab, or a changed file of the git tab.
    File(PathBuf),
    /// The models the CLI listed, for the next run.
    Models,
    /// The links of a paragraph of a reply, as written.
    Links(Vec<String>),
    /// How the git tab draws its diffs.
    DiffView,
}

/// A menu opened by a right click, where the pointer was.
#[derive(Debug, Clone)]
pub struct Menu {
    pub at: Point,
    pub what: MenuFor,
}

/// What many conversations are done to at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bulk {
    Pin,
    Archive,
    Unread,
    Group,
    Delete,
}

/// What the list said of its conversations, before a change that can be taken back.
#[derive(Debug, Clone, PartialEq)]
pub struct Listing {
    pinned: Vec<String>,
    archived: BTreeSet<String>,
    groups: BTreeMap<String, String>,
    names: BTreeMap<String, String>,
    branches: BTreeMap<String, String>,
    unread: BTreeSet<String>,
    kept: BTreeSet<String>,
}

impl Listing {
    pub fn of(p: &Prefs) -> Listing {
        Listing {
            pinned: p.pinned.clone(),
            archived: p.archived.clone(),
            groups: p.groups.clone(),
            names: p.names.clone(),
            branches: p.branches.clone(),
            unread: p.more.unread.clone(),
            kept: p.more.kept.clone(),
        }
    }

    fn put(self, p: &mut Prefs) {
        p.pinned = self.pinned;
        p.archived = self.archived;
        p.groups = self.groups;
        p.names = self.names;
        p.branches = self.branches;
        p.more.unread = self.unread;
        p.more.kept = self.kept;
    }
}

/// A change of the list that can be taken back: what the list said before it, the logs it moved
/// to the trash, and the conversations it archived for their age, which are then kept.
#[derive(Debug, Clone)]
pub struct Undo {
    pub before: Listing,
    pub trashed: Vec<(PathBuf, PathBuf)>,
    pub keep: Vec<String>,
}

/// How many changes are kept to take back.
const UNDOS: usize = 30;
/// How long a deleted log stays in the trash.
const TRASH_DAYS: u64 = 7;
/// Chosen together, deleted together: the mark of the bulk delete asked once.
pub const BULK: &str = "\u{0}bulk";

/// The trash of deleted logs, under the sasacode folder.
fn trash_dir(home: &Path) -> PathBuf {
    home.join("trash")
}

/// Moves a log into the trash, under a name no other has there: where it went.
pub fn to_trash(home: &Path, log: &Path) -> std::io::Result<PathBuf> {
    let dir = trash_dir(home);
    std::fs::create_dir_all(&dir)?;
    let name = log
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut to = dir.join(&name);
    let mut n = 1;
    while to.exists() {
        to = dir.join(format!("{n}-{name}"));
        n += 1;
    }
    if std::fs::rename(log, &to).is_err() {
        // Across disks a rename fails: a copy, then the log removed.
        std::fs::copy(log, &to)?;
        std::fs::remove_file(log)?;
    }
    Ok(to)
}

/// Empties the trash of what was deleted more than a week ago.
pub fn purge_trash(home: &Path) {
    let Ok(dir) = std::fs::read_dir(trash_dir(home)) else {
        return;
    };
    let old = Duration::from_secs(TRASH_DAYS * 24 * 60 * 60);
    for entry in dir.filter_map(Result::ok) {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|when| SystemTime::now().duration_since(when).ok())
            .is_some_and(|age| age > old);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

impl App {
    /// Keeps what the list says now, to take the change about to be made back.
    pub(crate) fn remember(&mut self) {
        self.remember_with(Vec::new(), Vec::new(), Listing::of(&self.prefs));
    }

    pub(crate) fn remember_with(&mut self, trashed: Vec<(PathBuf, PathBuf)>, keep: Vec<String>, before: Listing) {
        self.undo.push(Undo { before, trashed, keep });
        if self.undo.len() > UNDOS {
            self.undo.remove(0);
        }
    }

    /// Says what was done, with a button that takes it back.
    pub(crate) fn told_undoable(&mut self, what: String) {
        self.toast = Some(what.clone());
        self.undo_toast = Some(what);
    }

    /// Takes the last change of the list back: logs come out of the trash, and the list says what
    /// it said before.
    fn undo_last(&mut self) -> Task<Msg> {
        let w = self.s.w();
        let Some(undo) = self.undo.pop() else {
            self.toast = Some(w.undo_none.to_owned());
            return Task::none();
        };
        let mut failed = None;
        for (log, trashed) in &undo.trashed {
            if log.exists() {
                continue;
            }
            if let Some(dir) = log.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::rename(trashed, log) {
                failed = Some(format!("{}: {e}", log.display()));
            }
        }
        let keep = undo.keep;
        let before = undo.before;
        let _ = self.set(|p| {
            before.put(p);
            p.more.kept.extend(keep);
        });
        // Logs back from the trash are listed again.
        if !undo.trashed.is_empty() {
            self.reload_sessions();
        }
        self.undo_toast = None;
        self.toast = Some(failed.unwrap_or_else(|| w.undone.to_owned()));
        Task::none()
    }

    /// Whether the list shows `session`, as its filter says.
    pub(crate) fn shows(&self, session: &SessionSummary) -> bool {
        let chat = self.chat_of(&session.id);
        // A scratch conversation is not listed: it goes once it is left.
        if chat.is_some_and(|c| c.scratch) {
            return false;
        }
        match self.prefs.more.list_show {
            Show::All => true,
            Show::Running => chat.is_some_and(|c| c.running()),
            Show::Waiting => chat.is_some_and(|c| c.running() && c.transcript.waits()),
            Show::Unread => self.prefs.more.unread.contains(&session.id) || chat.is_some_and(|c| c.unread),
            Show::Done => !chat.is_some_and(|c| c.running()),
        }
    }

    /// The pull request of the branch `id` was started on, as last read: its number and state.
    pub(crate) fn pr_of(&self, id: &str) -> Option<&(u64, String)> {
        self.prs.get(self.prefs.branches.get(id)?)
    }

    /// Archives the conversations not written to for as many days as the settings say: never a
    /// pinned one, one taken back from the archive, the one shown, or one that runs.
    pub(crate) fn archive_idle(&mut self) {
        let Some(days) = self.prefs.more.archive_after else {
            return;
        };
        let old = Duration::from_secs(u64::from(days) * 24 * 60 * 60);
        let now = SystemTime::now();
        let idle: Vec<String> = self
            .sessions
            .iter()
            .filter(|s| now.duration_since(s.modified).is_ok_and(|age| age > old))
            .filter(|s| {
                let p = &self.prefs;
                !p.archived.contains(&s.id)
                    && !p.pinned.contains(&s.id)
                    && !p.more.kept.contains(&s.id)
                    && self.chat.session.as_deref() != Some(s.id.as_str())
                    && !self.chat_of(&s.id).is_some_and(|c| c.busy())
            })
            .map(|s| s.id.clone())
            .collect();
        if idle.is_empty() {
            return;
        }
        let before = Listing::of(&self.prefs);
        self.remember_with(Vec::new(), idle.clone(), before);
        let n = idle.len();
        let _ = self.set(|p| p.archived.extend(idle));
        self.told_undoable(fill(self.s.w().archived_idle, "n", n));
    }

    /// Deletes the logs of `ids`, into the trash: not one that runs. The one shown, if deleted, gives
    /// its place to a new conversation.
    fn delete_many(&mut self, ids: &[String]) -> Task<Msg> {
        let w = self.s.w();
        let Some(home) = self.home.clone() else {
            return Task::none();
        };
        let before = Listing::of(&self.prefs);
        let mut trashed = Vec::new();
        let mut gone = Vec::new();
        for id in ids {
            if self.chat_of(id).is_some_and(|c| c.busy()) {
                continue;
            }
            let Some(path) = self.sessions.iter().find(|s| s.id == *id).map(|s| s.path.clone()) else {
                continue;
            };
            match to_trash(&home, &path) {
                Ok(to) => {
                    trashed.push((path.clone(), to));
                    self.bodies.remove(&path);
                    gone.push(id.clone());
                }
                Err(e) => self.toast = Some(format!("{}: {e}", path.display())),
            }
        }
        if gone.is_empty() {
            return Task::none();
        }
        for id in &gone {
            self.forget(id);
        }
        self.remember_with(trashed, Vec::new(), before);
        self.told_undoable(if gone.len() == 1 {
            w.deleted.to_owned()
        } else {
            fill(w.deleted_n, "n", gone.len())
        });
        if self.chat.session.as_ref().is_some_and(|id| gone.contains(id)) {
            self.chat = self.new_chat();
            return operation::focus(crate::app::INPUT);
        }
        Task::none()
    }

    /// The rows' messages; any other comes back.
    pub(crate) fn update_rows(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let w = self.s.w();
        let task = match msg {
            Msg::OpenMenu(what) => {
                self.menu = Some(Menu { at: pointer_at(), what });
                Task::none()
            }
            Msg::CloseMenu => {
                self.menu = None;
                Task::none()
            }
            Msg::MenuDo(msg) => {
                self.menu = None;
                return Ok(self.update(*msg));
            }
            Msg::Select(id) => {
                if !self.selected.remove(&id) {
                    self.selected.insert(id);
                }
                self.confirm_delete = None;
                Task::none()
            }
            Msg::SelectNone => {
                self.selected.clear();
                self.confirm_delete = None;
                Task::none()
            }
            Msg::Bulk(what) => self.bulk(what),
            Msg::ListShow(show) => {
                let _ = self.set(|p| p.more.list_show = show);
                Task::none()
            }
            Msg::ListOrder(order) => {
                let _ = self.set(|p| p.more.list_order = order);
                Task::none()
            }
            Msg::ListTools => {
                self.list_tools = !self.list_tools;
                Task::none()
            }
            Msg::SetArchiveAfter(days) => {
                let _ = self.set(|p| p.more.archive_after = days);
                self.archive_idle();
                Task::none()
            }
            Msg::ReadAll => {
                self.remember();
                for chat in std::iter::once(&mut self.chat).chain(self.parked.iter_mut()) {
                    chat.unread = false;
                }
                let _ = self.set(|p| p.more.unread.clear());
                Task::none()
            }
            Msg::RowTui(id) => {
                if !crate::system::resume_in_terminal(&self.cwd, &self.program, &id) {
                    self.toast = Some(w.tui_failed.to_owned());
                }
                Task::none()
            }
            Msg::Undo => self.undo_last(),
            // ⌘Z takes a change of the list back only while no field has the keyboard: there it
            // is the field's.
            Msg::UndoKey => iced::advanced::widget::operate(AnyFocused(false)).map(Msg::UndoKeyHeld),
            Msg::UndoKeyHeld(held) => {
                if held {
                    Task::none()
                } else {
                    self.undo_last()
                }
            }
            other => return self.update_talk(other),
        };
        Ok(task)
    }

    fn bulk(&mut self, what: Bulk) -> Task<Msg> {
        let w = self.s.w();
        // In the list's order, those still in it.
        let ids: Vec<String> = self
            .sessions
            .iter()
            .filter(|s| self.selected.contains(&s.id))
            .map(|s| s.id.clone())
            .collect();
        if ids.is_empty() {
            self.selected.clear();
            return Task::none();
        }
        match what {
            Bulk::Pin => {
                self.remember();
                let all = ids.iter().all(|id| self.prefs.pinned.contains(id));
                let _ = self.set(|p| {
                    if all {
                        p.pinned.retain(|x| !ids.contains(x));
                    } else {
                        for id in &ids {
                            if !p.pinned.contains(id) {
                                p.pinned.push(id.clone());
                            }
                        }
                    }
                });
            }
            Bulk::Archive => {
                self.remember();
                let back = self.show_archived;
                let _ = self.set(|p| {
                    for id in &ids {
                        if back {
                            p.archived.remove(id);
                            p.more.kept.insert(id.clone());
                        } else {
                            p.pinned.retain(|x| x != id);
                            p.archived.insert(id.clone());
                        }
                    }
                });
                self.selected.clear();
                self.told_undoable(fill(
                    if back { w.unarchived_n } else { w.archived_n_done },
                    "n",
                    ids.len(),
                ));
            }
            Bulk::Unread => {
                self.remember();
                let unread = ids
                    .iter()
                    .any(|id| !self.prefs.more.unread.contains(id) && !self.chat_of(id).is_some_and(|c| c.unread));
                for chat in std::iter::once(&mut self.chat).chain(self.parked.iter_mut()) {
                    if chat.session.as_ref().is_some_and(|id| ids.contains(id)) {
                        chat.unread = unread;
                    }
                }
                let _ = self.set(|p| {
                    for id in &ids {
                        if unread {
                            p.more.unread.insert(id.clone());
                        } else {
                            p.more.unread.remove(id);
                        }
                    }
                });
            }
            Bulk::Group => {
                // The group they share, if they share one, to keep or change.
                let first = self.prefs.groups.get(&ids[0]).cloned();
                let name = first
                    .filter(|g| ids.iter().all(|id| self.prefs.groups.get(id) == Some(g)))
                    .unwrap_or_default();
                self.overlay = Some(Overlay::Group { ids, name });
                return Task::batch([
                    operation::focus(crate::app::GROUP),
                    operation::select_all(crate::app::GROUP),
                ]);
            }
            Bulk::Delete => {
                if self.confirm_delete.as_deref() != Some(BULK) {
                    self.confirm_delete = Some(BULK.to_owned());
                    self.toast = Some(fill(w.delete_n_sure, "n", ids.len()));
                    return Task::none();
                }
                self.confirm_delete = None;
                self.selected.clear();
                return self.delete_many(&ids);
            }
        }
        Task::none()
    }

    /// Deletes one conversation's log, into the trash.
    pub(crate) fn delete_one(&mut self, id: &str) -> Task<Msg> {
        self.delete_many(&[id.to_owned()])
    }
}

/// Whether any field has the keyboard.
struct AnyFocused(bool);

impl iced::advanced::widget::Operation<bool> for AnyFocused {
    fn focusable(
        &mut self,
        _id: Option<&iced::advanced::widget::Id>,
        _bounds: iced::Rectangle,
        state: &mut dyn iced::advanced::widget::operation::Focusable,
    ) {
        if state.is_focused() {
            self.0 = true;
        }
    }

    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation<bool>)) {
        operate(self);
    }

    fn finish(&self) -> iced::advanced::widget::operation::Outcome<bool> {
        iced::advanced::widget::operation::Outcome::Some(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_put_in_the_trash_keeps_a_name_of_its_own_and_old_ones_are_emptied() {
        let home = std::env::temp_dir().join(format!("sasacode-gui-trash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let logs = home.join("sessions");
        std::fs::create_dir_all(&logs).unwrap();
        let log = logs.join("a.jsonl");
        std::fs::write(&log, "one").unwrap();
        let first = to_trash(&home, &log).unwrap();
        assert!(!log.exists() && first.ends_with("trash/a.jsonl"));
        std::fs::write(&log, "two").unwrap();
        let second = to_trash(&home, &log).unwrap();
        assert!(second.ends_with("trash/1-a.jsonl"));
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "two");

        // A week old is emptied; what was deleted now stays.
        let old = SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&first)
            .unwrap()
            .set_modified(old)
            .unwrap();
        purge_trash(&home);
        assert!(!first.exists() && second.exists());
        std::fs::remove_dir_all(&home).unwrap();
    }
}
