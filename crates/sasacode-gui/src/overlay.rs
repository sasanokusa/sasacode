//! What is drawn over the window (the command palette, the list of keys, the settings, a name being
//! given to a conversation, a side question) and the messages that are not about the conversation
//! itself: settings, the list's names, pins and archive, the system, and the side question.

use iced::Task;
use iced::widget::operation;

use crate::app::{App, Chat, Clip, Msg, PALETTE, RENAME, Tab};
use crate::input::Command;
use crate::keys::Act;
use crate::layout::{ZOOM_MAX, ZOOM_MIN};
use crate::system;

/// The key of the side question's run, which is no conversation's.
pub const SIDE: u64 = u64::MAX;

#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    /// What is typed, and which of what it finds is chosen, among what it picks from.
    Palette {
        query: String,
        at: usize,
        pick: Pick,
    },
    Keys,
    Settings,
    /// A name for the conversation with this id.
    Rename {
        id: String,
        name: String,
    },
    /// The side question and its answer, `App::aside`.
    Side,
    /// The group of the conversations with these ids, as it is typed.
    Group {
        ids: Vec<String>,
        name: String,
    },
    /// The pull request being written, `App::pr_draft`.
    Pr,
    /// The window asked to close while runs go on.
    Quit(iced::window::Id),
    /// `auto` asked for the first time.
    ConfirmAuto,
}

/// What a palette picks from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// The window's commands, and the rest the command palette lists.
    Commands,
    /// The branches to check out, or a new one to make.
    Branches,
    /// The folder's scripts, to run one.
    Scripts,
    /// The conversation's messages and headings, to scroll to one.
    Outline,
    /// The list's conversations, to put one beside the one shown.
    Split,
}

impl Overlay {
    pub fn palette() -> Overlay {
        Overlay::picker(Pick::Commands)
    }

    pub fn picker(pick: Pick) -> Overlay {
        Overlay::Palette {
            query: String::new(),
            at: 0,
            pick,
        }
    }
}

/// An entry of the palette: what it is called, its keys if it has any, and what it does.
pub struct Entry {
    pub label: String,
    pub keys: Option<String>,
    pub msg: Msg,
}

