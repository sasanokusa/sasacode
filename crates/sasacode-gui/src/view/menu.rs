//! The menu a right click opens, where the pointer was: on a conversation's row, or on a file.
//! A click anywhere else puts it away, as Esc does.

use iced::widget::{Space, button, column, container, mouse_area, pin, row, stack, text};
use iced::{Alignment, Length};

use super::{El, hline};
use crate::app::{App, Clip, Msg};
use crate::i18n::fill;
use crate::icons::{Icon, icon};
use crate::rows::MenuFor;

const WIDTH: f32 = 240.0;
const ITEM: f32 = 28.0;

/// One line of a menu: what it says, and what it does; none, a line between.
type Item = Option<(Icon, String, Msg, bool)>;

fn row_items(app: &App, id: &str) -> Vec<Item> {
    let (s, w) = (app.s, app.s.w());
    let Some(session) = app.sessions.iter().find(|x| x.id == id) else {
        return Vec::new();
    };
    let current = app.chat.session.as_deref() == Some(id);
    let beside = app.split.as_deref() == Some(id);
    let pinned = app.prefs.pinned.contains(&session.id);
    let unread = app.prefs.more.unread.contains(id) || app.chat_of(id).is_some_and(|c| c.unread);
    let chosen = app.selected.contains(id);
    let mut items: Vec<Item> = Vec::new();
    if !current {
        items.push(Some((
            Icon::Arrow,
            w.menu_open.to_owned(),
            Msg::OpenSession(session.path.clone()),
            false,
        )));
        if !beside {
            items.push(Some((
                Icon::Panel,
                w.c_split.to_owned(),
                Msg::Split(session.path.clone()),
                false,
            )));
        }
    }
    items.push(Some((
        Icon::Plus,
        w.menu_window.to_owned(),
        Msg::NewWindow(app.cwd.clone(), Some(id.to_owned()), None),
        false,
    )));
    items.push(Some((
        Icon::Terminal,
        w.c_tui.to_owned(),
        Msg::RowTui(id.to_owned()),
        false,
    )));
    // Its Markdown, named in the input as context for the conversation shown.
    if !current {
        items.push(Some((
            Icon::File,
            w.attach_conversation.to_owned(),
            Msg::AttachConversation(id.to_owned()),
            false,
        )));
    }
    items.push(None);
    items.push(Some((
        Icon::Dot,
        if unread { w.mark_read } else { w.mark_unread }.to_owned(),
        Msg::MarkUnread(id.to_owned()),
        false,
    )));
    items.push(Some((
        Icon::Pin,
        if pinned { s.unpin } else { s.pin }.to_owned(),
        Msg::Pin(id.to_owned()),
        false,
    )));
    items.push(Some((
        Icon::Pencil,
        s.rename.to_owned(),
        Msg::Rename(id.to_owned()),
        false,
    )));
    items.push(Some((
        Icon::Spark,
        w.title_write.to_owned(),
        Msg::WriteTitle(id.to_owned()),
        false,
    )));
    items.push(Some((
        Icon::Tag,
        w.c_group.to_owned(),
        Msg::Group(id.to_owned()),
        false,
    )));
    items.push(Some((
        Icon::Check,
        if chosen { w.menu_unselect } else { w.menu_select }.to_owned(),
        Msg::Select(id.to_owned()),
        false,
    )));
    items.push(None);
    items.push(Some((
        Icon::Copy,
        w.menu_copy_id.to_owned(),
        Msg::Copy(Clip::Text(id.to_owned())),
        false,
    )));
    items.push(Some((
        Icon::Copy,
        w.menu_copy_log.to_owned(),
        Msg::Copy(Clip::Text(session.path.display().to_string())),
        false,
    )));
    items.push(None);
    items.push(Some((
        Icon::Archive,
        if app.show_archived { s.unarchive } else { s.archive }.to_owned(),
        Msg::Archive(id.to_owned()),
        false,
    )));
    let sure = app.confirm_delete.as_deref() == Some(id);
    items.push(Some((
        Icon::Trash,
        if sure { w.delete_sure } else { w.c_delete }.to_owned(),
        Msg::Delete(id.to_owned()),
        true,
    )));
    items
}

