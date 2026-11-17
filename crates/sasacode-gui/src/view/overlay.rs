//! What is drawn over the window: the command palette, the list of keys, the settings, a name for
//! a conversation, and a side question with its answer. A click beside it puts it away, as Esc does.

use iced::widget::{
    Space, button, column, container, mouse_area, row, scrollable, text, text_editor, text_input, toggler,
};
use iced::{Alignment, Color, Length};

use super::timeline::reply;
use super::{El, MOD, bold, mono};
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
        Overlay::Automations => (super::automations::automations(app), 680.0, 50.0),
        Overlay::Jobs => (super::jobs::jobs(app), 720.0, 50.0),
        Overlay::ConfirmGist => (confirm_gist(app), 520.0, 120.0),
        Overlay::Quote => (quote(app), 720.0, 50.0),
        Overlay::Args => (args(app), 640.0, 60.0),
        Overlay::Extensions => (extensions(app), 640.0, 50.0),
        Overlay::About => (about(app), 680.0, 50.0),
        Overlay::Usage => (super::usage::usage(app), 820.0, 40.0),
        Overlay::Compare => (super::usage::compare(app), 560.0, 70.0),
        Overlay::Image(path) => (picture(app, path), (app.width - 80.0).clamp(360.0, 1200.0), 30.0),
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

