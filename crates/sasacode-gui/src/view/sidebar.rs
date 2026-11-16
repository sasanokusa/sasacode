//! The left panel: this folder's conversations, newest first.

use std::time::SystemTime;

use iced::widget::{Space, button, column, container, mouse_area, row, scrollable, text, text_input};
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
    let section = |label: &'static str| {
        container(text(label).size(11.5).color(t.fg_3)).padding(iced::Padding {
            top: 6.0,
            left: 10.0,
            bottom: 2.0,
            ..iced::Padding::ZERO
        })
    };
    let mut pins_done = app.show_archived;
    for session in app.found() {
        shown += 1;
        let pinned = app.prefs.pinned.contains(&session.id);
        if !pins_done && !pinned {
            pins_done = true;
            if shown > 1 {
                list = list.push(section(s.sessions));
            }
        }
        let current = app.chat.session.as_deref() == Some(session.id.as_str());
        // The name it was given, else its first message, else the start of its id.
        let named = app.prefs.names.get(&session.id).map(String::as_str);
        let prompt = named.unwrap_or(session.first_prompt.trim());
        let hovered = app.row_hover.as_deref() == Some(session.id.as_str());
        // Room for what can be done to it, while the pointer is on it.
        let room = if hovered { cells.saturating_sub(10) } else { cells };
        let title = if prompt.is_empty() {
            clip(&session.id, 8)
        } else {
            clip(prompt, room)
        };
        // A title that had to be cut says all of itself under the pointer.
        let whole = (!prompt.is_empty() && title != prompt).then(|| clip(prompt, 600));
        let ago = now.duration_since(session.modified).unwrap_or_default();
        let meta = format!("{} · {}", s.ago(ago), fill(s.msgs, "n", session.message_count));
        let mut title_row = row![].spacing(6).align_y(Alignment::Center);
        if pinned {
            title_row = title_row.push(icon(Icon::Pin, 11.0, t.fg_3));
        }
        title_row = title_row.push(text(title).size(13).color(t.fg).wrapping(text::Wrapping::None));
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
                .push(icon(Icon::Dot, 15.0, color));
        }
        let item = button(column![title_row, text(meta).size(11.5).color(t.fg_3)].spacing(2))
            .width(Length::Fill)
            .padding([6, 10])
            .style(t.ghost(t.fg, current))
            .on_press_maybe((!current).then(|| Msg::OpenSession(session.path.clone())));
        // Pinned, named or archived from the row the pointer is on.
        let act = |mark: Icon, hint: &'static str, msg: Msg| {
            tip(
                t,
                button(icon(mark, 13.0, t.fg_3))
                    .padding(4)
                    .style(t.ghost(t.fg_3, false))
                    .on_press(msg),
                hint.to_owned(),
                iced::widget::tooltip::Position::Bottom,
            )
        };
        let actions: El<'_> = if hovered {
            let id = session.id.clone();
            row![
                act(Icon::Pin, if pinned { s.unpin } else { s.pin }, Msg::Pin(id.clone())),
                act(Icon::Pencil, s.rename, Msg::Rename(id.clone())),
                act(
                    Icon::Archive,
                    if app.show_archived { s.unarchive } else { s.archive },
                    Msg::Archive(id)
                ),
            ]
            .spacing(0)
            .align_y(Alignment::Center)
            .into()
        } else {
            Space::new().into()
        };
        // And what the mark means.
        let hint = match (whole, mark) {
            (Some(whole), Some((_, what))) => Some(format!("{whole}\n\n{what}")),
            (Some(whole), None) => Some(whole),
            (None, Some((_, what))) => Some(what.to_owned()),
            (None, None) => None,
        };
        let item: El<'_> = match hint {
            Some(hint) => tip(t, item, hint, iced::widget::tooltip::Position::Right),
            None => item.into(),
        };
        list = list.push(
            mouse_area(row![item, actions].spacing(2).align_y(Alignment::Center))
                .on_enter(Msg::RowHover(Some(session.id.clone())))
                .on_exit(Msg::RowHover(None)),
        );
    }
    // The archived conversations, or the way back from them.
    let archived = app
        .sessions
        .iter()
        .filter(|x| app.prefs.archived.contains(&x.id))
        .count();
    if app.show_archived || archived > 0 {
        let label = if app.show_archived {
            s.back_to_list.to_owned()
        } else {
            fill(s.archived_n, "n", archived)
        };
        list = list.push(
            button(
                row![icon(Icon::Archive, 12.0, t.fg_3), text(label).size(12).color(t.fg_3)]
                    .spacing(6)
                    .align_y(Alignment::Center),
            )
            .padding([6, 10])
            .style(t.ghost(t.fg_3, false))
            .on_press(Msg::Command(crate::input::Command::Archived)),
        );
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
            container(
                text(if app.show_archived {
                    s.archived
                } else if app.found().next().is_some_and(|x| app.prefs.pinned.contains(&x.id)) {
                    s.pinned
                } else {
                    s.sessions
                })
                .size(11.5)
                .color(t.fg_3)
            )
            .padding(iced::Padding {
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