/// Each link of a paragraph of a reply: opened, copied, and a file of the folder in the editor or
/// the file manager too.
fn link_items(app: &App, links: &[String]) -> Vec<Item> {
    let (s, w) = (app.s, app.s.w());
    let mut items: Vec<Item> = Vec::new();
    for link in links.iter().take(4) {
        if !items.is_empty() {
            items.push(None);
        }
        let short = crate::view::clip(link, 44);
        items.push(Some((
            Icon::Arrow,
            fill(w.menu_open_link, "link", &short),
            Msg::Link(link.clone()),
            false,
        )));
        items.push(Some((
            Icon::Copy,
            fill(w.menu_copy_link, "link", &short),
            Msg::Copy(Clip::Text(link.clone())),
            false,
        )));
        if let Some((path, line)) = crate::files::linked_file(link, &app.cwd) {
            items.push(Some((
                Icon::Pencil,
                s.open_editor.to_owned(),
                match line {
                    Some(line) => Msg::EditorAt(path.clone(), line),
                    None => Msg::Editor(path.clone()),
                },
                false,
            )));
            items.push(Some((Icon::Folder, s.reveal.to_owned(), Msg::Reveal(path), false)));
        }
    }
    items
}

fn file_items(app: &App, path: &std::path::Path) -> Vec<Item> {
    let (s, w) = (app.s, app.s.w());
    let mut items: Vec<Item> = Vec::new();
    if path.is_file() {
        items.push(Some((
            Icon::File,
            w.view.to_owned(),
            Msg::ViewFile(path.to_owned(), None),
            false,
        )));
    }
    if path.exists() {
        items.push(Some((
            Icon::Pencil,
            s.open_editor.to_owned(),
            Msg::Editor(path.to_owned()),
            false,
        )));
        items.push(Some((
            Icon::Folder,
            s.reveal.to_owned(),
            Msg::Reveal(path.to_owned()),
            false,
        )));
    }
    items.push(None);
    items.push(Some((
        Icon::Plus,
        w.menu_mention.to_owned(),
        Msg::Dropped(path.to_owned()),
        false,
    )));
    let shown = path.strip_prefix(&app.cwd).unwrap_or(path);
    items.push(Some((
        Icon::Copy,
        w.menu_copy_path.to_owned(),
        Msg::Copy(Clip::Text(shown.display().to_string())),
        false,
    )));
    items
}

/// How the git tab draws its diffs, each way marked when it is on; and all of them folded or
/// opened.
fn diff_view_items(app: &App) -> Vec<Item> {
    let (w, more) = (app.s.w(), &app.prefs.more);
    let mark = |on: bool| if on { Icon::Check } else { Icon::Blank };
    vec![
        Some((
            mark(more.split_diff),
            w.split_diff.to_owned(),
            Msg::SetSplitDiff(!more.split_diff),
            false,
        )),
        Some((
            mark(more.hide_whitespace),
            w.hide_ws.to_owned(),
            Msg::SetWhitespace(!more.hide_whitespace),
            false,
        )),
        Some((
            mark(more.diff_nowrap),
            w.diff_nowrap.to_owned(),
            Msg::SetDiffNowrap(!more.diff_nowrap),
            false,
        )),
        None,
        Some((Icon::ChevronRight, w.fold_all.to_owned(), Msg::FoldFiles(true), false)),
        Some((Icon::ChevronDown, w.open_all.to_owned(), Msg::FoldFiles(false), false)),
    ]
}

