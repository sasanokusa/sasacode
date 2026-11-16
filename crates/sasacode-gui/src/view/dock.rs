//! Under the conversation: the approval that waits for an answer, and the input.

use iced::keyboard::{Key, key::Named};
use iced::widget::{
    Row, Space, button, column, container, pick_list, row, scrollable, text, text_editor, text_input, tooltip,
};
use iced::{Alignment, Length};
use sasacode_protocol::Decision;

use super::{BOLD, COL_W, El, MOD, MONO, clip_start, diff_view, plus_minus, room, short_path, tag, tip};
use crate::app::{App, EFFORTS, INPUT, Msg, PERMISSIONS, shortcut};
use crate::i18n::fill;
use crate::icons::{Icon, icon};
use crate::theme::RADIUS_L;
use crate::transcript::{Approval, Kind};

pub fn dock(app: &App) -> El<'_> {
    let mut col = column![].spacing(8);
    if let Some(approval) = app.transcript.pending_approval() {
        col = col.push(approval_card(app, approval));
    }
    col = col.push(composer(app)).push(hints(app));
    container(container(col).max_width(COL_W))
        .center_x(Length::Fill)
        .padding(iced::Padding {
            top: 4.0,
            right: 24.0,
            bottom: 10.0,
            left: 24.0,
        })
        .into()
}

