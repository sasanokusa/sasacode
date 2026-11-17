//! The left panel: this folder's conversations, newest first.

use std::time::SystemTime;

use iced::widget::{Space, button, column, container, mouse_area, row, scrollable, text, text_input};
use iced::{Alignment, Length};

use super::{BOLD, El, clip, tip};
use crate::app::{App, Msg, SEARCH};
use crate::i18n::fill;
use crate::icons::{Icon, icon};
use crate::layout::{Order, Show};
use crate::list::Section;
use sasacode_protocol::SessionSummary;

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
            text(super::keys_for(crate::keys::Act::New)).size(11).color(t.fg_3),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([7, 10])
    .style(t.outline(t.fg))
    .on_press(Msg::NewSession);

    let w = s.w();
    let more = &app.prefs.more;
    let filtered = more.list_show != Show::All || more.list_order != Order::Updated;
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
            text(super::keys_for(crate::keys::Act::Find)).size(11).color(t.fg_3),
            tip(
                t,
                button(icon(Icon::Filter, 12.0, if filtered { t.accent } else { t.fg_3 }))
                    .padding(2)
                    .style(t.ghost(t.fg_3, app.list_tools))
                    .on_press(Msg::ListTools),
                w.list_filter.to_owned(),
                iced::widget::tooltip::Position::Bottom,
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([6, 10])
    .style(t.boxed(t.raised, t.line, crate::theme::RADIUS));

    let now = SystemTime::now();
    let mut list = column![].spacing(2);
    let mut shown = 0;
    let section = |label: String| {
        container(text(label).size(11.5).color(t.fg_3).wrapping(text::Wrapping::None)).padding(iced::Padding {
            top: 6.0,
            left: 10.0,
            bottom: 2.0,
            ..iced::Padding::ZERO
        })
    };
    for listed in app.listed() {
        list = list.push(match &listed.section {
            Section::Pinned => section(s.pinned.to_owned()).into(),
            Section::Rest => section(s.sessions.to_owned()).into(),
            Section::Archived => section(s.archived.to_owned()).into(),
            Section::Group(name) => group_head(app, name, listed.folded, folder_cells + 8),
        });
        for session in listed.rows {
            shown += 1;
            list = list.push(row_of(app, session, now, cells));
        }
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
    if !app.others.is_empty() {
        list = list.push(other_folders(app, folder_cells + 8));
    }
    if shown == 0 {
        let none = if app.sessions.is_empty() {
            s.no_sessions
        } else {
            s.no_match
        };
        list = list.push(container(text(none).size(12.5).color(t.fg_3)).padding([6, 10]));
    }

    // Which are listed, and in which order: open from the search's button, and kept open while it
    // leaves some out.
    let mut top = column![
        container(head).padding([0, 4]).height(30).align_y(Alignment::Center),
        new,
        search,
    ]
    .spacing(8);
    if app.list_tools || filtered {
        top = top.push(filters(app));
    }
    if let Some(bar) = super::menu::bulk_bar(app) {
        top = top.push(bar);
    }
    container(
        column![
            top,
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

/// The other folders sasacode was used in, folded or not: each opens in a window of its own, with
/// its conversations listed there.
fn other_folders(app: &App, cells: usize) -> El<'_> {
    let (t, w) = (app.t, app.s.w());
    let open = app.prefs.more.folders_open;
    let mut head = row![
        icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 11.0, t.fg_3),
        text(w.other_folders)
            .size(11.5)
            .color(t.fg_3)
            .wrapping(text::Wrapping::None),
    ]
    .spacing(4)
    .align_y(Alignment::Center);
    if !open {
        head = head.push(text(format!("({})", app.others.len())).size(11).color(t.fg_3));
    }
    let mut col = column![
        button(head)
            .width(Length::Fill)
            .padding(iced::Padding {
                top: 10.0,
                left: 8.0,
                bottom: 2.0,
                right: 8.0,
            })
            .style(t.ghost(t.fg_3, false))
            .on_press(Msg::FoldFolders)
    ]
    .spacing(1);
    if !open {
        return col.into();
    }
    for folder in &app.others {
        let name = folder
            .file_name()
            .map_or_else(|| folder.display().to_string(), |n| n.to_string_lossy().into_owned());
        let parent = folder.parent().map(|p| p.display().to_string()).unwrap_or_default();
        let line = row![
            icon(Icon::Folder, 12.0, t.fg_3),
            text(clip(&name, cells / 2 + 4))
                .size(12.5)
                .color(t.fg_2)
                .wrapping(text::Wrapping::None),
            text(clip(&parent, cells / 2))
                .size(11)
                .color(t.fg_3)
                .wrapping(text::Wrapping::None),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        col = col.push(tip(
            t,
            button(line)
                .width(Length::Fill)
                .padding([4, 10])
                .style(t.ghost(t.fg_2, false))
                .on_press(Msg::OpenProject(folder.clone())),
            format!("{}\n{}", folder.display(), w.other_folders_tip),
            iced::widget::tooltip::Position::Right,
        ));
    }
    col.into()
}

/// Which conversations are listed, and in which order.
fn filters(app: &App) -> El<'_> {
    let t = app.t;
    let w = app.s.w();
    let more = &app.prefs.more;
    let chip = |label: &'static str, on: bool, msg: Msg| -> El<'_> {
        let b = button(text(label).size(11.5).wrapping(text::Wrapping::None))
            .padding([2, 8])
            .on_press(msg);
        if on {
            b.style(t.primary()).into()
        } else {
            b.style(t.outline(t.fg_2)).into()
        }
    };
    let shows = [
        (w.show_all, Show::All),
        (w.show_running, Show::Running),
        (w.show_waiting, Show::Waiting),
        (w.show_unread, Show::Unread),
    ];
    let orders = [(w.order_updated, Order::Updated), (w.order_created, Order::Created)];
    let mut line = iced::widget::Row::new().spacing(4);
    for (label, show) in shows {
        line = line.push(chip(label, more.list_show == show, Msg::ListShow(show)));
    }
    let mut order = iced::widget::Row::new().spacing(4);
    for (label, by) in orders {
        order = order.push(chip(label, more.list_order == by, Msg::ListOrder(by)));
    }
    column![line.wrap(), order.wrap()].spacing(4).into()
}

/// A group's line in the list: its name, folded or not, and how many it hides while folded.
fn group_head<'a>(app: &'a App, name: &str, folded: usize, cells: usize) -> El<'a> {
    let t = app.t;
    let shut = folded > 0;
    let mut line = row![
        icon(if shut { Icon::ChevronRight } else { Icon::ChevronDown }, 11.0, t.fg_3),
        text(clip(name, cells))
            .size(11.5)
            .color(t.fg_3)
            .wrapping(text::Wrapping::None),
    ]
    .spacing(4)
    .align_y(Alignment::Center);
    if shut {
        line = line.push(text(format!("({folded})")).size(11).color(t.fg_3));
    }
    button(line)
        .width(Length::Fill)
        .padding(iced::Padding {
            top: 6.0,
            left: 8.0,
            bottom: 2.0,
            right: 8.0,
        })
        .style(t.ghost(t.fg_3, false))
        .on_press(Msg::FoldGroup(name.to_owned()))
        .into()
}