/// The menu over the window, if one is open.
pub fn layer(app: &App) -> Option<El<'_>> {
    let menu = app.menu.as_ref()?;
    let t = app.t;
    let items = match &menu.what {
        MenuFor::Row(id) => row_items(app, id),
        MenuFor::File(path) => file_items(app, path),
        MenuFor::Links(links) => link_items(app, links),
        MenuFor::DiffView => diff_view_items(app),
    };
    if items.is_empty() {
        return None;
    }
    let mut height = 10.0;
    let mut list = column![].spacing(0);
    for item in items {
        match item {
            None => {
                height += 9.0;
                list = list.push(container(hline(t)).padding([4, 6]));
            }
            Some((mark, label, msg, danger)) => {
                height += ITEM;
                let color = if danger { t.err } else { t.fg };
                list = list.push(
                    button(
                        row![
                            icon(mark, 13.0, if danger { t.err } else { t.fg_3 }),
                            text(label).size(12.5).color(color).wrapping(text::Wrapping::None),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                    )
                    .width(Length::Fill)
                    .height(ITEM)
                    .padding([0, 10])
                    .style(t.ghost(color, false))
                    .on_press(Msg::MenuDo(Box::new(msg))),
                );
            }
        }
    }
    // Where the pointer was, moved in as far as it must to be seen whole.
    let x = menu.at.x.min(app.width - WIDTH - 6.0).max(4.0);
    let y = menu.at.y.min(app.height - height - 6.0).max(4.0);
    // A press between its items is the menu's, not the layer's under it that closes it.
    let boxed = iced::widget::opaque(
        container(list)
            .width(WIDTH)
            .padding(5)
            .style(t.floating(t.raised, t.line)),
    );
    let away = mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
        .on_press(Msg::CloseMenu)
        .on_right_press(Msg::CloseMenu);
    Some(
        stack![away, pin(boxed).x(x).y(y)]
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
    )
}

/// The bar over the list while conversations are chosen together: how many, and what can be done
/// to all of them.
pub fn bulk_bar(app: &App) -> Option<El<'_>> {
    if app.selected.is_empty() {
        return None;
    }
    let (t, s, w) = (app.t, app.s, app.s.w());
    let sure = app.confirm_delete.as_deref() == Some(crate::rows::BULK);
    let act = |mark: Icon, color, hint: String, msg: Msg| {
        super::tip(
            t,
            button(icon(mark, 13.0, color))
                .padding(4)
                .style(t.ghost(t.fg_3, false))
                .on_press(msg),
            hint,
            iced::widget::tooltip::Position::Bottom,
        )
    };
    use crate::rows::Bulk;
    let bar = row![
        text(fill(w.selected_n, "n", app.selected.len()))
            .size(12)
            .color(t.fg)
            .wrapping(text::Wrapping::None),
        Space::new().width(Length::Fill),
        act(
            Icon::Pin,
            t.fg_3,
            format!("{} / {}", s.pin, s.unpin),
            Msg::Bulk(Bulk::Pin)
        ),
        act(Icon::Tag, t.fg_3, w.c_group.to_owned(), Msg::Bulk(Bulk::Group)),
        act(Icon::Dot, t.fg_3, w.bulk_unread.to_owned(), Msg::Bulk(Bulk::Unread)),
        act(
            Icon::Archive,
            t.fg_3,
            if app.show_archived { s.unarchive } else { s.archive }.to_owned(),
            Msg::Bulk(Bulk::Archive)
        ),
        act(
            Icon::Trash,
            if sure { t.err } else { t.fg_3 },
            if sure {
                fill(w.delete_n_sure, "n", app.selected.len())
            } else {
                w.c_delete.to_owned()
            },
            Msg::Bulk(Bulk::Delete)
        ),
        act(Icon::Close, t.fg_3, w.select_none.to_owned(), Msg::SelectNone),
    ]
    .spacing(0)
    .align_y(Alignment::Center);
    Some(
        container(bar)
            .padding([3, 4])
            .style(t.boxed(t.raised, t.line, crate::theme::RADIUS))
            .into(),
    )
}