/// The call that waits, shown whole, and the three answers.
fn approval_card<'a>(app: &'a App, approval: &'a Approval) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let tool = app.transcript.tool(&approval.call_id);
    let kind = tool.map(|tool| tool.kind()).unwrap_or(Kind::Other);
    let waiting = app.transcript.pending_approvals();

    let chord = if s.ja {
        format!("{MOD}↵で{}", s.h_allow)
    } else {
        format!("{MOD}↵ to {}", s.h_allow)
    };
    let mut head = row![
        icon(Icon::Alert, 15.0, t.warn),
        text(s.appr_title).size(13.5).font(BOLD).color(t.fg),
        tag(t, approval.tool.as_str(), t.inset, t.fg_2),
        Space::new().width(Length::Fill),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if waiting > 1 {
        head = head.push(text(fill(s.more_waiting, "n", waiting - 1)).size(12).color(t.warn));
    }
    head = head.push(text(chord).size(11.5).color(t.fg_3).wrapping(text::Wrapping::None));

    let arg = |key: &str| approval.args.get(key).and_then(|v| v.as_str());
    let mono = |body: String| {
        container(
            text(body)
                .size(12.5)
                .font(MONO)
                .color(t.fg)
                .wrapping(text::Wrapping::WordOrGlyph),
        )
        .width(Length::Fill)
        .padding([8, 10])
        .style(t.boxed(t.inset, t.inset, crate::theme::RADIUS))
    };
    // All of what is asked, however long: nothing is approved unseen.
    let body: El<'a> = match (kind, tool.and_then(|tool| tool.change.as_ref())) {
        (Kind::Bash, _) => mono(format!("$ {}", arg("command").unwrap_or_default())).into(),
        (Kind::Edit | Kind::Write, Some(change)) => {
            let mut what = column![
                row![
                    text(short_path(&change.path, &app.cwd))
                        .size(12.5)
                        .font(MONO)
                        .color(t.fg),
                    plus_minus(t, change.diff.added, change.diff.removed),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
                diff_view(t, s, &change.diff, usize::MAX),
            ]
            .spacing(6);
            if kind == Kind::Write {
                // The call's arguments are all there is to show: whether a file is there to be
                // replaced is not known before the call runs.
                what = what.push(text(s.appr_write).size(12).color(t.fg_3));
            }
            what.into()
        }
        _ => mono(serde_json::to_string_pretty(&approval.args).unwrap_or_default()).into(),
    };

    let always = match kind {
        Kind::Bash => s.always_cmd,
        Kind::Edit | Kind::Write => s.always_file,
        _ => s.always_call,
    };
    // Each on a line of its own when the card is narrow, never cut.
    let reason = row![
        text(s.appr_reason).size(12).color(t.fg_3),
        text(approval.reason.as_str()).size(12).font(MONO).color(t.fg_2),
        text(clip_start(&approval.cwd, 44)).size(12).font(MONO).color(t.fg_3),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .wrap();
    let label = |said: &'a str| text(said).size(13).wrapping(text::Wrapping::None);
    let buttons = row![
        button(label(s.allow))
            .padding([6, 16])
            .style(t.primary())
            .on_press(Msg::Answer(Decision::Allow)),
        button(label(always))
            .padding([6, 12])
            .style(t.outline(t.fg))
            .on_press(Msg::Answer(Decision::Always)),
        button(label(s.deny))
            .padding([6, 12])
            .style(t.outline(t.err))
            .on_press(Msg::Answer(Decision::Deny)),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .wrap();

    container(
        column![
            head,
            // Scrolls inside the card, so the answers stay in reach under a long call.
            container(
                scrollable(body)
                    .direction(scrollable::Direction::Vertical(
                        scrollable::Scrollbar::new().width(6).scroller_width(6).margin(1)
                    ))
                    .style(t.scroll())
            )
            .max_height(260),
            reason,
            buttons
        ]
        .spacing(10),
    )
    .width(Length::Fill)
    .padding(14)
    .style(t.floating(t.raised, t.warn_line))
    .into()
}

fn composer(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let waiting = app.transcript.pending_approval().is_some();
    let running = app.running();
    let empty = app.editor.text().trim().is_empty();
    let placeholder = if waiting {
        s.ph_waiting
    } else if running {
        s.ph_running
    } else if app.transcript.is_empty() {
        s.ph_new
    } else {
        s.ph_idle
    };

    let editor = text_editor(&app.editor)
        .id(INPUT)
        .placeholder(placeholder)
        .on_action(Msg::Edit)
        .size(14)
        .padding([4, 2])
        .min_height(24)
        .max_height(220)
        .style(t.editor())
        .key_binding(|press| {
            if !matches!(press.status, text_editor::Status::Focused { .. }) {
                return None;
            }
            // Enter sends and Shift+Enter breaks the line; the chords work from in here too.
            match press.key.as_ref() {
                Key::Named(Named::Enter) if press.modifiers.shift() => Some(text_editor::Binding::Enter),
                Key::Named(Named::Enter) if !press.modifiers.command() => Some(text_editor::Binding::Custom(Msg::Send)),
                _ => match shortcut(&press.key, press.modifiers) {
                    Some(msg) => Some(text_editor::Binding::Custom(msg)),
                    None => text_editor::Binding::from_key_press(press),
                },
            }
        });

    // Too narrow for one line of controls: the buttons get a line of their own.
    let tight = room(app) < 610.0;
    // What the next run starts with. A run that is going on keeps what it started with.
    // As wide as a long `provider/model`, less where the room is less: a width of its own inside a
    // box that takes the room the rest of the line leaves.
    // Marked like the two pickers next to it, so that it says what it is once a name is typed.
    let model = container(tip(
        t,
        row![
            icon(Icon::Bot, 13.0, t.fg_3),
            text_input(s.model_ph, &app.model)
                .on_input(Msg::Model)
                .size(12)
                .padding([4, 8])
                .width(260)
                .style(t.input()),
        ]
        .spacing(5)
        .align_y(Alignment::Center),
        format!("{}\n{}", s.m_model, s.next_run),
        tooltip::Position::Top,
    ))
    .width(Length::Fill);
    let mut modes = vec![s.default_tag];
    modes.extend(PERMISSIONS);
    let mode_hint = match app.permission {
        Some("edits") => s.mode_edits,
        Some("ask") => s.mode_ask,
        Some("agent") => s.mode_agent,
        Some("auto") => s.mode_auto,
        _ => s.next_run,
    };
    let mode = tip(
        t,
        row![
            icon(
                Icon::Shield,
                13.0,
                if app.permission == Some("auto") { t.err } else { t.fg_3 }
            ),
            pick_list(modes, app.permission, Msg::Permission)
                .placeholder(s.mode_ph)
                .text_size(12)
                .padding([4, 6])
                .style(t.pick(if app.permission == Some("auto") { t.err } else { t.fg_2 }))
                .menu_style(t.menu()),
        ]
        .spacing(2)
        .align_y(Alignment::Center),
        format!("{}：{mode_hint}", s.m_mode),
        // Under it: its list opens over it, where a hint would cover the choices.
        tooltip::Position::Bottom,
    );
    let mut efforts = vec![s.default_tag];
    efforts.extend(EFFORTS);
    let effort = tip(
        t,
        row![
            icon(Icon::Gauge, 13.0, t.fg_3),
            pick_list(efforts, app.effort, Msg::Effort)
                .placeholder(s.effort_ph)
                .text_size(12)
                .padding([4, 6])
                .style(t.pick(t.fg_2))
                .menu_style(t.menu()),
        ]
        .spacing(2)
        .align_y(Alignment::Center),
        format!("{}：{}", s.m_effort, s.next_run),
        tooltip::Position::Bottom,
    );

    let mut actions = row![].spacing(8).align_y(Alignment::Center);
    if running {
        actions = actions.push(
            button(
                row![
                    icon(Icon::Stop, 12.0, t.err),
                    text(s.stop).size(13).wrapping(text::Wrapping::None)
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .padding([6, 12])
            .style(t.outline(t.err))
            .on_press(Msg::Stop),
        );
    }
    let send = if waiting {
        s.deny_send
    } else if running {
        s.send_more
    } else {
        s.send
    };
    actions = actions.push(
        button(
            row![
                text(send).size(13).wrapping(text::Wrapping::None),
                icon(Icon::Arrow, 13.0, if empty { t.fg_3 } else { t.on_accent })
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding([6, 14])
        .style(t.primary())
        .on_press_maybe((!empty).then_some(Msg::Send)),
    );

    // The model's name takes the room the rest leaves: it is the one part that can be long.
    let controls = row![model, mode, effort]
        .spacing(6)
        .align_y(Alignment::Center)
        .width(Length::Fill);
    let mut col = column![editor].spacing(8);
    if tight {
        col = col.push(controls).push(container(actions).align_right(Length::Fill));
    } else {
        col = col.push(row![controls, actions].spacing(6).align_y(Alignment::Center));
    }
    container(col)
        .padding(iced::Padding {
            top: 10.0,
            right: 10.0,
            bottom: 8.0,
            left: 12.0,
        })
        .style(t.boxed(t.raised, t.line_2, RADIUS_L))
        .into()
}

/// What the keys do right now.
fn hints(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let key = |keys: String, what: &'static str| -> El<'_> {
        row![
            text(keys).size(11.5).font(MONO).color(t.fg_2),
            text(what).size(11.5).color(t.fg_3).wrapping(text::Wrapping::None)
        ]
        .spacing(4)
        .align_y(Alignment::Center)
        .into()
    };
    let (waiting, running) = (app.transcript.pending_approval().is_some(), app.running());
    let mut keys = Vec::new();
    if waiting {
        keys.push(key(format!("{MOD}↵"), s.h_allow));
        keys.push(key("↵".into(), s.deny_send));
    } else {
        keys.push(key("↵".into(), if running { s.send_more } else { s.h_send }));
        if running {
            keys.push(key(format!("{MOD}↵"), s.h_now));
        }
    }
    keys.push(key("⇧↵".into(), s.h_nl));
    if running {
        keys.push(key("Esc".into(), s.stop));
    }
    container(Row::with_children(keys).spacing(14).align_y(Alignment::Center).wrap())
        .padding([0, 6])
        .into()
}
