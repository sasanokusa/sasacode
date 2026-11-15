//! The files the conversation changed, beside it: each file once, with every change made to it.

use iced::widget::{button, column, container, row, scrollable, text, tooltip};
use iced::{Alignment, Length, padding};

use super::{El, hint, one_line};
use crate::app::{App, Msg};
use crate::icon::{self, icon};
use crate::style::{BOLD, MONO, size, thin};

pub(super) const WIDTH: f32 = 372.0;
const LINES: usize = 60;

impl App {
    pub(super) fn changed(&self, width: Length) -> El<'_> {
        let c = self.c;
        let files = self.transcript.changes();
        let head = row![
            text(self.s.changes).size(size::BASE).font(BOLD),
            text(if files.is_empty() {
                String::new()
            } else {
                files.len().to_string()
            })
            .size(size::SMALL)
            .color(c.ink3)
            .width(Length::Fill),
            hint(
                button(icon(icon::X, 15.0, c.ink2))
                    .padding(6)
                    .style(c.quiet(c.sheet, false))
                    .on_press(Msg::Changes),
                format!("{}  {}", self.s.hide_changes, self.s.key("D")),
                tooltip::Position::Bottom,
                c,
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let mut list = column![].spacing(18);
        if files.is_empty() {
            list = list.push(
                text(self.s.changes_empty)
                    .size(size::BASE)
                    .line_height(1.6)
                    .color(c.ink3),
            );
        }
        for file in files {
            let mut block = column![
                row![
                    icon(icon::FILE_TEXT, 14.0, c.ink3),
                    one_line(text(file.path.to_owned()).font(MONO).size(size::SMALL)),
                    text(format!("+{}", file.added)).size(size::SMALL).color(c.added_ink),
                    text(format!("\u{2212}{}", file.removed))
                        .size(size::SMALL)
                        .color(c.removed_ink),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
            ]
            .spacing(8);
            for hunk in &file.hunks {
                block = block.push(self.diff(hunk, LINES, c.well));
            }
            list = list.push(block);
        }

        column![
            container(head)
                .padding(padding::left(16).right(8).top(self.bar_top()))
                .height(self.bar_height())
                .align_y(Alignment::Center),
            scrollable(container(list).padding(padding::all(16).top(8)))
                .direction(thin())
                .height(Length::Fill)
                .style(c.scroll()),
        ]
        .width(width)
        .into()
    }
}