pub(super) fn title<'a>(app: &App, label: &'a str) -> El<'a> {
    let t = app.t;
    row![
        text(label).size(15).font(bold()).color(t.fg),
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
        Pick::Worktrees => s.w().worktrees_ph,
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
    } else if pick == Pick::Worktrees && app.worktree_list.is_none() {
        list = list.push(container(text(s.w().worktrees_loading).size(13).color(t.fg_3)).padding([8, 10]));
    } else if found.is_empty() {
        list = list.push(container(text(s.nothing).size(13).color(t.fg_3)).padding([8, 10]));
    }
    for (i, entry) in found.into_iter().enumerate() {
        let mut line = row![text(entry.label).size(13).color(t.fg).width(Length::Fill)]
            .spacing(10)
            .align_y(Alignment::Center);
        if let Some(keys) = entry.keys {
            line = line.push(text(keys).size(11.5).font(mono()).color(t.fg_3));
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
            text(keys).size(12).font(mono()).color(t.fg_2).into(),
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
                .font(mono())
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
    let mut accents = row![choice(
        w.accent_own.to_owned(),
        p.more.accent.trim().is_empty(),
        Msg::SetAccent(String::new())
    )]
    .spacing(8);
    for ((name, ..), label) in
        crate::theme::ACCENTS
            .iter()
            .zip([w.accent_blue, w.accent_violet, w.accent_amber, w.accent_rose])
    {
        accents = accents.push(choice(
            label.to_owned(),
            p.more.accent.trim() == *name,
            Msg::SetAccent((*name).to_owned()),
        ));
    }
    let hex = if p.more.accent.starts_with('#') {
        p.more.accent.as_str()
    } else {
        ""
    };
    let accent = column![
        heading(w.set_accent),
        accents.wrap(),
        text_input(w.accent_ph, hex)
            .on_input(Msg::SetAccent)
            .size(13)
            .font(mono())
            .width(180)
            .padding([6, 10])
            .style(t.input()),
    ]
    .spacing(6);
    let fonts = column![
        heading(w.set_fonts),
        row![
            text_input(w.ui_font_ph, &p.more.ui_font)
                .on_input(Msg::SetUiFont)
                .size(13)
                .padding([6, 10])
                .style(t.input()),
            text_input(w.code_font_ph, &p.more.code_font)
                .on_input(Msg::SetCodeFont)
                .size(13)
                .padding([6, 10])
                .style(t.input()),
        ]
        .spacing(8),
        text(w.fonts_note).size(11.5).color(t.fg_3),
    ]
    .spacing(6);
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
                .font(mono())
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
            .font(mono())
            .color(t.fg_3),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(6);
    let mut idle = row![choice(
        w.archive_never.to_owned(),
        p.more.archive_after.is_none(),
        Msg::SetArchiveAfter(None)
    )]
    .spacing(8);
    for days in [7, 30, 90] {
        idle = idle.push(choice(
            fill(w.archive_days, "n", days),
            p.more.archive_after == Some(days),
            Msg::SetArchiveAfter(Some(days)),
        ));
    }
    let idle = column![heading(w.set_archive_after), idle].spacing(6);
    let follow = column![
        heading(w.set_follow),
        row![
            choice(w.follow_steer.to_owned(), !p.more.queue, Msg::SetQueue(false)),
            choice(w.follow_queue.to_owned(), p.more.queue, Msg::SetQueue(true)),
        ]
        .spacing(8)
        .wrap(),
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
    // Which ones, under the switch that sends them at all.
    let indent = |e: El<'static>| -> El<'static> { container(e).padding(iced::Padding::ZERO.left(28)).into() };
    // Nothing in their place while it is off, not even the gap between rows.
    let mut notices = column![switch(s.set_notify, p.notify, Msg::SetNotify)].spacing(10);
    if p.notify {
        notices = notices
            .push(indent(
                switch(w.set_notify_done, !p.more.quiet_done, |on| Msg::SetQuietDone(!on)).into(),
            ))
            .push(indent(
                switch(w.set_notify_wait, !p.more.quiet_wait, |on| Msg::SetQuietWait(!on)).into(),
            ))
            .push(indent(switch(w.set_sound, p.more.sound, Msg::SetSound).into()))
            .push(indent(
                switch(w.set_notify_always, p.more.notify_always, Msg::SetNotifyAlways).into(),
            ));
    }
    let mut editor = column![
        heading(w.set_editor),
        text_input(w.editor_ph, &p.more.editor)
            .on_input(Msg::SetEditor)
            .size(13)
            .font(mono())
            .padding([6, 10])
            .style(t.input()),
    ]
    .spacing(6);
    // The known editors on the PATH, one press to choose.
    if !app.editors.is_empty() {
        let mut found = row![text(w.editors_found).size(11.5).color(t.fg_3)]
            .spacing(6)
            .align_y(Alignment::Center);
        for (name, program) in &app.editors {
            let chosen = p.more.editor.split_whitespace().next() == Some(*program);
            found = found.push(choice(
                (*name).to_owned(),
                chosen,
                Msg::SetEditor((*program).to_owned()),
            ));
        }
        editor = editor.push(found.wrap());
    }
    let program = column![
        heading(w.set_program),
        text_input(w.program_ph, &p.more.program)
            .on_input(Msg::SetProgram)
            .size(13)
            .font(mono())
            .padding([6, 10])
            .style(t.input()),
    ]
    .spacing(6);
    let setup = app
        .prefs
        .more
        .folders
        .get(&app.folder_key())
        .map_or("", |f| f.setup.as_str());
    let setup = column![
        heading(w.set_setup),
        text_input(w.setup_ph, setup)
            .on_input(Msg::SetWorktreeSetup)
            .size(13)
            .font(mono())
            .padding([6, 10])
            .style(t.input()),
    ]
    .spacing(6);
    let body = column![
        title(app, s.settings),
        enter,
        follow,
        theme,
        accent,
        fonts,
        zoom,
        notices,
        switch(s.set_awake, p.awake, Msg::SetAwake),
        switch(s.set_steps, p.steps, Msg::SetSteps),
        switch(s.w().set_failed_only, p.more.failed_only, Msg::SetFailedOnly),
        switch(s.w().set_autosave, p.more.autosave, Msg::SetAutosave),
        switch(s.w().set_show_thinking, !p.more.hide_thinking, |on| {
            Msg::SetHideThinking(!on)
        }),
        switch(s.w().set_auto_archive, p.auto_archive, Msg::SetAutoArchive),
        switch(s.w().set_suggest, p.suggest, Msg::SetSuggest),
        switch(s.w().set_auto_title, p.more.auto_title, Msg::SetAutoTitle),
        idle,
        width,
        lang,
        editor,
        program,
        setup,
        scripts(app),
        allowed(app),
        text(s.set_kept).size(11.5).color(t.fg_3),
    ]
    .spacing(16);
    container(scrollable(body.padding(iced::Padding::ZERO.right(12))).style(t.scroll()))
        .max_height(640)
        .into()
}

/// What "always" allowed in the conversation shown, each of which can be taken back.
fn allowed(app: &App) -> El<'_> {
    let (t, w) = (app.t, app.s.w());
    let rules = app
        .chat
        .session
        .as_ref()
        .and_then(|id| app.prefs.more.allowed.get(id))
        .filter(|r| !r.is_empty());
    let mut list = column![text(w.allowed_head).size(13).font(super::bold()).color(t.fg)].spacing(4);
    match rules {
        None => list = list.push(text(w.allowed_none).size(12).color(t.fg_3)),
        Some(rules) => {
            for rule in rules {
                list = list.push(
                    row![
                        text(rule.as_str())
                            .size(12.5)
                            .font(mono())
                            .color(t.fg_2)
                            .width(Length::Fill)
                            .wrapping(text::Wrapping::WordOrGlyph),
                        button(crate::icons::icon(crate::icons::Icon::Close, 12.0, t.fg_3))
                            .padding(3)
                            .style(t.ghost(t.fg_3, false))
                            .on_press(Msg::ForgetAllowed(rule.clone())),
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center),
                );
            }
        }
    }
    list.into()
}

/// A reply's text as it was written, to choose part of and quote into the input.
fn quote(app: &App) -> El<'_> {
    let (t, w) = (app.t, app.s.w());
    let Some((_, content)) = &app.quoting else {
        return Space::new().into();
    };
    let chosen = content.selection().is_some_and(|c| !c.trim().is_empty());
    column![
        title(app, w.quote_title),
        text(w.quote_how).size(12).color(t.fg_3),
        iced::widget::text_editor(content)
            .id(crate::talk::QUOTE)
            .on_action(Msg::QuoteEdit)
            .font(mono())
            .size(13)
            .height(Length::Fixed(380.0))
            .padding(10)
            .style(t.editor()),
        row![
            Space::new().width(Length::Fill),
            button(text(w.cancel).size(13))
                .padding([6, 14])
                .style(t.outline(t.fg))
                .on_press(Msg::Show(None)),
            button(text(w.quote_take).size(13))
                .padding([6, 14])
                .style(t.primary())
                .on_press_maybe(chosen.then_some(Msg::QuoteTake)),
        ]
        .spacing(8),
    ]
    .spacing(10)
    .into()
}

