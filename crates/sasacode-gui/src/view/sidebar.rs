//! The left panel: this folder's conversations, newest first.

use std::time::SystemTime;

use iced::widget::{Space, button, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Length};

use super::{BOLD, El, SIDE_W, clip, tip};
use crate::app::{App, Msg};
use crate::i18n::fill;
use crate::icons::{Icon, icon};

pub fn sidebar(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let busy = app.busy();
    let folder = app
        .cwd
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();

    let head = row![
        icon(Icon::Leaf, 16.0, t.accent),
        text("sasacode").size(14).font(BOLD).color(t.fg),
        Space::new().width(Length::Fill),
        text(clip(&folder, 18))
            .size(12)
            .color(t.fg_3)
            .wrapping(text::Wrapping::None),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let new = button(
        row![
            icon(Icon::Plus, 14.0, if busy { t.fg_3 } else { t.fg }),
            text(s.new_session).size(13),
            Space::new().width(Length::Fill),
            text(format!("{}N", super::MOD)).size(11).color(t.fg_3),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([7, 10])
    .style(t.outline(t.fg))
    .on_press_maybe((!busy).then_some(Msg::NewSession));

    let search = container(
        row![
            icon(Icon::Search, 14.0, t.fg_3),
            text_input(s.find, &app.filter)
                .on_input(Msg::Filter)
                .size(13)
                .padding(0)
                .style(t.input_bare()),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([6, 10])
    .style(t.boxed(t.raised, t.line, crate::theme::RADIUS));

    let needle = app.filter.trim().to_lowercase();
    let now = SystemTime::now();
    let mut list = column![].spacing(2);
    let mut shown = 0;
    for session in &app.sessions {
        if !needle.is_empty() && !session.first_prompt.to_lowercase().contains(&needle) {
            continue;
        }
        shown += 1;
        let current = app.session.as_deref() == Some(session.id.as_str());
        let title = if session.first_prompt.is_empty() {
            clip(&session.id, 8)
        } else {
            clip(&session.first_prompt, 26)
        };
        let ago = now.duration_since(session.modified).unwrap_or_default();
        let meta = format!("{} · {}", s.ago(ago), fill(s.msgs, "n", session.message_count));
        let mut title_row = row![text(title).size(13).color(t.fg).wrapping(text::Wrapping::None)]
            .spacing(6)
            .align_y(Alignment::Center);
        if current && app.running() {
            title_row = title_row
                .push(Space::new().width(Length::Fill))
                .push(icon(Icon::Dot, 9.0, t.accent));
        }
        let item = button(column![title_row, text(meta).size(11.5).color(t.fg_3)].spacing(2))
            .width(Length::Fill)
            .padding([6, 10])
            .style(t.ghost(t.fg, current))
            .on_press_maybe((!busy && !current).then(|| Msg::OpenSession(session.path.clone())));
        // Why a click does nothing while something runs.
        list = list.push(if busy && !current {
            tip(t, item, s.busy.to_owned(), iced::widget::tooltip::Position::Right)
        } else {
            item.into()
        });
    }
    if shown == 0 {
        let none = if app.sessions.is_empty() {
            s.no_sessions
        } else {
            s.no_match
        };
        list = list.push(container(text(none).size(12.5).color(t.fg_3)).padding([6, 10]));
    }

    container(
        column![
            container(head).padding([0, 4]).height(30).align_y(Alignment::Center),
            new,
            search,
            container(text(s.sessions).size(11.5).color(t.fg_3)).padding(iced::Padding {
                top: 8.0,
                left: 10.0,
                ..iced::Padding::ZERO
            }),
            scrollable(list)
                .direction(scrollable::Direction::Vertical(
                    scrollable::Scrollbar::new().width(6).scroller_width(6).margin(1)
                ))
                .style(t.scroll())
                .height(Length::Fill),
        ]
        .spacing(8),
    )
    .width(SIDE_W)
    .height(Length::Fill)
    .padding(10)
    .style(t.fill(t.surface))
    .into()
}
