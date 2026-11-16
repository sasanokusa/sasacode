//! The right panel: what the conversation changed, its plan, the commands it ran and what it used.

use iced::widget::{Column, Space, button, column, container, row, scrollable, text};
use iced::{Alignment, Length};

use iced::widget::tooltip;

use super::timeline::{command_copies, todo_list};
use super::{
    BOLD, El, MOD, MONO, clip, clip_start, diff_view, first_line, hline, insp_w, output_block, plus_minus, short_path,
    tag, tip,
};
use crate::app::{App, Msg, Tab};
use crate::i18n::{compact, fill};
use crate::icons::{Icon, icon};
use crate::theme::{RADIUS, RADIUS_L};
use crate::transcript::{TodoStatus, ToolState};

pub fn inspector(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let changes = app.transcript.changes().count();
    let commands = app.transcript.commands().count();
    let plan = app.transcript.plan().map(|p| {
        format!(
            "{}/{}",
            p.iter().filter(|todo| todo.status == TodoStatus::Completed).count(),
            p.len()
        )
    });
    // Words only: with an icon each, the four do not fit the panel in English.
    let tab = |tab: Tab, key: u8, label: &'static str, count: Option<String>| -> El<'_> {
        let on = app.tab == tab;
        let mut content = row![
            text(label)
                .size(12.5)
                .color(if on { t.fg } else { t.fg_2 })
                .wrapping(text::Wrapping::None)
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        if let Some(count) = count {
            content = content.push(
                text(count)
                    .size(11.5)
                    .font(MONO)
                    .color(if on { t.accent } else { t.fg_3 }),
            );
        }
        tip(
            t,
            button(content)
                .padding([5, 10])
                .style(t.ghost(t.fg_2, on))
                .on_press(Msg::Tab(tab)),
            format!("{MOD}{key}"),
            tooltip::Position::Bottom,
        )
    };
    let tabs = row![
        tab(
            Tab::Changes,
            1,
            s.tab_changes,
            (changes > 0).then(|| changes.to_string())
        ),
        tab(Tab::Plan, 2, s.tab_plan, plan),
        tab(
            Tab::Commands,
            3,
            s.tab_cmd,
            (commands > 0).then(|| commands.to_string())
        ),
        tab(Tab::Usage, 4, s.tab_usage, None),
    ]
    .spacing(2);

    let body = match app.tab {
        Tab::Changes => changes_tab(app),
        Tab::Plan => plan_tab(app),
        Tab::Commands => commands_tab(app),
        Tab::Usage => usage_tab(app),
    };
    container(
        column![
            container(tabs).padding([0, 8]).height(46).align_y(Alignment::Center),
            hline(t),
            scrollable(container(body).padding(12).width(Length::Fill))
                .direction(scrollable::Direction::Vertical(
                    scrollable::Scrollbar::new().width(8).scroller_width(8).margin(2)
                ))
                .style(t.scroll())
                .height(Length::Fill),
        ]
        .width(Length::Fill),
    )
    .width(insp_w(app))
    .height(Length::Fill)
    .style(t.fill(t.surface))
    .into()
}

/// What a tab says while it has nothing to show.
fn empty<'a>(app: &App, mark: Icon, head: &'a str, body: &'a str) -> El<'a> {
    let t = app.t;
    container(
        column![
            icon(mark, 22.0, t.line_2),
            text(head).size(13.5).font(BOLD).color(t.fg_2),
            text(body).size(12.5).color(t.fg_3).align_x(Alignment::Center),
        ]
        .spacing(8)
        .align_x(Alignment::Center),
    )
    .center_x(Length::Fill)
    .padding([48, 20])
    .into()
}

/// How many half-width cells of monospaced text a card's head has room for, when `rest` of the
/// panel's width goes to everything else in it.
fn cells(app: &App, rest: f32) -> usize {
    ((insp_w(app) - rest) / 8.0).max(8.0) as usize
}

/// The name in a card's head: it takes the room the rest of the head leaves and is cut there, so
/// nothing is drawn over what follows it when the guess in [`cells`] was too generous.
fn name<'a>(label: iced::widget::Text<'a>) -> El<'a> {
    container(label.wrapping(text::Wrapping::None))
        .width(Length::Fill)
        .clip(true)
        .into()
}

/// A card with a head that folds its body away.
fn card<'a>(app: &'a App, id: &'a str, head: El<'a>, body: Option<El<'a>>) -> El<'a> {
    let t = app.t;
    let open = !app.closed.contains(id);
    let mut col = column![
        button(
            row![
                head,
                icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 11.0, t.fg_3)
            ]
            .spacing(8)
            .align_y(Alignment::Center)
        )
        .width(Length::Fill)
        .padding([7, 10])
        .style(t.ghost(t.fg, false))
        .on_press(Msg::Card(id.to_owned()))
    ];
    if let Some(body) = body.filter(|_| open) {
        col = col.push(container(body).padding(iced::Padding {
            top: 0.0,
            right: 8.0,
            bottom: 8.0,
            left: 8.0,
        }));
    }
    container(col)
        .width(Length::Fill)
        .style(t.boxed(t.bg, t.line, RADIUS_L))
        .into()
}

