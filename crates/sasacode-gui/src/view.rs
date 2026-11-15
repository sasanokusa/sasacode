//! How the window looks.
//!
//! The window is a sheet set into its ground. On the ground, at the left, is the list of this
//! folder's conversations. On the sheet is one conversation, drawn as a stalk: a mark for each
//! request, and under the mark what was done and what was answered. Only the request in hand is
//! open; earlier ones are two lines each. What needs the person (an approval) and where they write
//! stay at the foot of the sheet, and everything the window can do is one key away in the command
//! list (`view/find.rs`).

mod changes;
mod conversation;
mod dock;
mod find;

pub(crate) use find::FIND_LIST;

use std::time::Duration;

use iced::widget::{Space, button, column, container, mouse_area, row, scrollable, stack, text, tooltip};
use iced::{Alignment, Element, Length, Padding, padding};

use crate::app::{App, Msg};
use crate::icon::{self, icon};
use crate::palette::Mode;
use crate::style::{BOLD, Colors, MEDIUM, size, thin};

pub(crate) type El<'a> = Element<'a, Msg>;

/// The width the conversation and the input share: the stalk, then about 43 full-width characters.
pub(crate) const COLUMN: f32 = 692.0;
const SIDE: f32 = 248.0;
const HEADER: f32 = 44.0;
/// The room the window's own buttons take at its top-left on macOS.
const LIGHTS: f32 = 76.0;
/// A window narrower than this has no room for the list of conversations beside the sheet. They are
/// still in the command list.
const LIST_ROOM: f32 = 780.0;
/// The least the conversation keeps when the changed files are beside it. With less room than that,
/// the changed files take the sheet.
const BESIDE: f32 = 420.0;
const GAP: f32 = 8.0;

