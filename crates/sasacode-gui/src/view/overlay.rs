//! What is drawn over the window: the command palette, the list of keys, the settings, a name for
//! a conversation, and a side question with its answer. A click beside it puts it away, as Esc does.

use iced::widget::{Space, button, column, container, mouse_area, row, scrollable, text, text_input, toggler};
use iced::{Alignment, Color, Length};

use super::timeline::reply;
use super::{BOLD, El, MOD, MONO};
use crate::app::{App, Msg, Overlay, PALETTE, RENAME};
use crate::i18n::fill;
use crate::icons::{Icon, icon};
use crate::layout::{ZOOM_MAX, ZOOM_MIN};
use crate::theme::RADIUS_L;
use crate::transcript::Step;

/// How many of the palette's entries are in view at once.
const LISTED: usize = 12;

/// `base`, and over it what is drawn over the window, if anything.
pub fn over<'a>(app: &'a App, base: El<'a>) -> El<'a> {
    let Some(overlay) = &app.overlay else {
        return base;
    };
    let t = app.t;
    let (panel, width, top) = match overlay {
        Overlay::Palette { query, at } => (palette(app, query, *at), 580.0, 70.0),
        Overlay::Keys => (keys(app), 560.0, 60.0),
        Overlay::Settings => (settings(app), 560.0, 60.0),
        Overlay::Rename { name, .. } => (rename(app, name), 460.0, 120.0),
        Overlay::Side => (side(app), 680.0, 50.0),
    };
    let shade = Color {
        a: if t.dark { 0.45 } else { 0.25 },
        ..Color::BLACK
    };
    let backdrop = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(t.fill(shade)),
    )
    .on_press(Msg::Show(None));
    let panel = container(
        container(panel)
            .max_width(width)
            .padding(16)
            .style(t.floating(t.raised, t.line_2)),
    )
    .center_x(Length::Fill)
    .padding(iced::Padding {
        top,
        right: 16.0,
        bottom: 16.0,
        left: 16.0,
    });
    // Clicks on the panel stay on it; those beside it reach the backdrop.
    iced::widget::stack![base, backdrop, mouse_area(panel)].into()
}

fn title<'a>(app: &App, label: &'a str) -> El<'a> {
    let t = app.t;
    row![
        text(label).size(15).font(BOLD).color(t.fg),
        Space::new().width(Length::Fill),
        button(icon(Icon::Close, 14.0, t.fg_3))
            .padding(4)
            .style(t.ghost(t.fg_3, false))
            .on_press(Msg::Show(None)),
    ]
    .align_y(Alignment::Center)
    .into()
}

fn palette<'a>(app: &'a App, query: &'a str, at: usize) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let found = app.found_entries();
    let field = text_input(s.palette_ph, query)
        .id(PALETTE)
        .on_input(Msg::OverlayText)
        .on_submit(Msg::PaletteRun(at))
        .size(14)
        .padding([8, 10])
        .style(t.input());
    let mut list = column![].spacing(1);
    if found.is_empty() {
        list = list.push(container(text(s.nothing).size(13).color(t.fg_3)).padding([8, 10]));
    }
    for (i, entry) in found.into_iter().enumerate() {
        let mut line = row![text(entry.label).size(13).color(t.fg).width(Length::Fill)]
            .spacing(10)
            .align_y(Alignment::Center);
        if let Some(keys) = entry.keys {
            line = line.push(text(keys).size(11.5).font(MONO).color(t.fg_3));
        }
        list = list.push(
            button(line)
                .width(Length::Fill)
                .padding([6, 10])
                .style(t.ghost(t.fg, i == at))
                .on_press(Msg::PaletteRun(i)),
        );
    }
    let body = column![
        field,
        scrollable(list)
            .height(Length::Shrink)
            .style(t.scroll())
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new().width(6).scroller_width(6).margin(1)
            )),
    ]
    .spacing(10);
    container(body).max_height(LISTED as f32 * 31.0 + 60.0).into()
}