impl App {
    /// All the palette offers, in the order it lists them.
    pub(crate) fn entries(&self) -> Vec<Entry> {
        let s = self.s;
        let entry = |label: &str, keys: Option<String>, msg: Msg| Entry {
            label: label.to_owned(),
            keys,
            msg,
        };
        let table = crate::keys::bound();
        let key = |act: Act| table.chord(act).map(crate::keys::Chord::show);
        let mut all = vec![
            entry(s.new_session, key(Act::New), Msg::NewSession),
            entry(s.find, key(Act::Find), Msg::Find),
            entry(s.c_find, key(Act::FindIn), Msg::Command(Command::Find)),
            entry(s.c_mention, key(Act::Mention), Msg::Command(Command::Mention)),
            entry(s.c_prev, key(Act::Prev), Msg::Neighbour(-1)),
            entry(s.c_next, key(Act::Next), Msg::Neighbour(1)),
            entry(s.c_attention, key(Act::Attention), Msg::Command(Command::Attention)),
            entry(s.c_btw, Some("/btw".into()), Msg::Command(Command::Btw)),
            entry(s.w().c_scripts, key(Act::Scripts), Msg::Command(Command::Scripts)),
            entry(s.w().c_outline, key(Act::Outline), Msg::Command(Command::Outline)),
            entry(
                if self.split.is_some() {
                    s.w().split_close
                } else {
                    s.w().c_split
                },
                key(Act::Split),
                Msg::Command(Command::Split),
            ),
            entry(s.w().c_draft_commit, None, Msg::Draft(crate::helper::Draft::Commit)),
            entry(s.w().copy_folder, None, Msg::Copy(Clip::Folder)),
            entry(s.w().c_tui, Some("/tui".into()), Msg::Command(Command::Tui)),
            entry(s.w().pr_title, None, Msg::PrOpen),
            entry(s.c_export, None, Msg::Copy(Clip::Conversation)),
            entry(s.c_steps, None, Msg::Command(Command::Steps)),
            entry(s.c_model, None, Msg::Command(Command::Model)),
            entry(s.w().c_undo, Some(format!("{}Z", crate::view::MOD)), Msg::Undo),
            entry(s.w().read_all, None, Msg::ReadAll),
        ];
        if let Some(id) = self.chat.session.clone() {
            let unread = self.prefs.more.unread.contains(&id) || self.chat.unread;
            all.push(entry(
                if unread { s.w().mark_read } else { s.w().mark_unread },
                None,
                Msg::MarkUnread(id),
            ));
            all.push(entry(s.w().copy_log, None, Msg::Copy(Clip::Log)));
        }
        if self.chat.session.is_some() {
            all.push(entry(s.c_rename, None, Msg::Command(Command::Rename)));
            all.push(entry(s.c_pin, None, Msg::Command(Command::Pin)));
            all.push(entry(s.c_archive, None, Msg::Command(Command::Archive)));
            all.push(entry(s.w().c_group, None, Msg::Command(Command::Group)));
            all.push(entry(s.w().c_delete, None, Msg::Command(Command::Delete)));
        }
        all.extend([
            entry(s.c_archived, None, Msg::Command(Command::Archived)),
            entry(s.a_side, key(Act::Side), Msg::ToggleSide),
            entry(s.a_insp, key(Act::Inspector), Msg::ToggleInspector),
            entry(s.tab_changes, key(Act::Tab(1)), Msg::Tab(Tab::Changes)),
            entry(s.tab_plan, key(Act::Tab(2)), Msg::Tab(Tab::Plan)),
            entry(s.tab_cmd, key(Act::Tab(3)), Msg::Tab(Tab::Commands)),
            entry(s.tab_usage, key(Act::Tab(4)), Msg::Tab(Tab::Usage)),
            entry(s.tab_git, key(Act::Tab(5)), Msg::Tab(Tab::Git)),
            entry(s.w().tab_files, key(Act::Tab(6)), Msg::Tab(Tab::Files)),
            entry(s.c_window, key(Act::Window), Msg::Command(Command::Window)),
            entry(s.c_folder, None, Msg::Command(Command::Folder)),
            entry(s.c_terminal, None, Msg::Command(Command::Terminal)),
            entry(s.c_editor, None, Msg::Command(Command::Editor)),
            entry(s.c_worktree, None, Msg::Command(Command::Worktree)),
            entry(s.settings, key(Act::Settings), Msg::Show(Some(Overlay::Settings))),
            entry(s.c_keys, key(Act::Keys), Msg::Show(Some(Overlay::Keys))),
            entry(s.c_zoom_in, key(Act::ZoomIn), Msg::Zoom(1)),
            entry(s.c_zoom_out, key(Act::ZoomOut), Msg::Zoom(-1)),
            entry(s.c_zoom_reset, key(Act::ZoomReset), Msg::Zoom(0)),
        ]);
        for (name, dark) in [
            (s.theme_system, None),
            (s.theme_light, Some(false)),
            (s.theme_dark, Some(true)),
        ] {
            all.push(Entry {
                label: crate::i18n::fill(s.c_theme, "name", name),
                keys: None,
                msg: Msg::SetDark(dark),
            });
        }
        // The models this folder's conversations used, newest first.
        let mut models: Vec<&str> = Vec::new();
        for m in self.sessions.iter().filter_map(|x| x.model.as_deref()) {
            if !models.contains(&m) {
                models.push(m);
            }
        }
        for m in models.into_iter().take(8) {
            all.push(Entry {
                label: crate::i18n::fill(s.recent_models, "model", m),
                keys: None,
                msg: Msg::Model(m.to_owned()),
            });
        }
        // The other folders sasacode was used in: each opens in a window of its own.
        if let Some(home) = &self.home {
            for project in system::projects(home).into_iter().filter(|p| *p != self.cwd).take(12) {
                all.push(Entry {
                    label: crate::i18n::fill(s.c_project, "path", project.display()),
                    keys: None,
                    msg: Msg::OpenProject(project),
                });
            }
        }
        all
    }

