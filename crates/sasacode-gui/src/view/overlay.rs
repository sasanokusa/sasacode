//! What is drawn over the window: the command palette, the list of keys, the settings, a name for
//! a conversation, and a side question with its answer. A click beside it puts it away, as Esc does.

use iced::widget::{
    Space, button, column, container, mouse_area, row, scrollable, text, text_editor, text_input, toggler,
};
use iced::{Alignment, Color, Length};

use super::timeline::reply;
use super::{BOLD, El, MOD, MONO};
use crate::app::{App, GROUP, Msg, Overlay, PALETTE, PR_TITLE, RENAME};
use crate::i18n::fill;
use crate::icons::{Icon, icon};
use crate::keys::Act;
use crate::layout::{Width, ZOOM_MAX, ZOOM_MIN};
use crate::overlay::Pick;
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
        Overlay::Palette { query, at, pick } => (palette(app, query, *at, *pick), 580.0, 70.0),
        Overlay::Keys => (keys(app), 560.0, 60.0),
        Overlay::Settings => (settings(app), 560.0, 60.0),
        Overlay::Rename { name, .. } => (rename(app, name), 460.0, 120.0),
        Overlay::Side => (side(app), 680.0, 50.0),
        Overlay::Group { name, .. } => (group(app, name), 460.0, 120.0),
        Overlay::Pr => (pr(app), 640.0, 60.0),
        Overlay::Quit(id) => (quit(app, *id), 460.0, 120.0),
        Overlay::ConfirmAuto => (confirm_auto(app), 480.0, 120.0),
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