impl App {
    pub fn view(&self) -> El<'_> {
        let c = self.c;
        let mut body = row![];
        if self.listed() {
            body = body.push(self.side());
        }
        let around = Padding::new(GAP).left(if self.listed() { 0.0 } else { GAP });
        let sheet = match (self.changes, self.beside()) {
            (false, _) => row![self.main()],
            (true, true) => row![
                self.main(),
                container(Space::new())
                    .width(1)
                    .height(Length::Fill)
                    .style(move |_| c.fill(c.hair)),
                self.changed(changes::WIDTH.into()),
            ],
            (true, false) => row![self.changed(Length::Fill)],
        };
        body = body.push(
            container(
                container(sheet)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(move |_| c.sheet()),
            )
            .padding(around),
        );
        let base = container(body)
            .style(move |_| c.fill(c.ground))
            .width(Length::Fill)
            .height(Length::Fill);
        match &self.palette {
            Some(palette) => stack![base, self.find(palette)].into(),
            None => base.into(),
        }
    }

    // ---- what there is room for ----

    /// The window is wide enough to have the list of conversations beside the sheet.
    pub(crate) fn list_fits(&self) -> bool {
        self.width >= LIST_ROOM
    }

    /// The list of conversations is drawn.
    pub(crate) fn listed(&self) -> bool {
        self.sidebar && self.list_fits()
    }

    fn sheet_width(&self) -> f32 {
        self.width - if self.listed() { SIDE + GAP } else { 2.0 * GAP }
    }

    /// The changed files fit beside the conversation.
    fn beside(&self) -> bool {
        self.sheet_width() - changes::WIDTH - 1.0 >= BESIDE
    }

    /// The width the conversation and the input have now.
    pub(crate) fn column_room(&self) -> f32 {
        let beside = if self.changes && self.beside() {
            changes::WIDTH + 1.0
        } else {
            0.0
        };
        (self.sheet_width() - beside - 40.0).min(COLUMN)
    }

    // ---- the list of conversations, on the ground ----

    fn side(&self) -> El<'_> {
        let c = self.c;
        let mut name = column![one_line(text(self.folder_name()).size(size::BASE).font(BOLD))].spacing(3);
        if let Some(branch) = &self.branch {
            name = name.push(
                row![
                    icon(icon::GIT_BRANCH, 12.0, c.ink3),
                    one_line(text(branch.as_str()).size(size::SMALL).color(c.ink3)),
                ]
                .spacing(4)
                .align_y(Alignment::Center),
            );
        }

        let action = |glyph, label: &'static str, key: String, msg: Option<Msg>| {
            button(
                row![
                    icon(glyph, 15.0, c.ink2),
                    text(label).size(size::BASE).width(Length::Fill),
                    text(key).size(size::TINY).color(c.ink3),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding([6, 8])
            .width(Length::Fill)
            .style(c.quiet(c.ground, false))
            .on_press_maybe(msg)
        };
        let actions = column![
            action(
                icon::SQUARE_PEN,
                self.s.new_session,
                self.s.key("N"),
                (!self.running()).then_some(Msg::NewSession),
            ),
            action(
                icon::SEARCH,
                self.s.find,
                self.s.key("K"),
                Some(Msg::Find(Some(Mode::Root)))
            ),
        ]
        .spacing(1);

        // Grouped by how long ago: the group says the time once, so a row is only its title.
        let mut list = column![].spacing(1);
        let mut group = "";
        for s in &self.sessions {
            let age = self
                .clock
                .duration_since(s.modified)
                .unwrap_or(Duration::ZERO)
                .as_secs();
            let label = match age {
                0..86_400 => self.s.within_day,
                86_400..604_800 => self.s.within_week,
                _ => self.s.older,
            };
            if label != group {
                group = label;
                list = list.push(
                    container(text(label).size(size::SMALL).color(c.ink3)).padding(padding::all(8).top(14).bottom(4)),
                );
            }
            let current = self.session.as_deref() == Some(s.id.as_str());
            let title = if s.first_prompt.is_empty() {
                self.s.untitled
            } else {
                s.first_prompt.lines().next().unwrap_or("")
            };
            let mut line = row![one_line(text(fit(title, 30)).size(size::BASE).color(if current {
                c.ink
            } else {
                c.ink2
            }))]
            .spacing(8)
            .align_y(Alignment::Center);
            if current && self.running() {
                line = line.push(self.dot(self.live_color(), self.breath()));
            }
            list = list.push(
                button(line)
                    .width(Length::Fill)
                    .padding([6, 8])
                    .style(c.quiet(c.ground, current))
                    .on_press_maybe((!self.running()).then(|| Msg::OpenSession(s.path.clone()))),
            );
        }
        if self.sessions.is_empty() {
            list = list.push(
                container(text(self.s.no_sessions).size(size::SMALL).color(c.ink3)).padding(padding::all(8).top(14)),
            );
        }

        let top = if self.unified { 40.0 } else { 14.0 };
        let head = container(name).padding(padding::left(8).right(8));
        let side = column![
            head,
            actions,
            scrollable(list)
                .direction(thin())
                .height(Length::Fill)
                .style(c.scroll())
        ]
        .spacing(12);
        self.grip(
            container(side)
                .padding(padding::all(8).top(top).bottom(0))
                .width(SIDE)
                .height(Length::Fill),
        )
    }

    // ---- the sheet ----

    fn main(&self) -> El<'_> {
        column![self.header(), self.conversation(), self.dock()]
            .width(Length::Fill)
            .into()
    }

    /// What the sheet is showing, and the two things that change what is around it.
    fn header(&self) -> El<'_> {
        let c = self.c;
        let side_label = if self.sidebar {
            self.s.hide_sidebar
        } else {
            self.s.show_sidebar
        };
        let toggle = hint(
            button(icon(icon::PANEL_LEFT, 16.0, c.ink2))
                .padding(6)
                .style(c.quiet(c.sheet, false))
                .on_press(Msg::Sidebar),
            format!("{side_label}  {}", self.s.key("B")),
            tooltip::Position::Bottom,
            c,
        );
        // The conversation is named by its first request. While that request is on the sheet, the
        // bar does not say it a second time.
        let turns = self.transcript.turns();
        let title = turns
            .first()
            .filter(|_| self.scrolled || (turns.len() > conversation::RECENT + 1 && !self.earlier))
            .and_then(|turn| self.transcript.request(*turn))
            .and_then(|request| request.lines().next())
            .unwrap_or("");
        let mut bar = row![].spacing(8).align_y(Alignment::Center);
        // Where the list has no room, there is nothing for its button to show.
        if self.list_fits() {
            bar = bar.push(toggle);
        }
        bar = bar.push(one_line(
            text(fit(title, 72)).size(size::BASE).font(MEDIUM).color(c.ink2),
        ));

        let files = self.transcript.changes().len();
        if files > 0 || self.changes {
            let label = if self.changes {
                self.s.hide_changes
            } else {
                self.s.show_changes
            };
            let ink = if self.changes { c.ink } else { c.ink2 };
            bar = bar.push(hint(
                button(
                    row![
                        icon(icon::FILE_DIFF, 15.0, ink),
                        text(files.to_string()).size(size::SMALL).color(ink)
                    ]
                    .spacing(5)
                    .align_y(Alignment::Center),
                )
                .padding([6, 8])
                .style(c.quiet(c.sheet, self.changes))
                .on_press(Msg::Changes),
                format!("{label}  {}", self.s.key("D")),
                tooltip::Position::Bottom,
                c,
            ));
        }
        // With the list put away on macOS, the window's own buttons are where the bar starts.
        let left = match (self.unified && !self.listed(), self.list_fits()) {
            (true, _) => LIGHTS,
            (false, true) => 8.0,
            (false, false) => 16.0,
        };
        let line = if self.scrolled { c.hair } else { c.sheet };
        column![
            self.grip(
                container(bar)
                    .padding(padding::left(left).right(8).top(self.bar_top()))
                    .height(self.bar_height())
                    .align_y(Alignment::Center)
            ),
            container(Space::new())
                .height(1)
                .width(Length::Fill)
                .style(move |_| c.fill(line)),
        ]
        .into()
    }

    /// The bar at the top of the sheet. On macOS its controls sit lower: the strip the system keeps
    /// for moving the window covers the window's first 28 points, and a press there is not theirs.
    pub(crate) fn bar_height(&self) -> f32 {
        if self.unified { HEADER + 8.0 } else { HEADER }
    }

    pub(crate) fn bar_top(&self) -> f32 {
        if self.unified { 10.0 } else { 0.0 }
    }

    /// On macOS the window has no title bar, so its top is what the window is moved by.
    fn grip<'a>(&self, content: impl Into<El<'a>>) -> El<'a> {
        if self.unified {
            mouse_area(content).on_press(Msg::Drag).into()
        } else {
            content.into()
        }
    }

    // ---- small parts ----

    /// The color of what is going on now: the leaf while the agent works, amber while it waits for
    /// the person.
    pub(crate) fn live_color(&self) -> iced::Color {
        if self.transcript.pending_approval().is_some() {
            self.c.amber
        } else {
            self.c.leaf
        }
    }

    /// A small disc; `strength` between 0 and 1 fades it toward the sheet.
    pub(crate) fn dot<'a>(&self, color: iced::Color, strength: f32) -> El<'a> {
        let c = self.c;
        let shown = crate::style::mix(c.sheet, color, 0.3 + 0.7 * strength);
        container(Space::new())
            .width(7)
            .height(7)
            .style(move |_| c.node(shown, false).1)
            .into()
    }
}