    /// The branches whose names have every word typed, the one checked out marked; then, when what
    /// is typed is a name no branch has, a new branch of that name: last, so that Enter on a part
    /// of a name checks that branch out rather than making another.
    fn branch_entries(&self, query: &str) -> Vec<Entry> {
        let w = self.s.w();
        let here = self.git.branch.as_deref();
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut all: Vec<Entry> = self
            .branch_list
            .iter()
            .flatten()
            .filter(|b| words.iter().all(|w| b.to_lowercase().contains(w)))
            .map(|b| Entry {
                label: if Some(b.as_str()) == here {
                    format!("{b} {}", w.branch_here)
                } else {
                    b.clone()
                },
                keys: None,
                msg: Msg::GitDo(crate::git::Op::Switch(b.clone())),
            })
            .collect();
        let name = query.trim();
        let valid = !name.is_empty() && !name.contains(char::is_whitespace);
        if valid && !self.branch_list.iter().flatten().any(|b| b == name) {
            all.push(Entry {
                label: crate::i18n::fill(w.branch_new, "name", name),
                keys: None,
                msg: Msg::GitDo(crate::git::Op::NewBranch(name.to_owned())),
            });
        }
        all
    }

    /// What the palette finds for what is typed: entries that have every word of it.
    pub(crate) fn found_entries(&self) -> Vec<Entry> {
        let Some(Overlay::Palette { query, pick, .. }) = &self.overlay else {
            return Vec::new();
        };
        if *pick == Pick::Branches {
            return self.branch_entries(query);
        }
        if *pick == Pick::Split {
            return self.split_entries(query);
        }
        if *pick == Pick::Outline {
            return self.outline_entries(query);
        }
        if *pick == Pick::Scripts {
            let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
            let w = self.s.w();
            let mut all: Vec<Entry> = self
                .scripts()
                .iter()
                .enumerate()
                .filter(|(_, sc)| {
                    let label = format!("{} {}", sc.name, sc.command).to_lowercase();
                    words.iter().all(|w| label.contains(w))
                })
                .map(|(i, sc)| Entry {
                    label: if sc.name.trim().is_empty() {
                        sc.command.clone()
                    } else {
                        sc.name.clone()
                    },
                    keys: (!sc.name.trim().is_empty()).then(|| crate::view::clip(&sc.command, 28)),
                    msg: Msg::RunScript(i),
                })
                .collect();
            all.push(Entry {
                label: w.script_edit.to_owned(),
                keys: None,
                msg: Msg::Show(Some(Overlay::Settings)),
            });
            return all;
        }
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        self.entries()
            .into_iter()
            .filter(|e| {
                let label = e.label.to_lowercase();
                words.iter().all(|w| label.contains(w))
            })
            .collect()
    }

