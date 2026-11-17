//! The usage screen, laid out as `/usage` is: the spans, the graph of the days, the last days one
//! by one, each model; and the conversations that used the most. And the screen that sends one
//! message to several models.

use chrono::{Datelike, Duration};
use iced::widget::{Space, button, column, container, responsive, row, scrollable, text, tooltip};
use iced::{Alignment, Color, Length};

use super::overlay::title;
use super::{El, MONO, tip};
use crate::app::{App, Msg};
use crate::i18n::{compact, fill};
use crate::icons::{Icon, icon};
use crate::usage::{HISTORY, Part, Report, Sum, levels, streaks, weeks};

/// A day of the graph, and the room between two.
const CELL: f32 = 11.0;
const GAP: f32 = 3.0;

/// ` · $0.123`, when anything was paid.
fn cost(c: f64) -> String {
    if c > 0.0 {
        format!(" · ${:.*}", if c < 1.0 { 3 } else { 2 }, c)
    } else {
        String::new()
    }
}

fn sum_line(app: &App, s: &Sum) -> String {
    let w = app.s.w();
    let line = fill(
        &fill(
            &fill(w.usage_sum, "tokens", compact(s.tokens)),
            "output",
            compact(s.output),
        ),
        "n",
        s.requests,
    );
    format!("{line}{}", cost(s.cost))
}

/// What the conversations used.
pub(crate) fn usage(app: &App) -> El<'_> {
    let (t, w) = (app.t, app.s.w());
    let choice = |label: &'static str, on: bool, msg: Msg| -> El<'_> {
        let b = button(text(label).size(12).wrapping(text::Wrapping::None))
            .padding([3, 10])
            .on_press(msg);
        if on {
            b.style(t.primary()).into()
        } else {
            b.style(t.ghost(t.fg_2, false)).into()
        }
    };
    // Which part, as `/usage` and its words; and of where.
    let mut parts = row![].spacing(4);
    for (part, label) in Part::ALL.into_iter().zip(w.usage_parts.split(',')) {
        parts = parts.push(choice(label, app.usage_part == part, Msg::UsagePart(part)));
    }
    let scope = row![
        parts,
        Space::new().width(Length::Fill),
        choice(w.usage_all, !app.usage_here, Msg::Usage(false)),
        choice(w.usage_here, app.usage_here, Msg::Usage(true)),
    ]
    .spacing(4)
    .align_y(Alignment::Center);
    let Some(r) = &app.usage else {
        return column![
            title(app, w.usage_title),
            scope,
            text(w.usage_reading).size(12.5).color(t.fg_3)
        ]
        .spacing(14)
        .into();
    };
    let heading = |label: String| text(label).size(13).font(super::BOLD).color(t.fg);
    let spans = [w.usage_today, w.usage_week, w.usage_month, w.usage_ever];
    let mut overview = column![].spacing(4);
    for (label, s) in spans.iter().zip(&r.spans) {
        overview = overview.push(
            row![
                text(*label).size(12.5).color(t.fg_3).width(96),
                text(sum_line(app, s)).size(12.5).color(t.fg),
            ]
            .spacing(8),
        );
    }
    let mut body = column![title(app, w.usage_title), scope].spacing(14);
    let part = app.usage_part;
    if part == Part::All {
        body = body.push(overview);
    }
    if matches!(part, Part::All | Part::Graph) {
        body = body.push(graph(app, r));
    }
    if matches!(part, Part::All | Part::History) {
        body = body
            .push(heading(fill(w.usage_history, "n", HISTORY)))
            .push(history(app, r));
    }
    if matches!(part, Part::All | Part::Models) {
        body = body.push(heading(w.usage_models.to_owned())).push(models(app, r));
    }
    if part == Part::All {
        body = body.push(heading(w.usage_top.to_owned())).push(top(app, r));
    }
    container(scrollable(body.padding(iced::Padding::ZERO.right(12))).style(t.scroll()))
        .max_height(700)
        .into()
}