/// A picture, larger: fitted to the window, or at its own size and scrolled.
fn picture<'a>(app: &'a App, path: &'a std::path::Path) -> El<'a> {
    let (t, w) = (app.t, app.s.w());
    let handle = iced::widget::image::Handle::from_path(path);
    let shown: El<'a> = if app.image_actual {
        scrollable(iced::widget::image(handle))
            .direction(scrollable::Direction::Both {
                vertical: scrollable::Scrollbar::default(),
                horizontal: scrollable::Scrollbar::default(),
            })
            .style(t.scroll())
            .width(Length::Fill)
            .height(Length::Fixed((app.height - 220.0).max(240.0)))
            .into()
    } else {
        iced::widget::image(handle)
            .content_fit(iced::ContentFit::Contain)
            .width(Length::Fill)
            .height(Length::Fixed((app.height - 220.0).max(240.0)))
            .into()
    };
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    column![
        title(app, name),
        container(shown).style(t.boxed(t.inset, t.line, crate::theme::RADIUS)),
        row![
            button(text(if app.image_actual { w.image_fit } else { w.image_actual }).size(13))
                .padding([6, 12])
                .style(t.outline(t.fg))
                .on_press(Msg::ImageActual(!app.image_actual)),
            button(text(app.s.reveal).size(13))
                .padding([6, 12])
                .style(t.outline(t.fg))
                .on_press(Msg::Reveal(path.to_owned())),
            button(text(w.image_open).size(13))
                .padding([6, 12])
                .style(t.outline(t.fg))
                .on_press(Msg::OpenPath(path.to_owned())),
            Space::new().width(Length::Fill),
            button(text(app.s.close).size(13))
                .padding([6, 14])
                .style(t.primary())
                .on_press(Msg::Show(None)),
        ]
        .spacing(8),
    ]
    .spacing(10)
    .into()
}