    /// The messages that are not about the conversation itself; any other comes back.
    pub(crate) fn update_more(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let task = match msg {
            Msg::Show(overlay) => {
                let focus = match &overlay {
                    Some(Overlay::Palette {
                        pick: Pick::Branches, ..
                    }) => Task::batch([operation::focus(PALETTE), self.list_branches()]),
                    Some(Overlay::Palette { .. }) => operation::focus(PALETTE),
                    Some(Overlay::Rename { .. }) => {
                        Task::batch([operation::focus(RENAME), operation::select_all(RENAME)])
                    }
                    None => operation::focus(crate::app::INPUT),
                    _ => Task::none(),
                };
                // A key waited for is let go with the list of keys.
                self.key_capture = None;
                self.key_note = None;
                if overlay.is_none() || self.overlay == Some(Overlay::Side) {
                    // The side question's run goes with it.
                    self.aside = None;
                }
                self.overlay = overlay;
                focus
            }
            Msg::OverlayText(text) => {
                match &mut self.overlay {
                    Some(Overlay::Palette { query, at, .. }) => {
                        *query = text;
                        *at = 0;
                    }
                    Some(Overlay::Rename { name, .. }) | Some(Overlay::Group { name, .. }) => *name = text,
                    _ => {}
                }
                Task::none()
            }
            Msg::Arrow(step) => {
                let found = self.found_entries().len();
                if let Some(Overlay::Palette { at, .. }) = &mut self.overlay
                    && found > 0
                {
                    *at = (*at as isize + step as isize).rem_euclid(found as isize) as usize;
                }
                Task::none()
            }
            Msg::PaletteRun(i) => {
                let mut found = self.found_entries();
                self.overlay = None;
                if i < found.len() {
                    let entry = found.swap_remove(i);
                    return Ok(Task::batch([
                        operation::focus(crate::app::INPUT),
                        self.update(entry.msg),
                    ]));
                }
                operation::focus(crate::app::INPUT)
            }
            Msg::SetEnter(sends) => self.set(|p| p.enter_sends = sends),
            Msg::SetDark(dark) => {
                let task = self.set(|p| p.dark = dark);
                self.apply_theme();
                task
            }
            Msg::Zoom(step) => {
                let old = self.prefs.zoom;
                let new = match step {
                    0 => 1.0,
                    _ => ((old * 10.0).round() + step as f32) / 10.0,
                }
                .clamp(ZOOM_MIN, ZOOM_MAX);
                // The window keeps its size on the screen: as drawn, it is larger or smaller.
                self.width = self.width * old / new;
                self.height = self.height * old / new;
                self.set(|p| p.zoom = new)
            }
            Msg::SetNotify(on) => self.set(|p| p.notify = on),
            Msg::SetAwake(on) => {
                let task = self.set(|p| p.awake = on);
                self.hold_awake();
                task
            }
            Msg::SetSteps(on) => self.set(|p| p.steps = on),
            Msg::Focus(on) => {
                self.focused = on;
                // Keys let go in another window are not heard here: none is taken as held.
                self.modifiers = iced::keyboard::Modifiers::default();
                // Back in front: what another window said about conversations meanwhile is shown.
                if on {
                    self.read_others();
                }
                Task::none()
            }
            Msg::Hover(on) => {
                self.dropping = on;
                Task::none()
            }
            Msg::Dropped(path) => self.dropped(&path),
            Msg::Paste => iced::clipboard::read().map(Msg::Pasted),
            Msg::Pasted(Some(text)) if !text.is_empty() => self.edit(iced::widget::text_editor::Action::Edit(
                iced::widget::text_editor::Edit::Paste(std::sync::Arc::new(text)),
            )),
            // No text: an image, as a file the message names, kept where sasacode keeps its own.
            Msg::Pasted(_) => {
                let dir = self.home.clone().unwrap_or_else(std::env::temp_dir).join("pasted");
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || system::clipboard_image(&dir))
                            .await
                            .ok()
                            .flatten()
                    },
                    Msg::PastedImage,
                )
            }
            Msg::PastedImage(Some(path)) => self.dropped(&path),
            Msg::PastedImage(None) => Task::none(),
            Msg::Recall(step) => self.recall(step),
            Msg::Files(files) => {
                self.files = files;
                Task::none()
            }
            Msg::Pick(step) => {
                let len = self.offer().map_or(0, |o| o.len());
                if len > 0 {
                    self.pick = (self.pick as isize + step as isize).rem_euclid(len as isize) as usize;
                }
                Task::none()
            }
            Msg::Complete(which) => self.complete(which),
            Msg::Dismiss => {
                self.dismissed = true;
                Task::none()
            }
            Msg::Command(command) => self.command(command),
            Msg::OpenPath(path) => {
                system::open(&path);
                Task::none()
            }
            Msg::Unattach(written) => {
                let text = self.chat.editor.text();
                if let Some(at) = text.find(&written) {
                    let mut rest = format!("{}{}", &text[..at], &text[at + written.len()..]);
                    // The space it was set apart by goes with it.
                    rest = rest.replace("  ", " ");
                    self.chat.editor = iced::widget::text_editor::Content::with_text(rest.trim_start());
                    self.chat.editor.perform(iced::widget::text_editor::Action::Move(
                        iced::widget::text_editor::Motion::DocumentEnd,
                    ));
                }
                operation::focus(crate::app::INPUT)
            }
            Msg::Reveal(path) => {
                system::reveal(&path);
                Task::none()
            }
            Msg::Editor(path) => {
                system::editor(&path);
                Task::none()
            }
            Msg::OpenProject(path) => self.new_window(path, None),
            Msg::NewWindow(cwd, session, at) => {
                // A little below and to the right of this one, so it is seen to open.
                let at = at.map(|at| iced::Point::new(at.x + CASCADE, at.y + CASCADE));
                system::new_window(&self.program, &cwd, session.as_deref(), at);
                Task::none()
            }
            Msg::Pin(id) => {
                self.remember();
                self.set(|p| {
                    if let Some(i) = p.pinned.iter().position(|x| *x == id) {
                        p.pinned.remove(i);
                    } else {
                        p.pinned.push(id);
                    }
                })
            }
            Msg::Archive(id) => {
                self.remember();
                let archiving = !self.prefs.archived.contains(&id);
                let _ = self.set(|p| {
                    if p.archived.remove(&id) {
                        // Taken back by hand: not archived again for its age.
                        p.more.kept.insert(id);
                    } else {
                        p.pinned.retain(|x| *x != id);
                        p.more.kept.remove(&id);
                        p.archived.insert(id);
                    }
                });
                let w = self.s.w();
                self.told_undoable(if archiving { w.archived_one } else { w.unarchived_one }.to_owned());
                Task::none()
            }
            Msg::Rename(id) => {
                let name = self.prefs.names.get(&id).cloned().unwrap_or_default();
                self.overlay = Some(Overlay::Rename { id, name });
                Task::batch([operation::focus(RENAME), operation::select_all(RENAME)])
            }
            Msg::RenameSave => {
                if let Some(Overlay::Rename { id, name }) = self.overlay.take() {
                    let name = name.trim().to_owned();
                    let _ = self.set(|p| {
                        if name.is_empty() {
                            p.names.remove(&id);
                        } else {
                            p.names.insert(id, name);
                        }
                    });
                }
                operation::focus(crate::app::INPUT)
            }
            Msg::RowHover(id) => {
                self.row_hover = id;
                Task::none()
            }
            Msg::FindInText(text) => {
                self.find_in = Some(text);
                self.find_at = 0;
                Task::none()
            }
            Msg::FindInClose => {
                self.find_in = None;
                operation::focus(crate::app::INPUT)
            }
            other => return self.update_git(other),
        };
        Ok(task)
    }

    /// Changes a setting, and keeps it.
    pub(crate) fn set(&mut self, change: impl FnOnce(&mut crate::layout::Prefs)) -> Task<Msg> {
        change(&mut self.prefs);
        crate::keys::bind(crate::keys::Table::from(&self.prefs.keys));
        self.keep_layout();
        Task::none()
    }

    /// The computer is held awake while any conversation runs, if the settings say so.
    pub(crate) fn hold_awake(&mut self) {
        let running = std::iter::once(&self.chat).chain(&self.parked).any(Chat::running);
        if running && self.prefs.awake {
            if self.awake.is_none() {
                self.awake = system::Awake::hold();
            }
        } else {
            self.awake = None;
        }
    }

    /// Tells the system that conversation `key` ended or waits, when the window is behind others.
    pub(crate) fn tell(&self, key: u64, what: &str) {
        if !self.prefs.notify || key == SIDE {
            return;
        }
        let title = std::iter::once(&self.chat)
            .chain(&self.parked)
            .find(|c| c.key == key)
            .map(|c| self.chat_title(c))
            .unwrap_or_else(|| "sasacode".to_owned());
        system::notify(&title, what);
    }

    /// The name the conversation shown was given in the list, if it was.
    pub(crate) fn name(&self) -> Option<&str> {
        self.chat
            .session
            .as_ref()
            .and_then(|id| self.prefs.names.get(id))
            .map(String::as_str)
    }

    /// What a conversation is called: the name it was given, else its first message.
    pub(crate) fn chat_title(&self, chat: &Chat) -> String {
        chat.session
            .as_ref()
            .and_then(|id| self.prefs.names.get(id))
            .cloned()
            .or_else(|| {
                chat.transcript
                    .title()
                    .map(|t| crate::view::clip(crate::view::first_line(t), 60))
            })
            .unwrap_or_else(|| self.s.new_session.to_owned())
    }

    /// The first conversation in the list that waits for an answer, else one with news.
    pub(crate) fn attention(&mut self) -> Task<Msg> {
        let needs = |c: &Chat| c.running() && c.transcript.pending_approval().is_some();
        let waiting = self.parked.iter().find(|c| needs(c));
        let news = self.parked.iter().find(|c| c.unread);
        let Some(id) = waiting.or(news).and_then(|c| c.session.clone()) else {
            return Task::none();
        };
        match self.sessions.iter().find(|s| s.id == id).map(|s| s.path.clone()) {
            Some(path) => self.update(Msg::OpenSession(path)),
            None => Task::none(),
        }
    }

    /// Asks the side question: the conversation so far is read, nothing is written to it, and its
    /// tools are not let do anything that asks first.
    pub(crate) fn side_question(&mut self, question: String) -> Task<Msg> {
        let mut side = Chat::new(SIDE);
        side.session = self.chat.session.clone();
        self.aside = Some(side);
        self.overlay = Some(Overlay::Side);
        self.start(SIDE, question)
    }

    /// A git worktree of this folder's repository on a branch of its own, opened in a new window.
    pub(crate) fn worktree(&mut self) -> Task<Msg> {
        let s = self.s;
        let (note, task) = match self.home.as_deref().map(|home| system::make_worktree(&self.cwd, home)) {
            // Its folder's name: the bar has no room for the path, which the new window shows.
            Some(Ok(path)) => (
                crate::i18n::fill(
                    s.worktree_made,
                    "path",
                    path.file_name()
                        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned()),
                ),
                self.new_window(path, None),
            ),
            Some(Err(error)) => (crate::i18n::fill(s.worktree_failed, "error", error), Task::none()),
            None => return Task::none(),
        };
        self.toast = Some(note);
        task
    }

    /// Another window on `cwd`, open on `session` if given: where this one is is asked first.
    pub(crate) fn new_window(&self, cwd: std::path::PathBuf, session: Option<String>) -> Task<Msg> {
        iced::window::oldest().then(move |id| match id {
            Some(id) => {
                let (cwd, session) = (cwd.clone(), session.clone());
                iced::window::position(id).map(move |at| Msg::NewWindow(cwd.clone(), session.clone(), at))
            }
            None => Task::done(Msg::NewWindow(cwd.clone(), session.clone(), None)),
        })
    }
}