fn changes_tab(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let mut col = column![].spacing(10);
    if app.transcript.changes().next().is_none() {
        return empty(app, Icon::Diff, s.ch_empty, s.ch_empty_p);
    }
    if app.loaded {
        col = col.push(text(s.ch_from_args).size(12).color(t.fg_3));
    }
    for (tool, change) in app.transcript.changes() {
        let path = clip_start(&short_path(&change.path, &app.cwd), cells(app, 110.0));
        let mut head = row![
            icon(
                if change.created == Some(true) {
                    Icon::File
                } else {
                    Icon::Pencil
                },
                13.0,
                t.fg_3
            ),
            name(text(path).size(12.5).font(MONO).color(t.fg)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if change.created == Some(true) {
            head = head.push(tag(t, s.new_file, t.accent_wash, t.accent));
        }
        head = head.push(plus_minus(t, change.diff.added, change.diff.removed));
        col = col.push(card(
            app,
            &tool.id,
            head.into(),
            Some(diff_view(t, s, &change.diff, 200)),
        ));
    }
    col.into()
}

fn plan_tab(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let Some(todos) = app.transcript.plan().filter(|p| !p.is_empty()) else {
        return empty(app, Icon::List, s.pl_empty, s.pl_empty_p);
    };
    let done = todos.iter().filter(|todo| todo.status == TodoStatus::Completed).count();
    // How far along, as a bar: the done part and the rest share the width by their counts.
    let mut bar = row![].height(4);
    if done > 0 {
        bar = bar.push(
            container(Space::new())
                .width(Length::FillPortion(done as u16))
                .height(4)
                .style(t.tag(t.accent, t.accent)),
        );
    }
    if done < todos.len() {
        bar = bar.push(
            container(Space::new())
                .width(Length::FillPortion((todos.len() - done) as u16))
                .height(4)
                .style(t.tag(t.line, t.line)),
        );
    }
    column![
        text(fill(&fill(s.pl_done, "a", done), "b", todos.len()))
            .size(12.5)
            .color(t.fg_2),
        bar.spacing(2),
        container(todo_list(t, todos)).padding([6, 2]),
    ]
    .spacing(8)
    .into()
}

fn commands_tab(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    // How much of a command its card's head has room for, next to how it ended and how long it took.
    let head_cells = cells(app, 200.0);
    let mut col = column![].spacing(10);
    if app.transcript.commands().next().is_none() {
        return empty(app, Icon::Terminal, s.cm_empty, s.cm_empty_p);
    }
    for tool in app.transcript.commands() {
        let command = tool.arg("command").unwrap_or(&tool.summary);
        let state = match (tool.state, tool.killed.as_deref(), tool.exit_code) {
            (ToolState::Running, _, _) => tag(t, s.running, t.accent_wash, t.accent),
            (_, Some("timeout"), _) => tag(t, s.killed_timeout, t.err_wash, t.err),
            (_, Some(_), _) => tag(t, s.killed_abort, t.inset, t.fg_2),
            (_, None, Some(0)) => tag(t, fill(s.exit, "n", 0), t.inset, t.fg_2),
            (_, None, Some(code)) => tag(t, fill(s.exit, "n", code), t.err_wash, t.err),
            (ToolState::Failed, None, None) => tag(t, s.tag_failed, t.err_wash, t.err),
            _ => Space::new().into(),
        };
        let mut head = row![
            text("$").size(12.5).font(MONO).color(t.fg_3),
            name(
                text(clip(first_line(command), head_cells))
                    .size(12.5)
                    .font(MONO)
                    .color(t.fg)
            ),
            state,
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if let Some(d) = tool.elapsed.filter(|d| d.as_millis() >= 1000) {
            head = head.push(
                text(s.duration(d))
                    .size(11.5)
                    .color(t.fg_3)
                    .wrapping(text::Wrapping::None),
            );
        }
        let mut body: Vec<El<'_>> = Vec::new();
        // The whole command when the head had to cut it.
        if command.lines().count() > 1 || clip(command, head_cells) != command {
            body.push(
                container(
                    text(command)
                        .size(12)
                        .font(MONO)
                        .color(t.fg)
                        .wrapping(text::Wrapping::WordOrGlyph),
                )
                .width(Length::Fill)
                .padding([6, 10])
                .style(t.boxed(t.raised, t.line, RADIUS))
                .into(),
            );
        }
        // Nothing written yet is not "no output" while the command still runs.
        if tool.state != ToolState::Running || !tool.shown_output().is_empty() {
            body.push(output_block(t, s, tool.shown_output(), 60));
        }
        if tool.arg("command").is_some() {
            body.push(command_copies(app, tool));
        }
        let body = (!body.is_empty()).then(|| Column::with_children(body).spacing(6).into());
        col = col.push(card(app, &tool.id, head.into(), body));
    }
    col.into()
}

fn usage_tab(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let u = &app.transcript.usage;
    let item = |label: &'static str, value: String| {
        row![
            text(label).size(13).color(t.fg_2),
            Space::new().width(Length::Fill),
            text(value).size(13).font(MONO).color(t.fg),
        ]
        .padding([5, 2])
        .align_y(Alignment::Center)
    };
    let tokens = |n: u64| {
        if n < 1000 {
            n.to_string()
        } else {
            format!("{} ({n})", compact(n))
        }
    };
    column![
        text(s.us_head).size(12).color(t.fg_3),
        container(
            column![
                item(s.us_in, tokens(u.input)),
                hline(t),
                item(s.us_out, tokens(u.output)),
                hline(t),
                item(s.us_cr, tokens(u.cache_read)),
                hline(t),
                item(s.us_cw, tokens(u.cache_write)),
                hline(t),
                item(s.us_req, app.transcript.requests.to_string()),
                hline(t),
                item(s.us_last, tokens(app.transcript.last_input)),
                hline(t),
                item(s.us_cost, format!("${:.4}", u.cost)),
            ]
            .padding([4, 10])
        )
        .style(t.boxed(t.bg, t.line, RADIUS_L)),
        text(s.us_note).size(12).color(t.fg_3),
    ]
    .spacing(10)
    .into()
}
