//! Under the conversation: the approval that waits for an answer, and the input.

use iced::keyboard::{Key, key::Named};
use iced::widget::{
    Row, Space, button, column, container, pick_list, row, scrollable, text, text_editor, text_input, tooltip,
};
use iced::{Alignment, Length};
use sasacode_protocol::Decision;

use super::{
    El, MOD, bold, clip, clip_start, col_w, diff_view, first_line, mono, plus_minus, room, short_path, tag, tip,
};
use crate::app::{App, EFFORTS, INPUT, MODEL, Msg, PERMISSIONS, shortcut};
use crate::i18n::fill;
use crate::icons::{Icon, icon};
use crate::input::{Offer, OfferKind};
use crate::theme::RADIUS_L;
use crate::transcript::{Approval, Ask, Kind};
use sasacode_protocol::event::UiAsk;

pub fn dock(app: &App) -> El<'_> {
    // Keyed, so that the input is the same widget whatever comes and goes above it: in a plain
    // column it would move down a place when the list after `/` or `@` opens, and lose the keys.
    let mut col = iced::widget::keyed::Column::new().spacing(8);
    if let Some(approval) = app.chat.transcript.pending_approval() {
        col = col.push(Above::Approval, approval_card(app, approval));
    }
    if let Some(ask) = app.chat.transcript.asks.first() {
        col = col.push(Above::Ask, ask_card(app, ask));
    }
    if let Some(trust) = app.listing.untrusted().filter(|_| !app.trust_once) {
        col = col.push(Above::Trust, trust_card(app, trust));
    }
    if let Some(offer) = app.offer() {
        col = col.push(Above::Offer, offered(app, offer));
    }
    if app.dropping {
        col = col.push(
            Above::Drop,
            container(
                row![
                    icon(Icon::File, 14.0, t_accent(app)),
                    text(app.s.drop_hint).size(12.5).color(t_accent(app))
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([8, 12])
            .style(app.t.boxed(app.t.accent_wash, app.t.accent, crate::theme::RADIUS)),
        );
    }
    if !app.chat.comments.is_empty() {
        col = col.push(Above::Comments, comments(app));
    }
    let images = crate::composer::image_refs(&app.chat.editor.text(), &app.cwd);
    if !images.is_empty() {
        col = col.push(Above::Images, attached(app, images));
    }
    if let Some(next) = suggested(app) {
        col = col.push(Above::Suggestion, next);
    }
    if let Some(moved) = branch_moved(app) {
        col = col.push(Above::Branch, moved);
    }
    if let Some(cut) = cut_short(app) {
        col = col.push(Above::Cut, cut);
    }
    if !app.chat.follow.is_empty() {
        col = col.push(Above::Follow, follow_ups(app));
    }
    if let Some(planned) = planned(app) {
        col = col.push(Above::Plan, planned);
    }
    col = col.push(Above::Input, composer(app)).push(Above::Hints, hints(app));
    container(container(col).max_width(col_w(app)))
        .center_x(Length::Fill)
        .padding(iced::Padding {
            top: 4.0,
            right: 24.0,
            bottom: 10.0,
            left: 24.0,
        })
        .into()
}

/// What the dock holds, from the top.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Above {
    Approval,
    Ask,
    Trust,
    Offer,
    Drop,
    Comments,
    Images,
    Suggestion,
    Branch,
    Cut,
    Follow,
    Plan,
    Input,
    Hints,
}

fn t_accent(app: &App) -> iced::Color {
    app.t.accent
}