/// A key's name on a keycap, for where the window teaches its keys.
pub(crate) fn keycap<'a>(label: String, c: Colors) -> El<'a> {
    container(text(label).size(size::TINY))
        .padding([1, 5])
        .style(move |_| c.keycap())
        .into()
}

/// What a control that is only an icon does, shown when the pointer rests on it.
pub(crate) fn hint<'a>(content: impl Into<El<'a>>, label: String, position: tooltip::Position, c: Colors) -> El<'a> {
    tooltip(
        content,
        container(text(label).size(size::SMALL))
            .padding([4, 8])
            .style(move |_| c.tooltip()),
        position,
    )
    .gap(6)
    .delay(Duration::from_millis(450))
    .into()
}

/// Text that takes the room left in its row and is cut at the edge instead of wrapping.
pub(crate) fn one_line<'a>(t: iced::widget::Text<'a>) -> El<'a> {
    container(t.wrapping(text::Wrapping::None))
        .clip(true)
        .width(Length::Fill)
        .into()
}

/// `s` cut to about `units` half-width columns (a full-width character counts two), with an
/// ellipsis when something was cut.
pub(crate) fn fit(s: &str, units: usize) -> String {
    let mut used = 0;
    for (at, ch) in s.char_indices() {
        used += if ch.is_ascii() { 1 } else { 2 };
        if used > units {
            return format!("{}…", &s[..at]);
        }
    }
    s.to_owned()
}

/// How many half-width columns `s` takes.
pub(crate) fn units(s: &str) -> usize {
    s.chars().map(|ch| if ch.is_ascii() { 1 } else { 2 }).sum()
}

/// `s` with each tab as the spaces up to the next stop of four: the fonts have no glyph for a tab.
pub(crate) fn spaced(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut column = 0;
    for ch in s.chars() {
        match ch {
            '\t' => {
                let pad = 4 - column % 4;
                out.extend(std::iter::repeat_n(' ', pad));
                column += pad;
            }
            '\n' => {
                out.push(ch);
                column = 0;
            }
            _ => {
                out.push(ch);
                column += if ch.is_ascii() { 1 } else { 2 };
            }
        }
    }
    out
}

/// 1234 as 1.2k: a count is read at a glance, not to the token.
pub(crate) fn count(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..1_000_000 => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_title_is_cut_by_its_width_tabs_become_spaces_and_counts_are_shortened() {
        assert_eq!(fit("CIが落ちている", 30), "CIが落ちている");
        assert_eq!(
            fit("依存パッケージを最新にして、壊れたところを直して", 12),
            "依存パッケー…"
        );
        assert_eq!(spaced("1\ta\n\tb"), "1   a\n    b");
        assert_eq!(count(999), "999");
        assert_eq!(count(1234), "1.2k");
        assert_eq!(count(2_500_000), "2.5M");
    }
}