/// The days as GitHub draws contributions: a column a week, as many as there is room for.
fn graph<'a>(app: &'a App, r: &'a Report) -> El<'a> {
    let (t, w) = (app.t, app.s.w());
    responsive(move |size| {
        let n = (((size.width - 24.0) / (CELL + GAP)).floor() as usize).clamp(4, 53);
        let grid = weeks(r.today, n);
        let values: Vec<u64> = grid
            .iter()
            .flatten()
            .flatten()
            .map(|d| r.days.get(d).map_or(0, |s| s.tokens))
            .collect();
        let level = levels(&values);
        let shade = |l: usize| -> Color {
            if l == 0 {
                t.inset
            } else {
                Color {
                    a: [0.0, 0.32, 0.52, 0.76, 1.0][l],
                    ..t.add
                }
            }
        };
        let months: Vec<&str> = w.months.split(',').collect();
        let days: Vec<&str> = w.weekdays.split(',').collect();
        // The months over the week each begins in, where there is room.
        let mut labels = row![Space::new().width(24)];
        let mut at = 0.0;
        for (i, week) in grid.iter().enumerate() {
            let first = week
                .iter()
                .flatten()
                .find(|d| d.day() == 1)
                .or(if i == 0 { week[0].as_ref() } else { None });
            if let Some(day) = first {
                let x = i as f32 * (CELL + GAP);
                let label = months.get(day.month0() as usize).copied().unwrap_or("");
                let width = label.chars().count() as f32 * 9.0;
                if x >= at {
                    labels = labels.push(Space::new().width(x - at));
                    labels = labels.push(
                        text(label)
                            .size(10.5)
                            .color(t.fg_3)
                            .wrapping(text::Wrapping::None)
                            .width(width),
                    );
                    at = x + width;
                }
            }
        }
        let mut names = column![].spacing(GAP);
        for d in 0..7 {
            let name = if d % 2 == 1 {
                days.get(d).copied().unwrap_or("")
            } else {
                ""
            };
            names = names.push(container(text(name).size(10).color(t.fg_3)).height(CELL).width(20));
        }
        let mut columns = row![names].spacing(GAP);
        for week in &grid {
            let mut col = column![].spacing(GAP);
            for day in week {
                let Some(day) = day else {
                    col = col.push(Space::new().width(CELL).height(CELL));
                    continue;
                };
                let s = r.days.get(day).copied().unwrap_or_default();
                let color = shade(level(s.tokens));
                let cell = container(Space::new().width(CELL).height(CELL)).style(move |_| container::Style {
                    background: Some(color.into()),
                    border: iced::border::rounded(3.0),
                    ..container::Style::default()
                });
                let hint = if s.requests == 0 {
                    day.format("%Y-%m-%d").to_string()
                } else {
                    format!("{} · {}", day.format("%Y-%m-%d"), sum_line(app, &s))
                };
                col = col.push(tip(t, cell, hint, tooltip::Position::Top));
            }
            columns = columns.push(col);
        }
        let mut legend = row![text(w.usage_less).size(10.5).color(t.fg_3)]
            .spacing(GAP)
            .align_y(Alignment::Center);
        for l in 0..5 {
            let color = shade(l);
            legend = legend.push(
                container(Space::new().width(CELL).height(CELL)).style(move |_| container::Style {
                    background: Some(color.into()),
                    border: iced::border::rounded(3.0),
                    ..container::Style::default()
                }),
            );
        }
        legend = legend.push(text(w.usage_more).size(10.5).color(t.fg_3));
        // What the weeks shown add up to, the busiest day, and the days in a row.
        let from = grid.first().and_then(|w| w[0]).unwrap_or(r.today);
        let shown: Vec<(&chrono::NaiveDate, &Sum)> = r.days.range(from..).collect();
        let total: u64 = shown.iter().map(|(_, s)| s.tokens).sum();
        let mut summary = fill(&fill(w.usage_graph_sum, "tokens", compact(total)), "n", shown.len());
        if let Some((day, s)) = shown.iter().max_by_key(|(_, s)| s.tokens) {
            summary = format!(
                "{summary} · {}",
                fill(
                    &fill(w.usage_peak, "day", day.format("%Y-%m-%d")),
                    "tokens",
                    compact(s.tokens)
                )
            );
        }
        let (current, longest) = streaks(&r.days, r.today);
        column![
            text(fill(w.usage_graph, "n", n)).size(13).font(super::BOLD).color(t.fg),
            labels,
            columns,
            legend,
            text(summary).size(12).color(t.fg_2),
            text(fill(&fill(w.usage_streak, "current", current), "longest", longest))
                .size(12)
                .color(t.fg_3),
        ]
        .spacing(6)
        .into()
    })
    .height(Length::Shrink)
    .into()
}