/// The plugins a run here loads, the skills it can use and the folder's MCP servers, as the CLI
/// lists them.
fn extensions(app: &App) -> El<'_> {
    let (t, w) = (app.t, app.s.w());
    let l = &app.listing;
    let heading = |label: &'static str| text(label).size(13).font(super::bold()).color(t.fg);
    let none = || text(w.ext_none).size(12).color(t.fg_3);
    let mut body = column![title(app, w.ext_title)].spacing(14);
    if l.plugins.is_none() && l.skills.is_none() && l.trust.is_none() {
        body = body.push(text(w.ext_old).size(12.5).color(t.fg_3));
    }
    let mut plugins = column![heading(w.ext_plugins)].spacing(4);
    match l.plugins.as_deref() {
        None | Some([]) => plugins = plugins.push(none()),
        Some(list) => {
            for p in list {
                let mut line = row![
                    text(p.name.as_str())
                        .size(12.5)
                        .font(mono())
                        .color(if p.enabled { t.fg } else { t.fg_3 }),
                    text(p.version.clone().unwrap_or_default()).size(11.5).color(t.fg_3),
                    text(p.source.as_str()).size(11.5).color(t.fg_3),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center);
                if !p.enabled {
                    line = line.push(text(w.ext_off).size(11.5).color(t.warn));
                }
                plugins = plugins.push(line);
                if let Some(error) = &p.error {
                    plugins = plugins.push(text(error.as_str()).size(11.5).color(t.err));
                }
            }
        }
    }
    let mut skills = column![heading(w.ext_skills)].spacing(4);
    match l.skills.as_deref() {
        None | Some([]) => skills = skills.push(none()),
        Some(list) => {
            for k in list {
                skills = skills.push(
                    column![
                        text(format!("/{}", k.name)).size(12.5).font(mono()).color(t.fg),
                        text(k.description.as_str())
                            .size(12)
                            .color(t.fg_2)
                            .wrapping(text::Wrapping::WordOrGlyph),
                    ]
                    .spacing(1),
                );
            }
        }
    }
    let mut mcp = column![heading(w.ext_mcp)].spacing(4);
    match l.trust.as_ref().map(|t| t.mcp_servers.as_slice()) {
        None | Some([]) => mcp = mcp.push(none()),
        Some(servers) => {
            for name in servers {
                mcp = mcp.push(text(name.as_str()).size(12.5).font(mono()).color(t.fg));
            }
        }
    }
    // The instructions every run here reads, each to open; one for the folder, to make.
    let mut instr = column![heading(w.instr_head)].spacing(4);
    let files = crate::about::instruction_files(app.home.as_deref(), &app.cwd);
    if files.is_empty() {
        instr = instr.push(text(w.instr_none).size(12).color(t.fg_3));
    }
    for file in &files {
        let shown = file
            .strip_prefix(&app.cwd)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| file.display().to_string());
        instr = instr.push(
            button(text(shown).size(12.5).font(mono()).color(t.accent))
                .padding([2, 0])
                .style(t.ghost(t.accent, false))
                .on_press(Msg::OpenInstructions(file.clone())),
        );
    }
    let mine = app.cwd.join("AGENTS.md");
    let mut make = row![].spacing(8);
    if !files.contains(&mine) {
        make = make.push(
            button(text(w.instr_make).size(12.5))
                .padding([4, 10])
                .style(t.outline(t.fg))
                .on_press(Msg::OpenInstructions(mine)),
        );
        // Written by the agent: the request put in the input, to read and send.
        make = make.push(
            button(text(w.instr_write).size(12.5))
                .padding([4, 10])
                .style(t.outline(t.fg))
                .on_press(Msg::WriteInstructions),
        );
    }
    if let Some(home) = &app.home
        && !files.contains(&home.join("AGENTS.md"))
    {
        make = make.push(
            button(text(w.instr_global).size(12.5))
                .padding([4, 10])
                .style(t.ghost(t.fg_2, false))
                .on_press(Msg::OpenInstructions(home.join("AGENTS.md"))),
        );
    }
    instr = instr.push(make);
    body = body.push(instr).push(plugins).push(skills).push(mcp).push(
        row![
            Space::new().width(Length::Fill),
            button(text(w.ext_reload).size(13))
                .padding([6, 14])
                .style(t.outline(t.fg))
                .on_press(Msg::Relist),
            button(text(app.s.close).size(13))
                .padding([6, 14])
                .style(t.primary())
                .on_press(Msg::Show(None)),
        ]
        .spacing(8),
    );
    container(scrollable(body.padding(iced::Padding::ZERO.right(12))).style(t.scroll()))
        .max_height(620)
        .into()
}