fn palette<'a>(app: &'a App, query: &'a str, at: usize, pick: Pick) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let found = app.found_entries();
    let placeholder = match pick {
        Pick::Commands => s.palette_ph,
        Pick::Branches => s.w().branch_ph,
        Pick::Scripts => s.w().script_ph,
        Pick::Outline => s.w().outline_ph,
        Pick::Split => s.w().split_ph,
    };
    let field = text_input(placeholder, query)
        .id(PALETTE)
        .on_input(Msg::OverlayText)
        .on_submit(Msg::PaletteRun(at))
        .size(14)
        .padding([8, 10])
        .style(t.input());
    let mut list = column![].spacing(1);
    if pick == Pick::Branches && app.branch_list.is_none() {
        list = list.push(container(text(s.w().branch_loading).size(13).color(t.fg_3)).padding([8, 10]));
    } else if found.is_empty() {
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
    let w = s.w();
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
    let fixed: Vec<(String, String)> = vec![
        (send, s.h_send.to_owned()),
        (newline, s.h_nl.to_owned()),
        (super::dock::chord_keys(app), format!("{} / {}", s.h_allow, s.h_now)),
        ("Esc".into(), s.stop.to_owned()),
        ("↑ ↓".into(), s.k_history.to_owned()),
        ("@".into(), s.c_mention.to_owned()),
        ("/".into(), s.k_commands.to_owned()),
    ];
    let line = |keys: El<'static>, what: String, end: El<'static>| -> El<'static> {
        row![
            container(keys).width(110),
            text(what).size(13).color(t.fg).width(Length::Fill),
            end,
        ]
        .spacing(12)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .into()
    };
    let mut list = column![text(w.keys_fixed).size(12).color(t.fg_3)]
        .spacing(4)
        .width(Length::Fill)
        .padding(iced::Padding::ZERO.right(14));
    for (keys, what) in fixed {
        list = list.push(line(
            text(keys).size(12).font(MONO).color(t.fg_2).into(),
            what,
            Space::new().into(),
        ));
    }
    list = list.push(Space::new().height(8)).push(
        row![
            text(w.keys_yours).size(12).color(t.fg_3).width(Length::Fill),
            button(text(w.key_all_default).size(11.5))
                .padding([2, 8])
                .style(t.ghost(t.fg_2, false))
                .on_press_maybe((!app.prefs.keys.is_empty()).then_some(Msg::KeyReset(None))),
        ]
        .align_y(Alignment::Center),
    );
    let table = crate::keys::bound();
    for act in crate::keys::ACTS {
        let waiting = app.key_capture == Some(act);
        let shown = if waiting {
            w.key_waiting.to_owned()
        } else {
            table
                .chord(act)
                .map_or_else(|| w.key_none.to_owned(), crate::keys::Chord::show)
        };
        let changed = app.prefs.keys.contains_key(&act.name());
        let mut end = row![
            button(text(w.key_change).size(11.5))
                .padding([2, 8])
                .style(t.ghost(t.fg_2, waiting))
                .on_press(Msg::KeyCapture(act))
        ]
        .spacing(2);
        if changed {
            end = end.push(
                button(text(w.key_default).size(11.5))
                    .padding([2, 8])
                    .style(t.ghost(t.fg_3, false))
                    .on_press(Msg::KeyReset(Some(act))),
            );
        }
        list = list.push(line(
            text(shown)
                .size(12)
                .font(MONO)
                .color(if waiting {
                    t.accent
                } else if changed {
                    t.fg
                } else {
                    t.fg_2
                })
                .into(),
            act.label(&s).to_owned(),
            end.into(),
        ));
    }
    let mut body = column![title(app, s.keys_title)].spacing(12);
    if let Some(note) = &app.key_note {
        body = body.push(text(note.clone()).size(12.5).color(t.accent));
    }
    let body = body.push(
        scrollable(list)
            .width(Length::Fill)
            .height(Length::Shrink)
            .style(t.scroll()),
    );
    container(body).max_height(620).into()
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
    let w = s.w();
    let width = column![
        heading(w.set_width),
        row![
            choice(
                w.width_normal.to_owned(),
                p.more.chat_width == Width::Normal,
                Msg::SetWidth(Width::Normal)
            ),
            choice(
                w.width_wide.to_owned(),
                p.more.chat_width == Width::Wide,
                Msg::SetWidth(Width::Wide)
            ),
            choice(
                w.width_full.to_owned(),
                p.more.chat_width == Width::Full,
                Msg::SetWidth(Width::Full)
            ),
        ]
        .spacing(8),
    ]
    .spacing(6);
    let lang = column![
        heading(w.set_lang),
        row![
            choice(w.lang_system.to_owned(), p.more.lang.is_none(), Msg::SetLang(None)),
            choice(
                "日本語".to_owned(),
                p.more.lang.as_deref() == Some("ja"),
                Msg::SetLang(Some("ja"))
            ),
            choice(
                "English".to_owned(),
                p.more.lang.as_deref() == Some("en"),
                Msg::SetLang(Some("en"))
            ),
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
            text(
                [Act::ZoomIn, Act::ZoomOut, Act::ZoomReset]
                    .map(super::keys_for)
                    .join(" / ")
            )
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
    let body = column![
        title(app, s.settings),
        enter,
        theme,
        zoom,
        switch(s.set_notify, p.notify, Msg::SetNotify),
        switch(s.set_awake, p.awake, Msg::SetAwake),
        switch(s.set_steps, p.steps, Msg::SetSteps),
        switch(s.w().set_auto_archive, p.auto_archive, Msg::SetAutoArchive),
        switch(s.w().set_suggest, p.suggest, Msg::SetSuggest),
        width,
        lang,
        scripts(app),
        text(s.set_kept).size(11.5).color(t.fg_3),
    ]
    .spacing(16);
    container(scrollable(body.padding(iced::Padding::ZERO.right(12))).style(t.scroll()))
        .max_height(640)
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

/// The folder's scripts: each one's name and command, and the way to add one or take it out.
fn scripts(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let w = s.w();
    let mut col = column![
        text(w.set_scripts).size(12).color(t.fg_3),
        text(w.set_scripts_hint).size(11.5).color(t.fg_3),
    ]
    .spacing(6);
    for (i, sc) in app.scripts().iter().enumerate() {
        col = col.push(
            row![
                text_input(w.script_name_ph, &sc.name)
                    .on_input(move |v| Msg::ScriptName(i, v))
                    .size(12.5)
                    .padding([5, 8])
                    .width(130)
                    .style(t.input()),
                text_input(w.script_command_ph, &sc.command)
                    .on_input(move |v| Msg::ScriptCommand(i, v))
                    .font(MONO)
                    .size(12.5)
                    .padding([5, 8])
                    .style(t.input()),
                button(icon(Icon::Play, 12.0, t.fg_2))
                    .padding(5)
                    .style(t.ghost(t.fg_2, false))
                    .on_press_maybe((!sc.command.trim().is_empty()).then_some(Msg::RunScript(i))),
                button(icon(Icon::Close, 12.0, t.fg_3))
                    .padding(5)
                    .style(t.ghost(t.fg_3, false))
                    .on_press(Msg::ScriptRemove(i)),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
    }
    col.push(
        button(
            row![icon(Icon::Plus, 12.0, t.fg_2), text(w.script_add).size(12.5)]
                .spacing(6)
                .align_y(Alignment::Center),
        )
        .padding([4, 10])
        .style(t.outline(t.fg))
        .on_press(Msg::ScriptAdd),
    )
    .into()
}

/// The group of a conversation: typed, or one of those there are taken.
fn group<'a>(app: &'a App, name: &'a str) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let w = s.w();
    let mut there = row![].spacing(6);
    for g in app.group_names() {
        let on = g == name.trim();
        let b = button(text(g.clone()).size(12.5).wrapping(text::Wrapping::None))
            .padding([4, 10])
            .on_press(Msg::GroupPick(g));
        there = there.push(if on {
            b.style(t.primary())
        } else {
            b.style(t.outline(t.fg))
        });
    }
    column![
        title(app, w.group_title),
        text_input(w.group_ph, name)
            .id(GROUP)
            .on_input(Msg::OverlayText)
            .on_submit(Msg::GroupSave)
            .size(14)
            .padding([8, 10])
            .style(t.input()),
        there.wrap(),
        text(w.group_hint).size(11.5).color(t.fg_3),
        row![
            Space::new().width(Length::Fill),
            button(text(s.close).size(13))
                .padding([6, 12])
                .style(t.outline(t.fg))
                .on_press(Msg::Show(None)),
            button(text(s.save).size(13))
                .padding([6, 14])
                .style(t.primary())
                .on_press(Msg::GroupSave),
        ]
        .spacing(8),
    ]
    .spacing(12)
    .into()
}

/// The pull request written before it is made: its title and description, typed or written by
/// the model, or left to gh.
fn pr(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let w = s.w();
    let Some(pr) = &app.pr_draft else {
        return title(app, w.pr_title);
    };
    let drafting = app.drafting.contains(&crate::helper::Draft::Pr);
    let busy = drafting || app.git_busy;
    let base = fill(
        &fill(w.pr_base, "base", pr.context.base.trim_start_matches("origin/")),
        "n",
        pr.context.commits.len(),
    );
    column![
        title(app, w.pr_title),
        text(base).size(12).color(t.fg_3),
        text_input(w.pr_title_ph, &pr.title)
            .id(PR_TITLE)
            .on_input(Msg::PrTitle)
            .size(14)
            .padding([8, 10])
            .style(t.input()),
        container(
            text_editor(&pr.body)
                .placeholder(w.pr_body_ph)
                .on_action(Msg::PrBody)
                .size(13)
                .padding([6, 8])
                .height(260)
                .style(t.editor()),
        )
        .style(t.boxed(t.inset, t.line, crate::theme::RADIUS)),
        row![
            button(text(if drafting { w.drafting } else { w.pr_draft }).size(13))
                .padding([6, 12])
                .style(t.outline(t.fg))
                .on_press_maybe((!busy).then_some(Msg::Draft(crate::helper::Draft::Pr))),
            button(text(w.pr_fill).size(13))
                .padding([6, 12])
                .style(t.ghost(t.fg_2, false))
                .on_press_maybe((!busy).then_some(Msg::PrFill)),
            Space::new().width(Length::Fill),
            button(text(w.pr_create).size(13))
                .padding([6, 14])
                .style(t.primary())
                .on_press_maybe((!busy && !pr.title.trim().is_empty()).then_some(Msg::PrCreate)),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(12)
    .into()
}

/// Two buttons under a question: the one that goes on, and the one that does not.
fn ask<'a>(app: &'a App, title_text: &'static str, body: String, yes: &'static str, msg: Msg) -> El<'a> {
    let (t, w) = (app.t, app.s.w());
    column![
        title(app, title_text),
        text(body).size(13).color(t.fg_2),
        row![
            Space::new().width(Length::Fill),
            button(text(w.cancel).size(13))
                .padding([6, 12])
                .style(t.outline(t.fg))
                .on_press(Msg::Show(None)),
            button(text(yes).size(13))
                .padding([6, 14])
                .style(t.primary())
                .on_press(msg),
        ]
        .spacing(8),
    ]
    .spacing(12)
    .into()
}

fn quit(app: &App, id: iced::window::Id) -> El<'_> {
    let w = app.s.w();
    ask(
        app,
        w.quit_title,
        fill(w.quit_body, "n", app.runs_going()),
        w.quit_yes,
        Msg::Quit(id),
    )
}

fn confirm_auto(app: &App) -> El<'_> {
    let w = app.s.w();
    ask(app, w.auto_title, w.auto_body.to_owned(), w.auto_yes, Msg::AutoAgree)
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
