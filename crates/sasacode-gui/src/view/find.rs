//! The command list, over the dimmed window. What it lists is decided in `palette.rs`.

use iced::widget::{Space, button, column, container, mouse_area, opaque, row, scrollable, text, text_input};
use iced::{Alignment, Length, padding};

use super::{El, keycap, one_line};
use crate::app::{App, FIND, Msg};
use crate::icon::{self, icon};
use crate::palette::{Mode, Palette};
use crate::style::{size, thin};

pub(crate) const FIND_LIST: &str = "find-list";
const WIDTH: f32 = 580.0;
/// How tall the list may get, and what the panel takes of the window's height around it.
const LIST: f32 = 420.0;
const AROUND: f32 = 210.0;

impl App {
    pub(super) fn find<'a>(&'a self, palette: &'a Palette) -> El<'a> {
        let c = self.c;
        let (placeholder, tag) = match palette.mode {
            Mode::Root => (self.s.find_root, None),
            Mode::Model => (self.s.find_model, Some(self.s.model)),
            Mode::Permission => (self.s.find_permission, Some(self.s.permission_label)),
            Mode::Effort => (self.s.find_effort, Some(self.s.effort_label)),
        };
        let mut field = row![icon(icon::SEARCH, 16.0, c.ink3)]
            .spacing(10)
            .align_y(Alignment::Center);
        if let Some(tag) = tag {
            field = field.push(keycap(tag.to_owned(), c));
        }
        field = field.push(
            text_input(placeholder, &palette.query)
                .id(FIND)
                .on_input(Msg::FindQuery)
                .on_submit(Msg::FindSubmit)
                .size(size::READ)
                .padding(0)
                .style(c.bare()),
        );

        let entries = self.entries(palette);
        let cursor = palette.cursor.min(entries.len().saturating_sub(1));
        let mut list = column![].spacing(1);
        let mut group = "";
        if entries.is_empty() {
            list = list.push(container(text(self.s.no_match).size(size::BASE).color(c.ink3)).padding([10, 10]));
        }
        for (at, entry) in entries.into_iter().enumerate() {
            if entry.group != group {
                group = entry.group;
                list = list.push(
                    container(text(group).size(size::SMALL).color(c.ink3))
                        .padding(padding::all(10).top(if at == 0 { 4 } else { 12 }).bottom(4)),
                );
            }
            let mut words = column![one_line(text(entry.label).size(size::BASE))].spacing(3);
            if !entry.note.is_empty() {
                words = words.push(text(entry.note).size(size::SMALL).line_height(1.5).color(c.ink3));
            }
            let mut line = row![
                container(icon(entry.icon, 15.0, c.ink3)).padding(padding::top(1)),
                words
            ]
            .spacing(10);
            if !entry.value.is_empty() {
                line = line.push(
                    text(entry.value)
                        .size(size::SMALL)
                        .color(c.ink3)
                        .wrapping(text::Wrapping::None),
                );
            }
            if entry.chosen {
                line = line.push(container(icon(icon::CHECK, 15.0, c.leaf)).padding(padding::top(1)));
            }
            list = list.push(
                button(line)
                    .padding([8, 10])
                    .width(Length::Fill)
                    .style(c.quiet(c.sheet, at == cursor))
                    .on_press(Msg::FindRun(at)),
            );
        }

        let hair = move || {
            container(Space::new())
                .height(1)
                .width(Length::Fill)
                .style(move |_| c.fill(c.hair))
        };
        let panel = container(column![
            container(field).padding([14, 16]),
            hair(),
            // The list gives way before the line of keys under it does.
            container(
                scrollable(container(list).padding(6))
                    .id(FIND_LIST)
                    .direction(thin())
                    .style(c.scroll())
            )
            .max_height((self.height - AROUND).clamp(96.0, LIST)),
            hair(),
            container(text(self.s.find_keys).size(size::TINY).color(c.ink3)).padding([8, 16]),
        ])
        .width(WIDTH)
        .style(move |_| c.floating());

        let lifted = container(
            container(container(panel).padding(1).style(move |_| c.halo(0)))
                .padding(2)
                .style(move |_| c.halo(1)),
        )
        .padding(4)
        .style(move |_| c.halo(2));
        // A press outside the list puts it away; a press on it stays with it.
        opaque(
            mouse_area(
                container(opaque(lifted))
                    .padding(padding::top(77).left(16).right(16).bottom(40))
                    .center_x(Length::Fill)
                    .height(Length::Fill)
                    .style(move |_| c.scrim()),
            )
            .on_press(Msg::Find(None)),
        )
    }
}