/// A bar `fraction` of `width` long.
fn bar<'a>(app: &App, fraction: f64, width: f32) -> El<'a> {
    let t = app.t;
    let filled = (fraction.clamp(0.0, 1.0) as f32 * width).round();
    let (on, off) = (t.add, t.inset);
    row![
        container(Space::new().width(filled).height(8)).style(move |_| container::Style {
            background: Some(on.into()),
            border: iced::border::rounded(2.0),
            ..container::Style::default()
        }),
        container(Space::new().width(width - filled).height(8)).style(move |_| container::Style {
            background: Some(off.into()),
            border: iced::border::rounded(2.0),
            ..container::Style::default()
        }),
    ]
    .into()
}

/// The last days, one by one.
fn history<'a>(app: &'a App, r: &'a Report) -> El<'a> {
    let (t, w) = (app.t, app.s.w());
    let days: Vec<&str> = w.weekdays.split(',').collect();
    let rows: Vec<(chrono::NaiveDate, Sum)> = (0..HISTORY as i64)
        .rev()
        .map(|i| r.today - Duration::days(i))
        .map(|d| (d, r.days.get(&d).copied().unwrap_or_default()))
        .collect();
    let max = rows.iter().map(|(_, s)| s.tokens).max().unwrap_or(0).max(1);
    let mut list = column![].spacing(3);
    for (day, s) in rows {
        let name = days
            .get(day.weekday().num_days_from_sunday() as usize)
            .copied()
            .unwrap_or("");
        let label = text(format!("{} {name}", day.format("%m-%d")))
            .size(12)
            .font(MONO)
            .color(t.fg_3)
            .width(76);
        let line: El<'_> = if s.requests == 0 {
            row![label, bar(app, 0.0, 160.0), text("-").size(12).color(t.fg_3)]
                .spacing(8)
                .align_y(Alignment::Center)
                .into()
        } else {
            let rest = fill(&fill(w.usage_day_rest, "output", compact(s.output)), "n", s.requests);
            row![
                label,
                bar(app, s.tokens as f64 / max as f64, 160.0),
                text(compact(s.tokens)).size(12).font(MONO).color(t.fg).width(56),
                text(format!("{rest}{}", cost(s.cost))).size(11.5).color(t.fg_3),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
        };
        list = list.push(line);
    }
    list.into()
}

