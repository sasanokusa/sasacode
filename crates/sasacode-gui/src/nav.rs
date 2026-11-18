//! Moving between conversations as a browser moves between pages: back to the one left, and
//! forward again; and the controls of the next run changed from the keyboard.

use iced::Task;
use iced::widget::operation;

use crate::app::{App, EFFORTS, Msg, Overlay, PERMISSIONS};

/// How many conversations back are remembered.
const BACK: usize = 50;

impl App {
    /// The conversation shown is being left for another: it is where "back" goes.
    pub(crate) fn leaving(&mut self) {
        if self.navigating {
            return;
        }
        if let Some(id) = self.chat.session.clone() {
            self.back.retain(|b| *b != id);
            self.back.push(id);
            if self.back.len() > BACK {
                self.back.remove(0);
            }
            self.forward.clear();
        }
    }

    /// Opens the conversation `id` without remembering the one left as a step of its own: back
    /// and forward move along what is remembered.
    fn go(&mut self, id: String, from: Option<String>, to_forward: bool) -> Task<Msg> {
        let Some(path) = self.sessions.iter().find(|s| s.id == id).map(|s| s.path.clone()) else {
            return Task::none();
        };
        self.navigating = true;
        let opened = self.open_session(&path);
        self.navigating = false;
        if opened && let Some(from) = from {
            if to_forward {
                self.forward.push(from);
            } else {
                self.back.push(from);
            }
        }
        if opened {
            return Task::batch([operation::focus(crate::app::INPUT), self.where_left()]);
        }
        operation::focus(crate::app::INPUT)
    }

    /// The navigation's and the keyboard controls' messages; any other comes back.
    pub(crate) fn update_nav(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let task = match msg {
            Msg::Back => {
                // Ones since deleted or archived away are passed over.
                while let Some(id) = self.back.pop() {
                    if Some(&id) != self.chat.session.as_ref() && self.sessions.iter().any(|s| s.id == id) {
                        let from = self.chat.session.clone();
                        return Ok(self.go(id, from, true));
                    }
                }
                Task::none()
            }
            Msg::Forward => {
                while let Some(id) = self.forward.pop() {
                    if Some(&id) != self.chat.session.as_ref() && self.sessions.iter().any(|s| s.id == id) {
                        let from = self.chat.session.clone();
                        return Ok(self.go(id, from, false));
                    }
                }
                Task::none()
            }
            Msg::CycleEffort => {
                let at = self.effort.and_then(|e| EFFORTS.iter().position(|x| *x == e));
                self.effort = match at {
                    None => Some(EFFORTS[0]),
                    Some(i) if i + 1 < EFFORTS.len() => Some(EFFORTS[i + 1]),
                    Some(_) => None,
                };
                self.toast = Some(format!(
                    "{}: {}",
                    self.s.m_effort,
                    self.effort.unwrap_or(self.s.default_tag)
                ));
                Task::none()
            }
            Msg::CyclePermission => {
                let at = self.permission.and_then(|p| PERMISSIONS.iter().position(|x| *x == p));
                let next = match at {
                    None => Some(PERMISSIONS[0]),
                    Some(i) if i + 1 < PERMISSIONS.len() => Some(PERMISSIONS[i + 1]),
                    Some(_) => None,
                };
                // auto is chosen with a click, where what it does is said first.
                let next = match next {
                    Some("auto") => PERMISSIONS.iter().copied().find(|p| *p == crate::plan::MODE),
                    other => other,
                };
                if next == Some(crate::plan::MODE) {
                    self.before_plan = self.permission;
                }
                self.permission = next;
                self.toast = Some(format!(
                    "{}: {}",
                    self.s.m_mode,
                    self.permission.unwrap_or(self.s.default_tag)
                ));
                Task::none()
            }
            Msg::FocusModel => operation::focus(crate::app::MODEL),
            Msg::SideOpen => {
                if self.aside.is_none() {
                    let mut side = crate::app::Chat::new(crate::app::SIDE);
                    side.session = self.chat.session.clone();
                    self.aside = Some(side);
                }
                self.overlay = Some(Overlay::Side);
                operation::focus(crate::view::overlay::SIDE_INPUT)
            }
            other => return self.update_about(other),
        };
        Ok(task)
    }
}