fn keys(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let send = if app.prefs.enter_sends {
        "↵".to_owned()
    } else {
        format!("{MOD}↵")
    };
    let newline = if app.prefs.enter_sends {
        "⇧↵".to_owned()
    } else {
        "↵".to_owned()
    };
    let mut rows: Vec<(String, String)> = vec![
        (send, s.h_send.to_owned()),
        (newline, s.h_nl.to_owned()),
        (
            if app.prefs.enter_sends {
                format!("{MOD}↵")
            } else {
                format!("{MOD}⇧↵")
            },
            format!("{} / {}", s.h_allow, s.h_now),
        ),
        ("Esc".into(), s.stop.to_owned()),
        ("↑ ↓".into(), s.k_history.to_owned()),
        ("@".into(), s.c_mention.to_owned()),
        ("/".into(), s.k_commands.to_owned()),
    ];
    rows.extend(
        app.entries()
            .into_iter()
            .filter_map(|e| Some((e.keys.filter(|k| k.starts_with(MOD))?, e.label))),
    );
    let mut list = column![].spacing(4);
    for (keys, what) in rows {
        list = list.push(
            row![
                text(keys).size(12).font(MONO).color(t.fg_2).width(110),
                text(what).size(13).color(t.fg),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
    }
    let body = column![
        title(app, s.keys_title),
        scrollable(list).height(Length::Shrink).style(t.scroll()),
    ]
    .spacing(12);
    container(body).max_height(560).into()
}

fn settings(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let p = &app.prefs;
    let choice = |label: String, on: bool, msg: Msg| -> El<'_> {
        let b = button(text(label).size(12.5).wrapping(text::Wrapping::None))
            .padding([5, 10])
            .on_press(msg);
        if on {
            b.style(t.primary()).into()
        } else {
            b.style(t.outline(t.fg)).into()
        }
    };
    let heading = |label: &'static str| text(label).size(12).color(t.fg_3);
    let enter = column![
        heading(s.set_enter),
        row![
            choice(s.enter_send.to_owned(), p.enter_sends, Msg::SetEnter(true)),
            choice(fill(s.enter_nl, "mod", MOD), !p.enter_sends, Msg::SetEnter(false)),
        ]
        .spacing(8)
        .wrap(),
    ]
    .spacing(6);
    let theme = column![
        heading(s.set_theme),
        row![
            choice(s.theme_system.to_owned(), p.dark.is_none(), Msg::SetDark(None)),
            choice(
                s.theme_light.to_owned(),
                p.dark == Some(false),
                Msg::SetDark(Some(false))
            ),
            choice(s.theme_dark.to_owned(), p.dark == Some(true), Msg::SetDark(Some(true))),
        ]
        .spacing(8),
    ]
    .spacing(6);
    let zoom = column![
        heading(s.set_zoom),
        row![
            button(text("−").size(14))
                .padding([3, 12])
                .style(t.outline(t.fg))
                .on_press_maybe((p.zoom > ZOOM_MIN + 0.01).then_some(Msg::Zoom(-1))),
            text(format!("{:.0}%", p.zoom * 100.0))
                .size(13)
                .font(MONO)
                .color(t.fg)
                .width(56)
                .center(),
            button(text("+").size(14))
                .padding([3, 12])
                .style(t.outline(t.fg))
                .on_press_maybe((p.zoom < ZOOM_MAX - 0.01).then_some(Msg::Zoom(1))),
            button(text(s.c_zoom_reset).size(12.5))
                .padding([4, 10])
                .style(t.ghost(t.fg_2, false))
                .on_press(Msg::Zoom(0)),
            text(format!("{MOD}+ / {MOD}- / {MOD}0"))
                .size(11.5)
                .font(MONO)
                .color(t.fg_3),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(6);
    let switch = |label: &'static str, on: bool, msg: fn(bool) -> Msg| {
        toggler(on)
            .label(label)
            .text_size(13)
            .size(18)
            .spacing(10)
            .on_toggle(msg)
    };
    column![
        title(app, s.settings),
        enter,
        theme,
        zoom,
        switch(s.set_notify, p.notify, Msg::SetNotify),
        switch(s.set_awake, p.awake, Msg::SetAwake),
        switch(s.set_steps, p.steps, Msg::SetSteps),
        text(s.set_kept).size(11.5).color(t.fg_3),
    ]
    .spacing(16)
    .into()
}

fn rename<'a>(app: &'a App, name: &'a str) -> El<'a> {
    let (t, s) = (app.t, app.s);
    column![
        title(app, s.rename),
        text_input(s.rename_ph, name)
            .id(RENAME)
            .on_input(Msg::OverlayText)
            .on_submit(Msg::RenameSave)
            .size(14)
            .padding([8, 10])
            .style(t.input()),
        text(s.rename_hint).size(11.5).color(t.fg_3),
        row![
            Space::new().width(Length::Fill),
            button(text(s.close).size(13))
                .padding([6, 12])
                .style(t.outline(t.fg))
                .on_press(Msg::Show(None)),
            button(text(s.save).size(13))
                .padding([6, 14])
                .style(t.primary())
                .on_press(Msg::RenameSave),
        ]
        .spacing(8),
    ]
    .spacing(12)
    .into()
}

/// The side question, and its answer as it comes.
fn side(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let mut col = column![title(app, s.side_title)].spacing(12);
    let Some(aside) = &app.aside else {
        return col.into();
    };
    let Some(ex) = aside.transcript.exchanges.last() else {
        return col.into();
    };
    col = col.push(
        container(text(ex.prompt.as_str()).size(13.5).color(t.fg))
            .padding([8, 12])
            .style(t.boxed(t.inset, t.inset, RADIUS_L)),
    );
    let mut answer = column![].spacing(8);
    for step in &ex.steps {
        match step {
            Step::Text(md) if !md.raw.trim().is_empty() => answer = answer.push(reply(app, md, false)),
            Step::Tool(tool) => {
                answer = answer.push(
                    text(format!("{} {}", tool.name, tool.summary))
                        .size(12)
                        .font(MONO)
                        .color(t.fg_3),
                )
            }
            Step::Note(crate::transcript::Note::Error(body)) => {
                answer = answer.push(text(body.as_str()).size(12.5).color(t.err))
            }
            _ => {}
        }
    }
    if aside.running() {
        answer = answer.push(
            row![
                icon(Icon::Dot, 10.0, t.accent),
                text(s.side_wait).size(12.5).color(t.accent)
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    let body = col.push(scrollable(answer).height(Length::Shrink).style(t.scroll()).direction(
        scrollable::Direction::Vertical(scrollable::Scrollbar::new().width(6).scroller_width(6).margin(1)),
    ));
    container(body).max_height(620).into()
}