/// How far below and to the right of the window a window it opens is put.
const CASCADE: f32 = 28.0;

impl App {
    /// The repository looked at again, away from the window.
    pub(crate) fn refresh_git(&mut self) -> Task<Msg> {
        let cwd = self.cwd.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || crate::git::status(&cwd))
                    .await
                    .unwrap_or_default()
            },
            |status| Msg::GitStatus(Box::new(status)),
        )
    }

    /// The git tab's messages; any other comes back.
    pub(crate) fn update_git(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        use crate::git::Op;
        let task = match msg {
            Msg::GitRefresh => self.refresh_git(),
            Msg::GitStatus(status) => {
                // Another branch has another pull request, if any.
                let moved = self.git.branch != status.branch;
                self.git = *status;
                if moved && self.tab == Tab::Git {
                    return Ok(self.refresh_pr());
                }
                Task::none()
            }
            Msg::CommitText(text) => {
                self.commit_msg = text;
                Task::none()
            }
            Msg::GitDo(op) => {
                // Discarding asks twice: what it throws away is not kept anywhere. So does a merge,
                // which others see.
                let sure = match &op {
                    Op::Discard(path) => Some(path.clone()),
                    Op::Hunk {
                        path,
                        act: crate::git::HunkAct::Discard,
                        at,
                        ..
                    } => Some(format!("{path}#{at}")),
                    _ => None,
                };
                if let Some(key) = sure
                    && self.confirm_discard.as_ref() != Some(&key)
                {
                    self.confirm_discard = Some(key);
                    return Ok(Task::none());
                }
                if matches!(op, Op::Merge(_)) && !std::mem::take(&mut self.confirm_merge) {
                    self.confirm_merge = true;
                    return Ok(Task::none());
                }
                // A commit or a push to the branch others start from asks once more.
                let on_default = self.git.branch.is_some() && self.git.branch == self.git.default_branch;
                if matches!(op, Op::Commit(_) | Op::Push) && on_default && !std::mem::take(&mut self.confirm_default) {
                    self.confirm_default = true;
                    self.toast = Some(crate::i18n::fill(
                        self.s.w().default_sure,
                        "branch",
                        self.git.branch.clone().unwrap_or_default(),
                    ));
                    return Ok(Task::none());
                }
                self.confirm_default = false;
                self.confirm_discard = None;
                self.confirm_merge = false;
                // A branch picked in the list: the list goes.
                if matches!(op, Op::Switch(_) | Op::NewBranch(_)) {
                    self.overlay = None;
                }
                if self.git_busy || matches!(&op, Op::Commit(m) if m.trim().is_empty()) {
                    return Ok(Task::none());
                }
                if matches!(op, Op::Commit(_)) {
                    self.commit_msg.clear();
                }
                self.git_busy = true;
                let (cwd, branch) = (self.cwd.clone(), self.git.branch.clone());
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || crate::git::apply(&cwd, &op, branch.as_deref()))
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()))
                    },
                    Msg::GitDone,
                )
            }
            Msg::GitDone(result) => {
                self.git_busy = false;
                let said = match result {
                    Ok(out) => out.lines().last().unwrap_or_default().to_owned(),
                    Err(err) => err.lines().last().unwrap_or_default().to_owned(),
                };
                // A pull request's address is what `gh` says last: opened in the browser.
                if said.starts_with("https://") {
                    let _ = self.update(Msg::Link(said.clone()));
                }
                self.toast = (!said.is_empty()).then_some(said);
                // What was pushed, opened or merged shows in the pull request's part of the tab.
                if self.tab == Tab::Git {
                    return Ok(Task::batch([self.refresh_git(), self.refresh_pr()]));
                }
                self.refresh_git()
            }
            Msg::Quote(path, line) => {
                // As `@path:line`, the path as a dropped file's is given.
                let text = format!("{} ", crate::review::place(&path, line, &self.cwd));
                self.chat.editor.perform(iced::widget::text_editor::Action::Edit(
                    iced::widget::text_editor::Edit::Paste(std::sync::Arc::new(text)),
                ));
                self.dismissed = true;
                operation::focus(crate::app::INPUT)
            }
            other => match self.update_review(other) {
                Ok(task) => task,
                Err(other) => return self.update_list(other),
            },
        };
        Ok(task)
    }
}
