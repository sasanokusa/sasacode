//! The conversation beside the one shown: its messages as they come, read only. To type or answer
//! in it, it is taken into the main place.

use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Alignment, Length};

use super::timeline::{bubble, reply};
use super::{El, bold, clip, first_line, hline, mono, tip};
use crate::app::{App, Chat, Msg};
use crate::icons::{Icon, icon};
use crate::transcript::{Note, Step};

/// The scrollable of the conversation beside.
pub const SPLIT: &str = "split";

pub fn pane<'a>(app: &'a App, chat: &'a Chat) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let w = s.w();
    let id = chat.session.as_deref().unwrap_or_default();
    let name = app
        .prefs
        .names
        .get(id)
        .map(String::as_str)
        .or_else(|| chat.transcript.exchanges.first().map(|ex| ex.prompt.trim()))
        .unwrap_or(id);
    let waiting = chat.running() && chat.transcript.pending_approval().is_some();
    let dot = if waiting {
        Some(t.warn)
    } else if chat.running() {
        Some(t.accent)
    } else {
        None
    };
    let mut head = row![].spacing(8).align_y(Alignment::Center).padding([0, 12]).height(44);
    if let Some(color) = dot {
        head = head.push(icon(Icon::Dot, 12.0, color));
    }
    head = head
        .push(
            text(clip(first_line(name), 48))
                .size(13)
                .font(bold())
                .color(t.fg)
                .wrapping(text::Wrapping::None),
        )
        .push(Space::new().width(Length::Fill))
        .push(tip(
            t,
            button(text(w.split_swap).size(12.5))
                .padding([4, 10])
                .style(t.ghost(t.fg_2, false))
                .on_press(Msg::SplitSwap),
            w.split_swap_hint.to_owned(),
            iced::widget::tooltip::Position::Bottom,
        ))
        .push(tip(
            t,
            button(icon(Icon::Close, 13.0, t.fg_3))
                .padding(4)
                .style(t.ghost(t.fg_3, false))
                .on_press(Msg::SplitClose),
            w.split_close.to_owned(),
            iced::widget::tooltip::Position::Bottom,
        ));

    let mut body = column![].spacing(14).padding([16, 18]);
    for ex in &chat.transcript.exchanges {
        if !ex.prompt.is_empty() {
            body = body.push(bubble(t, &ex.prompt));
        }
        for step in &ex.steps {
            match step {
                Step::Text(md) if !md.raw.trim().is_empty() => body = body.push(reply(app, md, false)),
                Step::Tool(tool) => {
                    body = body.push(
                        text(clip(&format!("{} {}", tool.name, tool.summary), 120))
                            .size(12)
                            .font(mono())
                            .color(t.fg_3)
                            .wrapping(text::Wrapping::None),
                    )
                }
                Step::Note(Note::Error(e)) => body = body.push(text(e.as_str()).size(12.5).color(t.err)),
                _ => {}
            }
        }
    }
    if waiting {
        body = body.push(
            container(text(w.split_waiting).size(12.5).color(t.fg))
                .padding([8, 12])
                .width(Length::Fill)
                .style(t.boxed(t.inset, t.warn, crate::theme::RADIUS)),
        );
    } else if chat.running() {
        body = body.push(
            row![
                icon(Icon::Dot, 10.0, t.accent),
                text(s.st_running).size(12.5).color(t.accent)
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    let body = scrollable(body)
        .id(SPLIT)
        .anchor_bottom()
        .height(Length::Fill)
        .style(t.scroll());
    column![head, hline(t), body]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