/// Each model: its share of all, how often, what it cost and when it was last used.
fn models<'a>(app: &'a App, r: &'a Report) -> El<'a> {
    let (t, w) = (app.t, app.s.w());
    if r.models.is_empty() {
        return text(w.usage_none).size(12.5).color(t.fg_3).into();
    }
    let all = r.models.iter().map(|m| m.sum.tokens).sum::<u64>().max(1);
    let mut list = column![].spacing(4);
    for m in &r.models {
        let share = m.sum.tokens as f64 / all as f64;
        let rest = format!(
            "{}% · {}{} · {}",
            (share * 100.0).round(),
            fill(w.usage_times, "n", m.sum.requests),
            cost(m.sum.cost),
            fill(w.usage_last, "day", m.last.format("%m-%d"))
        );
        list = list.push(
            row![
                text(m.model.as_str())
                    .size(12)
                    .font(MONO)
                    .color(t.fg)
                    .width(Length::FillPortion(2))
                    .wrapping(text::Wrapping::WordOrGlyph),
                bar(app, share, 100.0),
                text(compact(m.sum.tokens)).size(12).font(MONO).color(t.fg).width(56),
                text(rest).size(11.5).color(t.fg_3).width(Length::FillPortion(3)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    list.into()
}

/// The conversations that used the most; one of this folder opens.
fn top<'a>(app: &'a App, r: &'a Report) -> El<'a> {
    let (t, w) = (app.t, app.s.w());
    if r.top.is_empty() {
        return text(w.usage_none).size(12.5).color(t.fg_3).into();
    }
    let mut list = column![].spacing(2);
    for c in &r.top {
        let here = app.sessions.iter().any(|s| s.id == c.id);
        let first = if c.first.trim().is_empty() {
            app.s.new_session
        } else {
            c.first.as_str()
        };
        let line = row![
            text(super::clip(first, 60))
                .size(12.5)
                .color(if here { t.fg } else { t.fg_2 })
                .width(Length::Fill)
                .wrapping(text::Wrapping::None),
            text(format!("{}{}", compact(c.sum.tokens), cost(c.sum.cost)))
                .size(12)
                .font(MONO)
                .color(t.fg_3),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        list = list.push(
            button(line)
                .width(Length::Fill)
                .padding([3, 8])
                .style(t.ghost(t.fg, false))
                .on_press_maybe(here.then(|| Msg::OpenSession(c.log.clone()))),
        );
    }
    list.into()
}

/// The models one message goes to at once.
pub(crate) fn compare(app: &App) -> El<'_> {
    let (t, w) = (app.t, app.s.w());
    let mut known: Vec<String> = app.listing.models.iter().flatten().map(|m| m.id.clone()).collect();
    for chosen in &app.compare {
        if !known.contains(chosen) {
            known.push(chosen.clone());
        }
    }
    let mut list = column![].spacing(2);
    if known.is_empty() {
        list = list.push(text(w.compare_none).size(12).color(t.fg_3));
    }
    for model in known {
        let on = app.compare.contains(&model);
        list = list.push(
            button(
                row![
                    container(icon(Icon::Check, 12.0, if on { t.fg } else { Color::TRANSPARENT })).width(16),
                    text(model.clone()).size(12.5).font(MONO).color(t.fg),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([4, 8])
            .style(t.ghost(t.fg, on))
            .on_press(Msg::CompareToggle(model)),
        );
    }
    let typed = row![
        iced::widget::text_input(w.compare_add_ph, &app.compare_typed)
            .on_input(Msg::CompareType)
            .on_submit(Msg::CompareAdd)
            .size(12.5)
            .font(MONO)
            .padding([5, 8])
            .style(t.input()),
        button(text(w.compare_add).size(12.5))
            .padding([5, 12])
            .style(t.outline(t.fg))
            .on_press(Msg::CompareAdd),
    ]
    .spacing(6);
    let typed_text = app.chat.editor.text();
    let empty = typed_text.trim().is_empty();
    let n = app.compare.len();
    let send = button(text(fill(w.compare_send, "n", n)).size(13))
        .padding([6, 14])
        .style(t.primary())
        .on_press_maybe((!empty && n > 0).then_some(Msg::CompareSend));
    let mut foot = row![Space::new().width(Length::Fill), send]
        .spacing(8)
        .align_y(Alignment::Center);
    if empty {
        foot = row![text(w.compare_empty).size(12).color(t.warn), foot]
            .spacing(8)
            .align_y(Alignment::Center);
    }
    column![
        title(app, w.compare_title),
        text(w.compare_how).size(12.5).color(t.fg_2),
        container(scrollable(list).style(t.scroll())).max_height(300),
        typed,
        foot,
    ]
    .spacing(12)
    .into()
}