/// A conversation's row: its name or first message, how long ago and how long, what it is doing,
/// and, under the pointer, what can be done to it.
fn row_of<'a>(app: &'a App, session: &'a SessionSummary, now: SystemTime, cells: usize) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let pinned = app.prefs.pinned.contains(&session.id);
    let current = app.chat.session.as_deref() == Some(session.id.as_str());
    // Shown beside: marked as the one shown is, and taken into the main place by a click.
    let beside = app.split.as_deref() == Some(session.id.as_str());
    // The name it was given, else its first message, else the start of its id.
    let named = app.prefs.names.get(&session.id).map(String::as_str);
    let prompt = named.unwrap_or(session.first_prompt.trim());
    let hovered = app.row_hover.as_deref() == Some(session.id.as_str());
    // Room for what can be done to it, while the pointer is on it.
    let room = if hovered { cells.saturating_sub(15) } else { cells };
    let title = if prompt.is_empty() {
        clip(&session.id, 8)
    } else {
        clip(prompt, room)
    };
    // A title that had to be cut says all of itself under the pointer.
    let whole = (!prompt.is_empty() && title != prompt).then(|| clip(prompt, 600));
    let ago = now.duration_since(session.modified).unwrap_or_default();
    let mut meta = format!("{} · {}", s.ago(ago), fill(s.msgs, "n", session.message_count));
    // The pull request of the branch it was started on, once the git tab has read it.
    let pr = app.pr_of(&session.id);
    if let Some((n, _)) = pr {
        meta.push_str(&format!(" · #{n}"));
    }
    let chosen = app.selected.contains(&session.id);
    let mut title_row = row![].spacing(6).align_y(Alignment::Center);
    if chosen {
        title_row = title_row.push(icon(Icon::Check, 12.0, t.accent));
    }
    if pinned {
        title_row = title_row.push(icon(Icon::Pin, 11.0, t.fg_3));
    }
    title_row = title_row.push(text(title).size(13).color(t.fg).wrapping(text::Wrapping::None));
    // What it is doing, shown or not: it runs, it waits for an answer, or it has news.
    let marked = app.prefs.more.unread.contains(&session.id);
    let mark = app
        .chat_of(&session.id)
        .map_or(marked.then_some((t.fg, s.unread)), |chat| {
            if chat.running() && chat.transcript.waits() {
                Some((t.warn, s.st_waiting))
            } else if chat.running() {
                Some((t.accent, s.st_running))
            } else if chat.unread || marked {
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
        .style(t.ghost(t.fg, current || beside || chosen))
        .on_press_maybe(if app.modifiers.shift() {
            // With ⇧ held, chosen with others, the one shown too.
            Some(Msg::Select(session.id.clone()))
        } else {
            (!current).then(|| {
                // With ⌘ held, beside the one shown.
                if app.modifiers.command() && !beside {
                    Msg::Split(session.path.clone())
                } else {
                    Msg::OpenSession(session.path.clone())
                }
            })
        });
    // Pinned, named, grouped, archived or deleted from the row the pointer is on.
    let act = |mark: Icon, color, hint: &'static str, msg: Msg| {
        tip(
            t,
            button(icon(mark, 13.0, color))
                .padding(3)
                .style(t.ghost(t.fg_3, false))
                .on_press(msg),
            hint.to_owned(),
            iced::widget::tooltip::Position::Bottom,
        )
    };
    let actions: El<'_> = if hovered {
        let w = s.w();
        let id = session.id.clone();
        // Red once pressed: the next press deletes.
        let sure = app.confirm_delete.as_deref() == Some(id.as_str());
        row![
            act(
                Icon::Pin,
                t.fg_3,
                if pinned { s.unpin } else { s.pin },
                Msg::Pin(id.clone())
            ),
            act(Icon::Pencil, t.fg_3, s.rename, Msg::Rename(id.clone())),
            act(Icon::Tag, t.fg_3, w.c_group, Msg::Group(id.clone())),
            act(
                Icon::Archive,
                t.fg_3,
                if app.show_archived { s.unarchive } else { s.archive },
                Msg::Archive(id.clone())
            ),
            act(
                Icon::Trash,
                if sure { t.err } else { t.fg_3 },
                if sure { w.delete_sure } else { w.c_delete },
                Msg::Delete(id)
            ),
        ]
        .spacing(0)
        .align_y(Alignment::Center)
        .into()
    } else {
        Space::new().into()
    };
    // And what the mark means, its model, the branch it was started on and that branch's pull
    // request.
    let w = s.w();
    let mut about: Vec<String> = Vec::new();
    if let Some((_, what)) = mark {
        about.push(what.to_owned());
    }
    if let Some(model) = &session.model {
        about.push(fill(w.row_model, "model", model));
    }
    if let Some(branch) = app.prefs.branches.get(&session.id) {
        about.push(fill(w.row_branch, "branch", branch));
    }
    if let Some((n, state)) = pr {
        about.push(fill(fill(w.row_pr, "n", n).as_str(), "state", state.to_lowercase()));
    }
    let about = (!about.is_empty()).then(|| about.join("\n"));
    let hint = match (whole, about) {
        (Some(whole), Some(about)) => Some(format!("{whole}\n\n{about}")),
        (Some(whole), None) => Some(whole),
        (None, about) => about,
    };
    // Beside the whole row, past its buttons: beside the title alone it covered them.
    let line = row![item, actions].spacing(2).align_y(Alignment::Center);
    let line: El<'_> = match hint {
        Some(hint) => tip(t, line, hint, iced::widget::tooltip::Position::Right),
        None => line.into(),
    };
    mouse_area(line)
        .on_enter(Msg::RowHover(Some(session.id.clone())))
        .on_exit(Msg::RowHover(None))
        .on_right_press(Msg::OpenMenu(crate::rows::MenuFor::Row(session.id.clone())))
        .into()
}