/// The versions, where things are kept, and what the runs wrote on stderr.
fn about(app: &App) -> El<'_> {
    let (t, w) = (app.t, app.s.w());
    let line = |label: &'static str, value: String| -> El<'_> {
        row![
            text(label).size(12.5).color(t.fg_3).width(150),
            text(value)
                .size(12.5)
                .font(mono())
                .color(t.fg)
                .width(Length::Fill)
                .wrapping(text::Wrapping::WordOrGlyph),
        ]
        .spacing(8)
        .into()
    };
    let cli = app.listing.version.clone().unwrap_or_else(|| w.about_old.to_owned());
    let home = app
        .home
        .as_ref()
        .map_or_else(|| w.about_unknown.to_owned(), |h| h.display().to_string());
    let info = column![
        line("sasacode-gui", crate::about::VERSION.to_owned()),
        line("sasacode", cli),
        line(w.about_program, app.program.display().to_string()),
        line(w.about_folder, app.cwd.display().to_string()),
        line(w.about_home, home),
        line(
            w.about_system,
            format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
        ),
    ]
    .spacing(4);
    // Asked on a click only: the check asks the release server.
    let check = button(
        text(if app.checking_update {
            w.about_update_checking
        } else {
            w.about_update_check
        })
        .size(12.5),
    )
    .padding([5, 12])
    .style(t.outline(t.fg))
    .on_press_maybe((!app.checking_update).then_some(Msg::CheckUpdate));
    let mut update = row![check].spacing(10).align_y(iced::Alignment::Center);
    match &app.update_said {
        Some(Ok(said)) => {
            update = update.push(
                column![
                    text(said.clone()).size(12.5).color(t.fg),
                    text(w.about_update_hint).size(11.5).color(t.fg_3),
                ]
                .spacing(2)
                .width(Length::Fill),
            );
        }
        Some(Err(why)) => {
            update = update.push(
                text(crate::i18n::fill(w.about_update_failed, "why", why))
                    .size(12.5)
                    .color(t.err)
                    .width(Length::Fill),
            );
        }
        None => {}
    }
    let info = column![info, update].spacing(10);
    let log: El<'_> = if app.log.is_empty() {
        text(w.about_log_none).size(12).color(t.fg_3).into()
    } else {
        let all: Vec<&str> = app.log.iter().map(String::as_str).collect();
        container(
            scrollable(
                text(all.join("\n"))
                    .size(11.5)
                    .font(mono())
                    .color(t.fg_2)
                    .wrapping(text::Wrapping::WordOrGlyph),
            )
            .anchor_bottom()
            .style(t.scroll()),
        )
        .max_height(260)
        .padding(8)
        .style(t.boxed(t.inset, t.line, 6.0))
        .into()
    };
    column![
        title(app, w.about_title),
        info,
        text(w.about_log).size(13).font(super::bold()).color(t.fg),
        log,
        row![
            button(text(w.about_open_home).size(13))
                .padding([6, 14])
                .style(t.ghost(t.fg_2, false))
                .on_press(Msg::OpenHome),
            Space::new().width(Length::Fill),
            button(text(w.about_copy).size(13))
                .padding([6, 14])
                .style(t.outline(t.fg))
                .on_press(Msg::Copy(crate::app::Clip::About)),
            button(text(app.s.close).size(13))
                .padding([6, 14])
                .style(t.primary())
                .on_press(Msg::Show(None)),
        ]
        .spacing(8),
    ]
    .spacing(12)
    .into()
}

