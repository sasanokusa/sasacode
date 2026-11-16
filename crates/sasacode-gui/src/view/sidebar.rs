//! The left panel: this folder's conversations, newest first.

use std::time::SystemTime;

use iced::widget::{Space, button, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Length};

use super::{BOLD, El, MOD, clip, tip};
use crate::app::{App, Msg, SEARCH};
use crate::i18n::fill;
use crate::icons::{Icon, icon};

pub fn sidebar(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    // What the names have room for, in half-width cells, as wide as the list is drawn: 26 for a
    // conversation and 18 for the folder at the width it opens at.
    let width = app.side_width();
    let cells = ((width - 75.0) / 6.5).max(8.0) as usize;
    let folder_cells = ((width - 136.0) / 6.0).max(6.0) as usize;
    let folder = app
        .cwd
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();

    let head = row![
        icon(Icon::Leaf, 16.0, t.accent),
        text("sasacode").size(14).font(BOLD).color(t.fg),
        Space::new().width(Length::Fill),
        text(clip(&folder, folder_cells))
            .size(12)
            .color(t.fg_3)
            .wrapping(text::Wrapping::None),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let new = button(
        row![
            icon(Icon::Plus, 14.0, t.fg),
            text(s.new_session).size(13),
            Space::new().width(Length::Fill),
            text(format!("{MOD}N")).size(11).color(t.fg_3),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([7, 10])
    .style(t.outline(t.fg))
    .on_press(Msg::NewSession);

    let search = container(
        row![
            icon(Icon::Search, 14.0, t.fg_3),
            // Enter opens the first it finds; Esc gives the keyboard back to the input.
            text_input(s.find, &app.filter)
                .id(SEARCH)
                .on_input(Msg::Filter)
                .on_submit(Msg::FindOpen)
                .size(13)
                .padding(0)
                .style(t.input_bare()),
            text(format!("{MOD}K")).size(11).color(t.fg_3),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([6, 10])
    .style(t.boxed(t.raised, t.line, crate::theme::RADIUS));

    let now = SystemTime::now();
    let mut list = column![].spacing(2);
    let mut shown = 0;
    for session in app.found() {
        shown += 1;
        let current = app.chat.session.as_deref() == Some(session.id.as_str());
        let prompt = session.first_prompt.trim();
        let title = if prompt.is_empty() {
            clip(&session.id, 8)
        } else {
            clip(prompt, cells)
        };
        // A title that had to be cut says all of itself under the pointer.
        let whole = (!prompt.is_empty() && title != prompt).then(|| clip(prompt, 600));
        let ago = now.duration_since(session.modified).unwrap_or_default();
        let meta = format!("{} · {}", s.ago(ago), fill(s.msgs, "n", session.message_count));
        let mut title_row = row![text(title).size(13).color(t.fg).wrapping(text::Wrapping::None)]
            .spacing(6)
            .align_y(Alignment::Center);
        // What it is doing, shown or not: it runs, it waits for an answer, or it has news.
        let mark = app.chat_of(&session.id).and_then(|chat| {
            if chat.running() && chat.transcript.pending_approval().is_some() {
                Some((t.warn, s.st_waiting))
            } else if chat.running() {
                Some((t.accent, s.st_running))
            } else if chat.unread {
                Some((t.fg, s.unread))
            } else {
                None
            }
        });
        if let Some((color, _)) = mark {
            title_row = title_row
                .push(Space::new().width(Length::Fill))
                .push(icon(Icon::Dot, 9.0, color));
        }
        let item = button(column![title_row, text(meta).size(11.5).color(t.fg_3)].spacing(2))
            .width(Length::Fill)
            .padding([6, 10])
            .style(t.ghost(t.fg, current))
            .on_press_maybe((!current).then(|| Msg::OpenSession(session.path.clone())));
        // And what the mark means.
        let hint = match (whole, mark) {
            (Some(whole), Some((_, what))) => Some(format!("{whole}\n\n{what}")),
            (Some(whole), None) => Some(whole),
            (None, Some((_, what))) => Some(what.to_owned()),
            (None, None) => None,
        };
        list = list.push(match hint {
            Some(hint) => tip(t, item, hint, iced::widget::tooltip::Position::Right),
            None => item.into(),
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
    .width(width)
    .height(Length::Fill)
    .padding(10)
    .style(t.fill(t.surface))
    .into()
}
