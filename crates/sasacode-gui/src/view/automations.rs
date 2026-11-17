//! The automations of the folder: each with when it runs next, and the form one is written in.

use iced::widget::{Space, button, column, container, row, scrollable, text, text_editor, text_input, tooltip};
use iced::{Alignment, Length};

use super::overlay::title;
use super::{El, bold, mono, tip};
use crate::app::{App, Msg};
use crate::automations::{AutoMsg, Automation, Every, Kind, MODES, show_at};
use crate::i18n::fill;
use crate::icons::{Icon, icon};

/// When `a` runs, in words.
fn when(app: &App, a: &Automation) -> String {
    let w = app.s.w();
    match a.every {
        Every::Hours { hours } => fill(w.am_every_hours, "n", hours),
        Every::Daily { at } => fill(w.am_daily, "at", show_at(at)),
        Every::Weekdays { at } => fill(w.am_weekdays, "at", show_at(at)),
    }
}

/// What is said of an automation, item by item.
fn about_row(app: &App, about: Vec<String>) -> El<'_> {
    let t = app.t;
    let mut line = row![].spacing(5);
    for (i, said) in about.into_iter().enumerate() {
        if i > 0 {
            line = line.push(text("·").size(11.5).color(t.fg_3));
        }
        line = line.push(text(said).size(11.5).color(t.fg_3));
    }
    line.into()
}

/// A time as the list says it.
fn stamp(t: chrono::NaiveDateTime) -> String {
    t.format("%m/%d %H:%M").to_string()
}

pub(crate) fn automations(app: &App) -> El<'_> {
    let (t, w) = (app.t, app.s.w());
    let now = chrono::Local::now().timestamp();
    let mut list = column![].spacing(6);
    let here = app.automations_here();
    if here.is_empty() {
        list = list.push(text(w.am_none).size(12).color(t.fg_3));
    }
    for (id, a) in here {
        let mut about = vec![when(app, a), a.mode().to_owned()];
        if a.on {
            about.push(fill(w.am_next, "when", stamp(a.next(now))));
        } else {
            about.push(w.am_off.to_owned());
        }
        if let Some(last) = a.last {
            about.push(fill(w.am_last, "when", stamp(crate::automations::local(last))));
        }
        let small = |i: Icon, said: &'static str, msg: Msg| {
            tip(
                t,
                button(icon(i, 13.0, t.fg_3))
                    .padding(5)
                    .style(t.ghost(t.fg_3, false))
                    .on_press(msg),
                said.to_owned(),
                tooltip::Position::Top,
            )
        };
        let toggle = tip(
            t,
            button(icon(
                if a.on { Icon::Check } else { Icon::Circle },
                13.0,
                if a.on { t.accent } else { t.fg_3 },
            ))
            .padding(5)
            .style(t.ghost(t.fg_3, false))
            .on_press(Msg::Auto(AutoMsg::Toggle(id.clone()))),
            if a.on { w.am_turn_off } else { w.am_turn_on }.to_owned(),
            tooltip::Position::Top,
        );
        list = list.push(
            container(
                row![
                    toggle,
                    column![
                        text(a.name.clone())
                            .size(13)
                            .font(bold())
                            .color(if a.on { t.fg } else { t.fg_3 }),
                        about_row(app, about),
                        text(a.prompt.lines().next().unwrap_or_default().to_owned())
                            .size(12)
                            .color(t.fg_2),
                    ]
                    .spacing(2)
                    .width(Length::Fill),
                    small(Icon::Play, w.am_run_now, Msg::Auto(AutoMsg::RunNow(id.clone()))),
                    small(Icon::Pencil, w.am_edit, Msg::Auto(AutoMsg::Edit(id.clone()))),
                    small(Icon::Trash, w.am_delete, Msg::Auto(AutoMsg::Delete(id.clone()))),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .padding([6, 8])
            .style(t.boxed(t.surface, t.line, crate::theme::RADIUS)),
        );
    }
    column![
        title(app, w.c_automations),
        text(w.am_how).size(12.5).color(t.fg_2),
        container(scrollable(list).style(t.scroll())).max_height(300),
        form(app),
    ]
    .spacing(12)
    .into()
}

/// The form an automation is written in: new, or one being changed.
fn form(app: &App) -> El<'_> {
    let (t, w) = (app.t, app.s.w());
    let f = &app.auto_form;
    let head = match f.editing.as_ref().and_then(|id| app.prefs.more.automations.get(id)) {
        Some(a) => fill(w.am_editing, "name", &a.name),
        None => w.am_new.to_owned(),
    };
    let chip = |label: &'static str, on: bool, msg: Msg| {
        button(text(label).size(12))
            .padding([4, 10])
            .style(t.ghost(if on { t.fg } else { t.fg_2 }, on))
            .on_press(msg)
    };
    let kinds = row![
        chip(
            w.am_kind_daily,
            f.kind == Kind::Daily,
            Msg::Auto(AutoMsg::Kind(Kind::Daily))
        ),
        chip(
            w.am_kind_weekdays,
            f.kind == Kind::Weekdays,
            Msg::Auto(AutoMsg::Kind(Kind::Weekdays))
        ),
        chip(
            w.am_kind_hours,
            f.kind == Kind::Hours,
            Msg::Auto(AutoMsg::Kind(Kind::Hours))
        ),
    ]
    .spacing(4);
    let time: El<'_> = if f.kind == Kind::Hours {
        row![
            text_input("3", &f.hours)
                .on_input(|h| Msg::Auto(AutoMsg::Hours(h)))
                .size(12.5)
                .font(mono())
                .width(56)
                .padding([4, 8])
                .style(t.input()),
            text(w.am_hours_unit).size(12).color(t.fg_2),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into()
    } else {
        text_input("09:00", &f.at)
            .on_input(|at| Msg::Auto(AutoMsg::At(at)))
            .on_submit(Msg::Auto(AutoMsg::Save))
            .size(12.5)
            .font(mono())
            .width(72)
            .padding([4, 8])
            .style(t.input())
            .into()
    };
    let mut modes = row![].spacing(4);
    for mode in MODES {
        modes = modes.push(chip(mode, f.mode == mode, Msg::Auto(AutoMsg::Mode(mode))));
    }
    let mut foot = row![text(w.am_mode_note).size(11.5).color(t.fg_3).width(Length::Fill)]
        .spacing(8)
        .align_y(Alignment::Center);
    if f.editing.is_some() {
        foot = foot.push(
            button(text(w.am_cancel).size(12.5))
                .padding([5, 12])
                .style(t.outline(t.fg))
                .on_press(Msg::Auto(AutoMsg::Clear)),
        );
    }
    foot = foot.push(
        button(text(w.am_save).size(12.5))
            .padding([5, 14])
            .style(t.primary())
            .on_press(Msg::Auto(AutoMsg::Save)),
    );
    let mut col = column![
        text(head).size(13).font(bold()).color(t.fg),
        text_input(w.am_name_ph, &f.name)
            .on_input(|n| Msg::Auto(AutoMsg::Name(n)))
            .size(13)
            .padding([5, 8])
            .style(t.input()),
        text_editor(&f.prompt)
            .placeholder(w.am_prompt_ph)
            .on_action(|a| Msg::Auto(AutoMsg::Prompt(a)))
            .size(13)
            .height(Length::Fixed(88.0))
            .padding(8)
            .style(t.editor()),
        row![kinds, Space::new().width(8), time].align_y(Alignment::Center),
        modes,
    ]
    .spacing(8);
    if let Some(wrong) = f.wrong {
        col = col.push(text(wrong).size(12).color(t.err));
    }
    col.push(foot).into()
}