/// The waiting call's arguments, as JSON to edit before it is allowed with them.
fn args(app: &App) -> El<'_> {
    let (t, w) = (app.t, app.s.w());
    let Some((_, content)) = &app.editing_args else {
        return Space::new().into();
    };
    column![
        title(app, w.args_title),
        text(w.args_how).size(12).color(t.fg_3),
        iced::widget::text_editor(content)
            .id(crate::plugins::ARGS)
            .on_action(Msg::ArgsEdit)
            .font(mono())
            .size(13)
            .height(Length::Fixed(320.0))
            .padding(10)
            .style(t.editor()),
        row![
            Space::new().width(Length::Fill),
            button(text(w.cancel).size(13))
                .padding([6, 14])
                .style(t.outline(t.fg))
                .on_press(Msg::Show(None)),
            button(text(w.args_send).size(13))
                .padding([6, 14])
                .style(t.primary())
                .on_press(Msg::ArgsSend),
        ]
        .spacing(8),
    ]
    .spacing(10)
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
                    .font(mono())
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
        row![
            text(base).size(12).color(t.fg_3),
            Space::new().width(Length::Fill),
            iced::widget::checkbox(pr.draft)
                .label(w.pr_as_draft)
                .on_toggle(Msg::PrAsDraft)
                .size(14)
                .text_size(12.5),
        ]
        .align_y(Alignment::Center),
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

/// What sharing the conversation as a secret gist does, asked each time.
fn confirm_gist(app: &App) -> El<'_> {
    let w = app.s.w();
    let size = app.gist().map_or(0, |g| g.markdown.chars().count());
    let body = format!("{}\n\n{}", w.gist_body, fill(w.gist_size, "n", size));
    ask(app, w.gist_title, body, w.gist_yes, Msg::GistCreate)
}

fn confirm_auto(app: &App) -> El<'_> {
    let w = app.s.w();
    ask(app, w.auto_title, w.auto_body.to_owned(), w.auto_yes, Msg::AutoAgree)
}

/// The side questions, and their answers as they come; another can follow, and all of it can be
/// put in the input for the conversation.
fn side(app: &App) -> El<'_> {
    let (t, s, w) = (app.t, app.s, app.s.w());
    let col = column![title(app, s.side_title)].spacing(12);
    let Some(aside) = &app.aside else {
        return col.into();
    };
    let mut answer = column![].spacing(10);
    for ex in &aside.transcript.exchanges {
        if ex.prompt.is_empty() {
            continue;
        }
        answer = answer.push(
            container(text(ex.prompt.as_str()).size(13.5).color(t.fg))
                .padding([8, 12])
                .style(t.boxed(t.inset, t.inset, RADIUS_L)),
        );
        for step in &ex.steps {
            match step {
                Step::Text(md) if !md.raw.trim().is_empty() => answer = answer.push(reply(app, md, false)),
                Step::Tool(tool) => {
                    answer = answer.push(
                        text(format!("{} {}", tool.name, tool.summary))
                            .size(12)
                            .font(mono())
                            .color(t.fg_3),
                    )
                }
                Step::Note(crate::transcript::Note::Error(body)) => {
                    answer = answer.push(text(body.as_str()).size(12.5).color(t.err))
                }
                _ => {}
            }
        }
    }
    let running = aside.running();
    if running {
        answer = answer.push(
            row![
                icon(Icon::Dot, 10.0, t.accent),
                text(s.side_wait).size(12.5).color(t.accent)
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    let ask = row![
        text_input(w.side_more, &app.side_typed)
            .id(SIDE_INPUT)
            .on_input(Msg::SideType)
            .on_submit(Msg::SideAsk)
            .size(13)
            .padding([7, 10])
            .style(t.input()),
        button(text(w.side_send).size(13))
            .padding([6, 14])
            .style(t.primary())
            .on_press_maybe((!running && !app.side_typed.trim().is_empty()).then_some(Msg::SideAsk)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let bring = row![
        Space::new().width(Length::Fill),
        button(text(w.side_bring).size(12.5))
            .padding([5, 12])
            .style(t.outline(t.fg))
            .on_press_maybe((!running).then_some(Msg::SideBring)),
    ];
    let body = col
        .push(
            scrollable(answer)
                .height(Length::Shrink)
                .style(t.scroll())
                .direction(scrollable::Direction::Vertical(
                    scrollable::Scrollbar::new().width(6).scroller_width(6).margin(1),
                ))
                .anchor_bottom(),
        )
        .push(ask)
        .push(bring);
    container(body).max_height(620).into()
}

/// Where the next side question is typed.
pub const SIDE_INPUT: &str = "side-input";