/// What the user may say next, as the model guessed: put in the input by a click.
fn suggested(app: &App) -> Option<El<'_>> {
    let (t, w) = (app.t, app.s.w());
    let (key, next) = app.suggestion.as_ref()?;
    if *key != app.chat.key || app.busy() || !app.chat.editor.text().trim().is_empty() {
        return None;
    }
    let chip = tip(
        t,
        button(
            row![
                text(w.suggest_label).size(11.5).color(t.accent),
                text(next.as_str()).size(13).color(t.fg_2)
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([5, 10])
        .style(t.outline(t.line_2))
        .on_press(Msg::TakeSuggestion),
        w.suggest_take.to_owned(),
        tooltip::Position::Top,
    );
    Some(
        row![
            chip,
            button(icon(Icon::Close, 11.0, t.fg_3))
                .padding(4)
                .style(t.ghost(t.fg_3, false))
                .on_press(Msg::DropSuggestion),
        ]
        .spacing(4)
        .align_y(Alignment::Center)
        .into(),
    )
}

/// A plan run's plan, written: gone ahead with in the mode chosen before plan mode.
fn planned(app: &App) -> Option<El<'_>> {
    let (t, w) = (app.t, app.s.w());
    if !app.chat.planned || app.busy() || app.permission != Some(crate::plan::MODE) {
        return None;
    }
    let mode = app.before_plan.unwrap_or(app.s.default_tag);
    Some(
        container(
            row![
                icon(Icon::List, 13.0, t.accent),
                text(w.plan_ready).size(12.5).color(t.fg).width(Length::Fill),
                tip(
                    t,
                    button(text(w.plan_go).size(12))
                        .padding([4, 12])
                        .style(t.primary())
                        .on_press(Msg::PlanGo),
                    fill(w.plan_go_mode, "mode", mode),
                    tooltip::Position::Top,
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([6, 10])
        .style(t.boxed(t.accent_wash, t.accent, crate::theme::RADIUS))
        .into(),
    )
}

/// The conversation's run was stopped partway by closing the window, and nothing runs in it now.
fn cut_short(app: &App) -> Option<El<'_>> {
    let (t, w) = (app.t, app.s.w());
    let id = app.chat.session.clone()?;
    if !app.prefs.more.cut.contains(&id) || app.chat.busy() {
        return None;
    }
    Some(
        container(
            row![
                icon(Icon::Play, 13.0, t.warn),
                text(w.cut_short).size(12.5).color(t.fg).width(Length::Fill),
                button(text(w.cut_go_on).size(12))
                    .padding([3, 10])
                    .style(t.outline(t.fg))
                    .on_press(Msg::GoOn(id.clone())),
                button(icon(Icon::Close, 11.0, t.fg_3))
                    .padding(4)
                    .style(t.ghost(t.fg_3, false))
                    .on_press(Msg::CutSeen(id)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([6, 10])
        .style(t.boxed(t.warn_wash, t.warn_line, crate::theme::RADIUS))
        .into(),
    )
}

/// The branch the conversation was started on, when another is checked out now.
fn branch_moved(app: &App) -> Option<El<'_>> {
    let (t, w) = (app.t, app.s.w());
    let (was, now) = app.branch_moved()?;
    let id = app.chat.session.clone()?;
    Some(
        container(
            row![
                icon(Icon::Branch, 13.0, t.warn),
                text(fill(fill(w.branch_moved, "was", was).as_str(), "now", now))
                    .size(12.5)
                    .color(t.fg)
                    .width(Length::Fill),
                button(text(fill(w.branch_back, "branch", was)).size(12))
                    .padding([3, 10])
                    .style(t.outline(t.fg))
                    .on_press_maybe((!app.git_busy).then(|| Msg::BackToBranch(was.to_owned()))),
                button(icon(Icon::Close, 11.0, t.fg_3))
                    .padding(4)
                    .style(t.ghost(t.fg_3, false))
                    .on_press(Msg::BranchSeen(id)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([6, 10])
        .style(t.boxed(t.warn_wash, t.warn_line, crate::theme::RADIUS))
        .into(),
    )
}

/// The messages that wait for the run to end, each for a run of its own: taken back, sent to the
/// run now, moved up, or dropped.
fn follow_ups(app: &App) -> El<'_> {
    let (t, w) = (app.t, app.s.w());
    let mut list = column![text(w.follow_head).size(11.5).color(t.fg_3)].spacing(4);
    for (i, said) in app.chat.follow.iter().enumerate() {
        let small = |label: &'static str, msg: Msg| {
            button(text(label).size(11.5).color(t.fg_2).wrapping(text::Wrapping::None))
                .padding([2, 8])
                .style(t.ghost(t.fg_2, false))
                .on_press(msg)
        };
        let mut line = row![
            text(clip(first_line(said), 70))
                .size(12.5)
                .color(t.fg)
                .wrapping(text::Wrapping::None)
                .width(Length::Fill)
        ]
        .spacing(2)
        .align_y(Alignment::Center);
        if i > 0 {
            line = line.push(
                button(icon(Icon::ChevronUp, 11.0, t.fg_3))
                    .padding(4)
                    .style(t.ghost(t.fg_3, false))
                    .on_press(Msg::FollowUp(i)),
            );
        }
        line = line
            .push(small(w.follow_now, Msg::FollowNow(i)))
            .push(small(w.follow_edit, Msg::FollowEdit(i)))
            .push(
                button(icon(Icon::Close, 11.0, t.fg_3))
                    .padding(4)
                    .style(t.ghost(t.fg_3, false))
                    .on_press(Msg::FollowDrop(i)),
            );
        list = list.push(line);
    }
    container(list)
        .width(Length::Fill)
        .padding([6, 10])
        .style(t.boxed(t.raised, t.line, crate::theme::RADIUS))
        .into()
}

/// The images the input names, each small, with the way to take it out of the input.
fn attached(app: &App, images: Vec<(std::path::PathBuf, String)>) -> El<'_> {
    let t = app.t;
    let mut line = row![].spacing(8);
    for (path, written) in images {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let picture = iced::widget::image(iced::widget::image::Handle::from_path(&path))
            .width(56)
            .height(56)
            .content_fit(iced::ContentFit::Cover);
        line = line.push(super::tip(
            t,
            iced::widget::stack![
                button(container(picture).style(t.boxed(t.raised, t.line, crate::theme::RADIUS)))
                    .padding(0)
                    .style(t.ghost(t.fg, false))
                    .on_press(Msg::ViewImage(path)),
                container(
                    button(icon(Icon::Close, 9.0, t.fg))
                        .padding(2)
                        .style(t.outline(t.fg))
                        .on_press(Msg::Unattach(written))
                )
                .align_right(56)
                .padding(2),
            ],
            name,
            iced::widget::tooltip::Position::Top,
        ));
    }
    line.wrap().into()
}

/// The comments on a diff that go with the next message, each where it was made and with the way
/// to take it back.
fn comments(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let mut list = column![text(s.w().comments_head).size(11.5).color(t.fg_3)].spacing(3);
    for (i, c) in app.chat.comments.iter().enumerate() {
        let place = crate::review::place(&c.path, c.line, &app.cwd);
        list = list.push(
            row![
                text(clip_start(&place, 36)).size(12).font(mono()).color(t.fg_2),
                text(c.text.as_str())
                    .size(12.5)
                    .color(t.fg)
                    .width(Length::Fill)
                    .wrapping(text::Wrapping::WordOrGlyph),
                button(icon(Icon::Close, 10.0, t.fg_3))
                    .padding(2)
                    .style(t.ghost(t.fg_3, false))
                    .on_press(Msg::CommentRemove(i)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    container(list)
        .width(Length::Fill)
        .padding([6, 12])
        .style(app.t.boxed(app.t.accent_wash, app.t.line, crate::theme::RADIUS))
        .into()
}

/// What the input offers where its cursor is: files after `@`, commands after `/`. ↑ and ↓ choose,
/// Tab or Enter takes, Esc puts it away; a click takes one too.
fn offered(app: &App, offer: Offer) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let mut list = column![].spacing(1);
    let pick = app.pick.min(offer.len().saturating_sub(1));
    match offer.kind {
        OfferKind::Files(files) => {
            if files.is_empty() {
                // Nothing to say before the list has come.
                if app.files.is_empty() {
                    return Space::new().into();
                }
                list = list.push(container(text(s.no_files).size(12.5).color(t.fg_3)).padding([5, 10]));
            }
            for (i, file) in files.into_iter().enumerate() {
                let folder = file.ends_with('/');
                list = list.push(
                    button(
                        row![
                            icon(if folder { Icon::Folder } else { Icon::File }, 13.0, t.fg_3),
                            text(file)
                                .size(12.5)
                                .font(mono())
                                .color(t.fg)
                                .wrapping(text::Wrapping::None),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                    )
                    .width(Length::Fill)
                    .padding([4, 10])
                    .style(t.ghost(t.fg, i == pick))
                    .on_press(Msg::Complete(Some(i))),
                );
            }
        }
        OfferKind::Commands(commands, plugins) => {
            let first = commands.len();
            // After the window's own, in the order picked with ↑ and ↓.
            let mut later: Vec<El<'_>> = Vec::new();
            for (i, plugin) in plugins.into_iter().enumerate() {
                let what = match &plugin.argument_hint {
                    Some(hint) => format!("{hint}  {}", plugin.description),
                    None => plugin.description.clone(),
                };
                let from = if plugin.owner == "skills" {
                    app.s.w().slash_skill.to_owned()
                } else {
                    plugin.owner.clone()
                };
                later.push(
                    button(
                        row![
                            text(format!("/{}", plugin.name))
                                .size(12.5)
                                .font(mono())
                                .color(t.fg)
                                .width(110)
                                .wrapping(text::Wrapping::None),
                            text(clip(&what, 70))
                                .size(12.5)
                                .color(t.fg_2)
                                .width(Length::Fill)
                                .wrapping(text::Wrapping::None),
                            text(from).size(11).color(t.fg_3).wrapping(text::Wrapping::None),
                        ]
                        .spacing(10)
                        .align_y(Alignment::Center),
                    )
                    .width(Length::Fill)
                    .padding([4, 10])
                    .style(t.ghost(t.fg, first + i == pick))
                    .on_press(Msg::Complete(Some(first + i)))
                    .into(),
                );
            }
            for (i, (name, command)) in commands.into_iter().enumerate() {
                list = list.push(
                    button(
                        row![
                            text(format!("/{name}")).size(12.5).font(mono()).color(t.fg).width(110),
                            text(command.what(&s)).size(12.5).color(t.fg_2),
                        ]
                        .spacing(10)
                        .align_y(Alignment::Center),
                    )
                    .width(Length::Fill)
                    .padding([4, 10])
                    .style(t.ghost(t.fg, i == pick))
                    .on_press(Msg::Complete(Some(i))),
                );
            }
            for el in later {
                list = list.push(el);
            }
        }
    }
    let keys = text(if s.ja {
        "↑↓で選ぶ · Tabか↵で入れる · Escで閉じる"
    } else {
        "↑↓ choose · Tab or ↵ take · Esc close"
    })
    .size(11)
    .color(t.fg_3);
    container(column![list, container(keys).padding([2, 10])].spacing(4))
        .width(Length::Fill)
        .padding(6)
        .style(t.floating(t.raised, t.line_2))
        .into()
}

/// The call that waits, shown whole, and the three answers.
/// A plugin's question, answered here: yes or no, one of its options, several, or a text. The run
/// waits for it.
fn ask_card<'a>(app: &'a App, ask: &'a Ask) -> El<'a> {
    let (t, w) = (app.t, app.s.w());
    let id = ask.id.clone();
    let mut head = row![
        icon(Icon::Bot, 15.0, t.accent),
        text(w.ask_head).size(12).color(t.fg_3),
        Space::new().width(Length::Fill),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let more = app.chat.transcript.asks.len() - 1;
    if more > 0 {
        head = head.push(text(fill(w.ask_more, "n", more)).size(12).color(t.fg_3));
    }
    head = head.push(tip(
        t,
        button(icon(Icon::Close, 13.0, t.fg_3))
            .padding(4)
            .style(t.ghost(t.fg_3, false))
            .on_press(Msg::AskAnswer(id.clone(), serde_json::Value::Null)),
        w.ask_dismiss.to_owned(),
        tooltip::Position::Top,
    ));
    let mut body = column![
        head,
        text(ask.title.as_str())
            .size(14)
            .font(bold())
            .color(t.fg)
            .wrapping(text::Wrapping::WordOrGlyph)
    ]
    .spacing(8);
    let said = |message: &'a Option<String>| {
        message
            .as_deref()
            .filter(|m| !m.trim().is_empty())
            .map(|m| text(m).size(13).color(t.fg_2).wrapping(text::Wrapping::WordOrGlyph))
    };
    let label = |said: &'a str| text(said).size(13).wrapping(text::Wrapping::None);
    let option = |o: &'a sasacode_protocol::event::UiOption, on: bool| -> El<'a> {
        let mut what = column![text(o.label.as_str()).size(13).color(t.fg)].spacing(2);
        if let Some(d) = o.description.as_deref().filter(|d| !d.is_empty()) {
            what = what.push(text(d).size(12).color(t.fg_3).wrapping(text::Wrapping::WordOrGlyph));
        }
        row![
            icon(
                if on { Icon::Check } else { Icon::Dot },
                13.0,
                if on { t.accent } else { t.fg_3 }
            ),
            what
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
    };
    match &ask.ask {
        UiAsk::Confirm { message } => {
            if let Some(m) = said(message) {
                body = body.push(m);
            }
            body = body.push(
                row![
                    button(label(w.ask_yes))
                        .padding([6, 16])
                        .style(t.primary())
                        .on_press(Msg::AskAnswer(id.clone(), serde_json::Value::Bool(true))),
                    button(label(w.ask_no))
                        .padding([6, 12])
                        .style(t.outline(t.fg))
                        .on_press(Msg::AskAnswer(id.clone(), serde_json::Value::Bool(false))),
                ]
                .spacing(8),
            );
        }
        UiAsk::Select { options } => {
            let mut list = column![].spacing(2);
            for o in options {
                list = list.push(
                    button(option(o, false))
                        .width(Length::Fill)
                        .padding([6, 10])
                        .style(t.ghost(t.fg, false))
                        .on_press(Msg::AskAnswer(id.clone(), serde_json::Value::String(o.value.clone()))),
                );
            }
            body = body.push(container(scrollable(list).style(t.scroll())).max_height(240));
        }
        UiAsk::SelectMany { options, .. } => {
            let mut list = column![].spacing(2);
            for o in options {
                list = list.push(
                    button(option(o, ask.chosen.contains(&o.value)))
                        .width(Length::Fill)
                        .padding([6, 10])
                        .style(t.ghost(t.fg, false))
                        .on_press(Msg::AskToggle(id.clone(), o.value.clone())),
                );
            }
            body = body.push(container(scrollable(list).style(t.scroll())).max_height(240));
            body = body.push(
                button(label(w.ask_ok))
                    .padding([6, 16])
                    .style(t.primary())
                    .on_press(Msg::AskSubmit(id.clone())),
            );
        }
        UiAsk::Input {
            message, placeholder, ..
        } => {
            if let Some(m) = said(message) {
                body = body.push(m);
            }
            let typed = id.clone();
            body = body.push(
                row![
                    text_input(placeholder.as_deref().unwrap_or(""), &ask.typed)
                        .id(ASK)
                        .on_input(move |v| Msg::AskType(typed.clone(), v))
                        .on_submit(Msg::AskSubmit(id.clone()))
                        .size(13)
                        .padding([7, 10])
                        .style(t.input()),
                    button(label(w.ask_ok))
                        .padding([6, 16])
                        .style(t.primary())
                        .on_press(Msg::AskSubmit(id.clone())),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            );
        }
    }
    container(body)
        .width(Length::Fill)
        .padding(14)
        .style(t.floating(t.raised, t.accent))
        .into()
}

/// The folder's config wants what is used only once it is trusted: what, and the two ways to.
fn trust_card<'a>(app: &'a App, trust: &'a crate::cli::Trust) -> El<'a> {
    let (t, w) = (app.t, app.s.w());
    let mut body = column![
        row![
            icon(Icon::Shield, 15.0, t.warn),
            text(w.trust_head).size(13).font(bold()).color(t.fg)
        ]
        .spacing(8)
        .align_y(Alignment::Center)
    ]
    .spacing(6);
    for item in &trust.items {
        body = body.push(text(format!("・{item}")).size(12.5).color(t.fg_2));
    }
    let mut buttons = row![].spacing(8).align_y(Alignment::Center);
    if trust.complete {
        buttons = buttons.push(
            button(text(w.trust_save).size(13))
                .padding([6, 16])
                .style(t.primary())
                .on_press(Msg::TrustFolder),
        );
    } else {
        body = body.push(text(w.trust_partial).size(12).color(t.fg_3));
    }
    buttons = buttons.push(
        button(text(w.trust_once).size(13))
            .padding([6, 12])
            .style(t.outline(t.fg))
            .on_press(Msg::TrustOnce),
    );
    container(body.push(buttons.wrap()))
        .width(Length::Fill)
        .padding(14)
        .style(t.floating(t.raised, t.warn_line))
        .into()
}

/// The text a plugin's question is answered with.
pub const ASK: &str = "ask";

fn approval_card<'a>(app: &'a App, approval: &'a Approval) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let tool = app.chat.transcript.tool(&approval.call_id);
    let kind = tool.map(|tool| tool.kind()).unwrap_or(Kind::Other);
    let waiting = app.chat.transcript.pending_approvals();

    let chord = if s.ja {
        format!("{}で{}", chord_keys(app), s.h_allow)
    } else {
        format!("{} to {}", chord_keys(app), s.h_allow)
    };
    let mut head = row![
        icon(Icon::Alert, 15.0, t.warn),
        text(s.appr_title).size(13.5).font(bold()).color(t.fg),
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
    let code_block = |body: String| {
        container(
            text(body)
                .size(12.5)
                .font(mono())
                .color(t.fg)
                .wrapping(text::Wrapping::WordOrGlyph),
        )
        .width(Length::Fill)
        .padding([8, 10])
        .style(t.boxed(t.inset, t.inset, crate::theme::RADIUS))
    };
    // All of what is asked, however long: nothing is approved unseen.
    let body: El<'a> = match (kind, tool.and_then(|tool| tool.change.as_ref())) {
        (Kind::Bash, _) => code_block(format!("$ {}", arg("command").unwrap_or_default())).into(),
        (Kind::Edit | Kind::Write, Some(change)) => {
            let mut what = column![
                row![
                    text(short_path(&change.path, &app.cwd))
                        .size(12.5)
                        .font(mono())
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
        _ => code_block(serde_json::to_string_pretty(&approval.args).unwrap_or_default()).into(),
    };

    let always = match kind {
        Kind::Bash => s.always_cmd,
        Kind::Edit | Kind::Write => s.always_file,
        _ => s.always_call,
    };
    // Each on a line of its own when the card is narrow, never cut.
    let reason = row![
        text(s.appr_reason).size(12).color(t.fg_3),
        text(approval.reason.as_str()).size(12).font(mono()).color(t.fg_2),
        text(clip_start(&approval.cwd, 44)).size(12).font(mono()).color(t.fg_3),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .wrap();
    let label = |said: &'a str| text(said).size(13).wrapping(text::Wrapping::None);
    let mut buttons = row![
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
    .align_y(Alignment::Center);
    if app.chat.newer {
        buttons = buttons.push(
            button(label(s.w().args_edit))
                .padding([6, 12])
                .style(t.ghost(t.fg_2, false))
                .on_press(Msg::ArgsOpen),
        );
    }
    let buttons = buttons.wrap();

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
    let waiting = app.chat.transcript.pending_approval().is_some();
    let running = app.running();
    // Comments alone are a message too.
    let empty = app.chat.editor.text().trim().is_empty() && app.chat.comments.is_empty();
    let placeholder = if waiting {
        s.ph_waiting
    } else if running {
        s.ph_running
    } else if app.chat.transcript.is_empty() {
        s.ph_new
    } else {
        s.ph_idle
    };

    // What the keys do in the input depends on what it offers, on what was sent before, and on
    // what Enter was set to do.
    let offer = app.offer();
    let has_offer = offer.is_some();
    let offering = offer.is_some_and(|o| o.len() > 0);
    let cursor = app.chat.editor.cursor().position;
    let last_line = app.chat.editor.line_count().saturating_sub(1);
    let recall_up = (app.recall.is_some() || app.chat.editor.text().is_empty()) && cursor.line == 0;
    let recall_down = app.recall.is_some() && cursor.line >= last_line;
    let enter_sends = app.prefs.enter_sends;
    let editor = text_editor(&app.chat.editor)
        .id(INPUT)
        .placeholder(placeholder)
        .on_action(Msg::Edit)
        .size(14)
        .padding([4, 2])
        .min_height(24)
        .max_height(220)
        .style(t.editor())
        .key_binding(move |press| {
            use text_editor::Binding;
            if !matches!(press.status, text_editor::Status::Focused { .. }) {
                return None;
            }
            let (m, plain) = (press.modifiers, press.modifiers.is_empty());
            match press.key.as_ref() {
                // What is offered is chosen and taken with the keys, before anything else they do.
                Key::Named(Named::ArrowUp) if offering && plain => Some(Binding::Custom(Msg::Pick(-1))),
                Key::Named(Named::ArrowDown) if offering && plain => Some(Binding::Custom(Msg::Pick(1))),
                Key::Named(Named::Tab) if offering && plain => Some(Binding::Custom(Msg::Complete(None))),
                Key::Named(Named::Enter) if offering && plain => Some(Binding::Custom(Msg::Complete(None))),
                Key::Named(Named::Escape) if has_offer => Some(Binding::Custom(Msg::Dismiss)),
                // What was sent before, from the first line of an empty input, and back.
                Key::Named(Named::ArrowUp) if recall_up && plain => Some(Binding::Custom(Msg::Recall(-1))),
                Key::Named(Named::ArrowDown) if recall_down && plain => Some(Binding::Custom(Msg::Recall(1))),
                // Enter sends and Shift+Enter breaks the line; or, as set, Enter breaks it and the
                // chord sends, the chord's own work moving to the chord with Shift. The chords
                // never get here: the window takes them first (`chords`, `Msg::CommandEnter`).
                Key::Named(Named::Enter) if enter_sends && m.shift() && !m.command() => Some(Binding::Enter),
                Key::Named(Named::Enter) if enter_sends && !m.command() => Some(Binding::Custom(Msg::Send)),
                Key::Named(Named::Enter) if !enter_sends && !m.command() => Some(Binding::Enter),
                // A paste with no text on the clipboard takes its image.
                Key::Character("v") if m.command() && !m.shift() && !m.alt() => Some(Binding::Custom(Msg::Paste)),
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
                .id(MODEL)
                .on_input(Msg::Model)
                .size(12)
                .padding([4, 8])
                .width(260)
                .style(t.input()),
            iced::widget::mouse_area(
                button(icon(Icon::ChevronDown, 12.0, t.fg_3))
                    .padding(3)
                    .style(t.ghost(t.fg_3, false))
                    .on_press_maybe(
                        app.listing
                            .models
                            .as_ref()
                            .filter(|m| !m.is_empty())
                            .map(|_| Msg::OpenMenu(crate::rows::MenuFor::Models)),
                    ),
            ),
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
        Some(crate::plan::MODE) => s.w().mode_plan,
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
/// The chord's keys as Enter was set: ⌘↵, or ⌘⇧↵ where ⌘↵ sends.
pub(crate) fn chord_keys(app: &App) -> String {
    if app.prefs.enter_sends {
        format!("{MOD}↵")
    } else {
        format!("{MOD}⇧↵")
    }
}

fn hints(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let key = |keys: String, what: &'static str| -> El<'_> {
        row![
            text(keys).size(11.5).font(mono()).color(t.fg_2),
            text(what).size(11.5).color(t.fg_3).wrapping(text::Wrapping::None)
        ]
        .spacing(4)
        .align_y(Alignment::Center)
        .into()
    };
    let (waiting, running) = (app.chat.transcript.pending_approval().is_some(), app.running());
    // As Enter was set: it sends, or it breaks the line and the chord sends.
    let (send, newline) = if app.prefs.enter_sends {
        ("↵".to_owned(), "⇧↵".to_owned())
    } else {
        (format!("{MOD}↵"), "↵".to_owned())
    };
    let chord = chord_keys(app);
    let mut keys = Vec::new();
    if waiting {
        keys.push(key(chord, s.h_allow));
        keys.push(key(send, s.deny_send));
    } else {
        keys.push(key(send, if running { s.send_more } else { s.h_send }));
        if running {
            keys.push(key(chord, s.h_now));
        }
    }
    keys.push(key(newline, s.h_nl));
    if running {
        keys.push(key("Esc".into(), s.stop));
    }
    // What the plugins show as their status, as the newest run left it.
    if !app.chat.transcript.status.is_empty() {
        keys.push(Space::new().width(Length::Fill).into());
        for status in app.chat.transcript.status.values() {
            keys.push(
                text(clip(first_line(status), 48))
                    .size(11.5)
                    .color(t.fg_3)
                    .wrapping(text::Wrapping::None)
                    .into(),
            );
        }
    }
    container(Row::with_children(keys).spacing(14).align_y(Alignment::Center).wrap())
        .padding([0, 6])
        .into()
}
