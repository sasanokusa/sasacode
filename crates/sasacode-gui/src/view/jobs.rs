//! The background jobs: what each runs, whether it still does, and the end of what it wrote.

use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Alignment, Length};

use super::overlay::title;
use super::{El, clip, mono};
use crate::app::{App, Msg};
use crate::i18n::fill;

pub(crate) fn jobs(app: &App) -> El<'_> {
    let (t, w) = (app.t, app.s.w());
    let mut list = column![].spacing(6);
    if app.jobs.is_empty() {
        list = list.push(text(w.jobs_none).size(12).color(t.fg_3));
    }
    for job in &app.jobs {
        let (state, color) = if job.running {
            (w.jobs_running.to_owned(), t.accent)
        } else if job.killed == Some(true) {
            (w.jobs_killed.to_owned(), t.fg_3)
        } else {
            match job.exit_code {
                Some(0) => (fill(w.jobs_exit, "code", 0), t.fg_3),
                Some(code) => (fill(w.jobs_exit, "code", code), t.err),
                None => (w.jobs_ended.to_owned(), t.fg_3),
            }
        };
        let started = chrono::DateTime::from_timestamp_millis(job.started_at)
            .map(|at| at.with_timezone(&chrono::Local).format("%m/%d %H:%M").to_string())
            .unwrap_or_default();
        let mut line = row![
            text(state).size(11.5).color(color).width(84),
            text(clip(&job.command, 56))
                .size(12.5)
                .font(mono())
                .color(t.fg)
                .width(Length::Fill)
                .wrapping(text::Wrapping::None),
            text(started).size(11.5).color(t.fg_3),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if job.running && cfg!(unix) {
            let asked = app.job_stopping.as_ref() == Some(&job.id);
            line = line.push(
                button(text(if asked { w.jobs_stop_again } else { w.jobs_stop }).size(12))
                    .padding([3, 10])
                    .style(t.outline(if asked { t.err } else { t.fg }))
                    .on_press(Msg::JobStop(job.id.clone())),
            );
        }
        let mut card = column![
            button(line)
                .width(Length::Fill)
                .padding([4, 6])
                .style(t.ghost(t.fg, false))
                .on_press(Msg::JobOpen(job.id.clone())),
            text(clip(&job.cwd, 80)).size(11).color(t.fg_3),
        ]
        .spacing(2);
        if app.job_open.as_ref() == Some(&job.id) {
            let out = if job.tail.is_empty() {
                w.jobs_no_output.to_owned()
            } else {
                job.tail.clone()
            };
            card = card.push(
                container(
                    scrollable(
                        text(out)
                            .size(11.5)
                            .font(mono())
                            .color(t.fg_2)
                            .wrapping(text::Wrapping::WordOrGlyph),
                    )
                    .anchor_bottom()
                    .style(t.scroll()),
                )
                .width(Length::Fill)
                .max_height(220)
                .padding(8)
                .style(t.boxed(t.inset, t.line, 6.0)),
            );
        }
        list = list.push(
            container(card)
                .padding([4, 6])
                .style(t.boxed(t.surface, t.line, crate::theme::RADIUS)),
        );
    }
    column![
        title(app, w.c_jobs),
        text(w.jobs_how).size(12.5).color(t.fg_2),
        container(scrollable(list).style(t.scroll())).max_height(460),
        row![
            Space::new().width(Length::Fill),
            button(text(w.jobs_refresh).size(12.5))
                .padding([5, 12])
                .style(t.outline(t.fg))
                .on_press(Msg::JobsRefresh),
        ],
    ]
    .spacing(12)
    .into()
}
