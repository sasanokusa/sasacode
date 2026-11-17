//! The conversation. Each exchange is the user's message, the steps taken about it (folded away
//! once it is answered) and the reply.

use iced::widget::{Column, Row, Space, button, column, container, markdown, row, rule, scrollable, stack, text};
use iced::{Alignment, Color, Length, Padding, border};
use sasacode_protocol::StopCause;

use super::{
    El, bold, clip, col_w, copy_button, diff_view, first_line, mono, output_block, plus_minus, short_path, tag, ui,
};
use crate::app::{App, Clip, Msg, TIMELINE, Tab};
use crate::i18n::fill;
use crate::icons::{Icon, icon};
use crate::theme::{RADIUS, RADIUS_L, Tokens};
use crate::transcript::{
    Answer, Approval, End, Exchange, Kind, Markdown, Note, Step, Todo, TodoStatus, Tool, ToolState,
};

/// How far what belongs to a row is set in under it: past the row's icon.
const INDENT: f32 = 28.0;

pub fn timeline(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    if app.chat.transcript.is_empty() && app.chat.next.is_none() {
        return blank(app);
    }
    let mut col = column![].spacing(22).width(Length::Fill);
    // While the search inside the conversation has something typed, only what has it is shown.
    let needle = app
        .find_in
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_lowercase);
    // Of those found, the one stepped to is marked.
    let current = needle.as_ref().and_then(|_| app.find_current());
    for (i, ex) in app.chat.transcript.exchanges.iter().enumerate() {
        if needle.as_deref().is_none_or(|n| has(ex, n)) {
            let mut shown = container(exchange(app, i, ex)).id(crate::outline::exchange_id(i));
            if current == Some(i) {
                shown = shown.padding([6, 8]).style(t.boxed(t.bg, t.accent, RADIUS));
            }
            col = col.push(shown);
        }
    }
    for queued in &app.chat.transcript.queued {
        col = col.push(
            column![
                bubble(t, queued),
                container(text(s.next_turn).size(11.5).color(t.fg_3)).align_right(Length::Fill),
            ]
            .spacing(4),
        );
    }
    if let Some(next) = &app.chat.next {
        // Not sent yet: it can be taken back.
        col = col.push(
            column![
                bubble(t, next),
                container(
                    button(text(s.retract).size(11.5).color(t.fg_3))
                        .padding([2, 8])
                        .style(t.ghost(t.fg_2, false))
                        .on_press(Msg::Retract)
                )
                .align_right(Length::Fill),
            ]
            .spacing(4),
        );
    }
    let mut list = scrollable(
        container(container(col).max_width(col_w(app)))
            .center_x(Length::Fill)
            .padding([20, 24]),
    )
    .direction(scrollable::Direction::Vertical(
        scrollable::Scrollbar::new().width(8).scroller_width(8).margin(2),
    ));
    // After the direction, which it is a part of. Held by its end, the newest is what is in view
    // as the conversation grows; once the reader scrolls back it is held by its start, so what
    // comes in does not move what they read.
    if !app.reading {
        list = list.anchor_bottom();
    }
    let list = list
        .id(TIMELINE)
        .on_scroll(|v| Msg::Scrolled {
            offset: v.absolute_offset().y,
            room: (v.content_bounds().height - v.bounds().height).max(0.0),
        })
        .style(t.scroll())
        .width(Length::Fill)
        .height(Length::Fill);
    // Always a stack, so that the list keeps where it is scrolled to when the button comes and goes.
    if !app.reading {
        return stack![list, Space::new()].into();
    }
    let latest = button(
        row![
            icon(Icon::Down, 13.0, t.fg_2),
            text(s.latest).size(12.5).color(t.fg).wrapping(text::Wrapping::None)
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding([6, 12])
    .style(t.ghost(t.fg, false))
    .on_press(Msg::Latest);
    stack![
        list,
        container(container(latest).style(t.floating(t.raised, t.line_2)))
            .align_bottom(Length::Fill)
            .center_x(Length::Fill)
            .padding(14)
    ]
    .into()
}

/// Whether an exchange says `needle` (lower case) anywhere: in its message, what was answered, or
/// what its calls were.
pub(crate) fn has(ex: &Exchange, needle: &str) -> bool {
    let says = |text: &str| text.to_lowercase().contains(needle);
    says(&ex.prompt)
        || ex.steps.iter().any(|step| match step {
            Step::Text(md) => says(&md.raw),
            Step::Tool(tool) => says(&tool.summary) || says(&tool.name),
            Step::Note(Note::Error(body) | Note::Harness(body)) => says(body),
            _ => false,
        })
}

/// A conversation that has not started.
fn blank(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    container(
        column![
            icon(Icon::Leaf, 28.0, t.accent),
            text(s.blank_h).size(20).font(bold()).color(t.fg),
            text(if app.chat.bare { s.w().bare_blank } else { s.blank_p })
                .size(13)
                .color(t.fg_3),
        ]
        .spacing(10)
        .align_x(Alignment::Center),
    )
    .center(Length::Fill)
    .padding(24)
    .into()
}

/// The corners of what the user said: round, a pill while it is one line.
const BUBBLE: f32 = 18.0;

/// What the user said, on the right.
/// A message's start, drawn as the message is.
fn bubble_owned<'a>(t: Tokens, prompt: String) -> El<'a> {
    container(
        container(text(prompt).size(14).color(t.fg))
            .padding([8, 14])
            .max_width(560)
            .style(t.boxed(t.raised, t.line, BUBBLE)),
    )
    .align_right(Length::Fill)
    .into()
}

pub(crate) fn bubble<'a>(t: Tokens, prompt: &'a str) -> El<'a> {
    container(
        container(text(prompt).size(14).color(t.fg))
            .padding([8, 14])
            .max_width(560)
            .style(t.boxed(t.raised, t.line, BUBBLE)),
    )
    .align_right(Length::Fill)
    .into()
}

/// What the user said, with what the search inside the conversation found marked in it.
fn bubble_found<'a>(t: Tokens, prompt: &str, needle: &str) -> El<'a> {
    let said: Vec<iced::advanced::text::Span<'static, ()>> = vec![iced::advanced::text::Span::new(prompt.to_owned())];
    container(
        container(
            iced::widget::rich_text(found_marked(&said, needle, found_mark(t)))
                .size(14)
                .color(t.fg),
        )
        .padding([8, 14])
        .max_width(560)
        .style(t.boxed(t.raised, t.line, BUBBLE)),
    )
    .align_right(Length::Fill)
    .into()
}

/// How what the search found is marked.
fn found_mark(t: Tokens) -> iced::advanced::text::Highlight {
    iced::advanced::text::Highlight {
        background: t.warn_wash.into(),
        border: border::rounded(3.0).color(t.warn).width(1.0),
    }
}

/// `spans` with each place that says `needle` (lower case) made a span of its own, `mark`ed. A
/// span whose lower case is not as long as it is (so that places in one are not places in the
/// other) is left as it is.
pub(crate) fn found_marked<Link: Clone>(
    spans: &[iced::advanced::text::Span<'static, Link>],
    needle: &str,
    mark: iced::advanced::text::Highlight,
) -> Vec<iced::advanced::text::Span<'static, Link>> {
    let mut out = Vec::new();
    for span in spans {
        let whole: &str = span.text.as_ref();
        let lower = whole.to_lowercase();
        if needle.is_empty() || lower.len() != whole.len() || !lower.contains(needle) {
            out.push(span.clone());
            continue;
        }
        let mut at = 0;
        for (start, _) in lower.match_indices(needle) {
            if start < at || !whole.is_char_boundary(start) || !whole.is_char_boundary(start + needle.len()) {
                continue;
            }
            if start > at {
                let mut before = span.clone();
                before.text = whole[at..start].to_owned().into();
                out.push(before);
            }
            let mut found = span.clone();
            found.text = whole[start..start + needle.len()].to_owned().into();
            found.highlight = Some(mark);
            out.push(found);
            at = start + needle.len();
        }
        if at < whole.len() {
            let mut after = span.clone();
            after.text = whole[at..].to_owned().into();
            out.push(after);
        }
    }
    out
}

fn exchange<'a>(app: &'a App, i: usize, ex: &'a Exchange) -> El<'a> {
    let t = app.t;
    let last = i + 1 == app.chat.transcript.exchanges.len();
    let live = last && app.running();
    let mut col = column![].spacing(10);
    if !ex.prompt.is_empty() {
        // A long message is folded to its start until it is opened.
        let mut said = match crate::extras::folded(&ex.prompt) {
            Some(head) => {
                let whole = app.chat.whole.contains(&i);
                let toggle = button(
                    text(if whole {
                        app.s.w().fold_whole
                    } else {
                        app.s.w().show_whole
                    })
                    .size(12)
                    .color(t.accent),
                )
                .padding([2, 6])
                .style(t.ghost(t.accent, false))
                .on_press(Msg::Whole(i));
                column![
                    if whole {
                        bubble(t, &ex.prompt)
                    } else {
                        bubble_owned(t, head)
                    },
                    container(toggle).align_right(Length::Fill)
                ]
                .spacing(3)
            }
            // What the search found in it is marked.
            None => match app.find_needle() {
                Some(needle) => column![bubble_found(t, &ex.prompt, &needle)].spacing(3),
                None => column![bubble(t, &ex.prompt)].spacing(3),
            },
        };
        // The images it named, small, each opened by a click.
        let images = crate::composer::image_refs(&ex.prompt, &app.cwd);
        if !images.is_empty() {
            let mut line = row![Space::new().width(Length::Fill)].spacing(6);
            for (path, _) in images {
                line = line.push(
                    button(
                        container(
                            iced::widget::image(iced::widget::image::Handle::from_path(&path))
                                .height(96)
                                .content_fit(iced::ContentFit::Contain),
                        )
                        .max_width(220)
                        .style(t.boxed(t.raised, t.line, RADIUS)),
                    )
                    .padding(0)
                    .style(t.ghost(t.fg, false))
                    .on_press(Msg::ViewImage(path)),
                );
            }
            said = said.push(line);
        }
        // When it was sent, as the list says how long ago a conversation was used.
        if let Some(at) = ex.at {
            let ago = std::time::SystemTime::now().duration_since(at).unwrap_or_default();
            said = said.push(
                container(text(app.s.ago(ago)).size(11).color(t.fg_3))
                    .align_right(Length::Fill)
                    .padding([0, 4]),
            );
        }
        // Under the pointer, once the conversation is in its log and nothing runs: what can be
        // done with the message.
        let can = !app.busy() && app.chat.session.is_some() && !live;
        if can && app.ex_hover == Some(i) {
            let small = |label: &'a str, msg: Msg| {
                button(text(label).size(11.5).color(t.fg_3).wrapping(text::Wrapping::None))
                    .padding([2, 8])
                    .style(t.ghost(t.fg_2, false))
                    .on_press(msg)
            };
            let mut acts = row![Space::new().width(Length::Fill)]
                .spacing(2)
                .align_y(Alignment::Center);
            acts = acts.push(super::tip(
                t,
                small(app.s.edit_resend, Msg::Fork(i)),
                app.s.fork_hint.to_owned(),
                iced::widget::tooltip::Position::Bottom,
            ));
            if last {
                acts = acts.push(super::tip(
                    t,
                    small(app.s.retry, Msg::Retry),
                    app.s.fork_hint.to_owned(),
                    iced::widget::tooltip::Position::Bottom,
                ));
            }
            said = said.push(acts);
        }
        col = col.push(
            iced::widget::mouse_area(said)
                .on_enter(Msg::ExHover(Some(i)))
                .on_exit(Msg::ExHover(None)),
        );
    }
    if live {
        // While it runs, everything is in the order it happened, the text being written included.
        if !ex.steps.is_empty() {
            col = col.push(steps(app, i, &ex.steps));
        }
        return col.push(live_row(app, ex)).into();
    }
    // What came back, apart from what was sent: under the pointer it shows what can be done with
    // the reply.
    let mut said = col;
    let mut col = column![].spacing(10).width(Length::Fill);
    let mut any = false;
    let answer = ex.answer();
    let work = &ex.steps[..answer.unwrap_or(ex.steps.len())];
    // The steps hidden by the settings: only what was said, and how it ended.
    if !work.is_empty() && app.prefs.steps {
        let open = app.steps_open(i);
        any = true;
        col = col.push(fold_row(app, i, ex, work, open));
        if open {
            col = col.push(
                row![
                    rule::vertical(2).style(t.rule(t.line)),
                    steps(app, i, work).width(Length::Fill)
                ]
                .spacing(10),
            );
        }
    }
    if !app.prefs.steps && answer.is_none() {
        // What went wrong is how it ended, shown with the steps hidden too: read from a log, an
        // exchange has no end of its run to say it. One that was answered after all ended well.
        for (j, step) in work.iter().enumerate() {
            if let Step::Note(note @ Note::Error(_)) = step {
                any = true;
                col = col.push(note_row(app, i, j, note));
            }
        }
    }
    if let Some(Step::Text(md)) = answer.map(|a| &ex.steps[a]) {
        any = true;
        col = col.push(reply(app, md, true));
    }
    // The reply as it was written, Markdown and all: its text cannot be selected here. Shown under
    // the pointer and under the last reply, in a place kept for it, so nothing moves when it shows.
    let shown = last || app.reply_hover == Some(i);
    let copy = answer.map(|_| {
        if !shown {
            return container(Space::new()).height(FOOT).into();
        }
        let mut foot = row![].spacing(2).align_y(Alignment::Center);
        // Branched off here: a new conversation holding this one through this reply.
        if !app.busy() && app.chat.session.is_some() {
            let w = app.s.w();
            foot = foot.push(super::tip(
                t,
                button(
                    text(w.fork_after)
                        .size(11.5)
                        .color(t.fg_3)
                        .wrapping(text::Wrapping::None),
                )
                .padding([2, 8])
                .style(t.ghost(t.fg_2, false))
                .on_press(Msg::ForkAfter(i)),
                w.fork_after_hint.to_owned(),
                iced::widget::tooltip::Position::Bottom,
            ));
        }
        // Part of it chosen and quoted into the input.
        foot = foot.push(super::tip(
            t,
            button(
                text(app.s.w().quote)
                    .size(11.5)
                    .color(t.fg_3)
                    .wrapping(text::Wrapping::None),
            )
            .padding([2, 8])
            .style(t.ghost(t.fg_2, false))
            .on_press(Msg::QuoteOpen(i)),
            app.s.w().quote_part_hint.to_owned(),
            iced::widget::tooltip::Position::Bottom,
        ));
        let foot = foot.push(copy_button(app, Clip::Reply(i), app.s.copy));
        container(foot).height(FOOT).align_y(Alignment::Center).into()
    });
    match (&ex.end, copy) {
        (Some(end), copy) => col = col.push(ending(app, ex, end, last, copy)),
        (None, Some(copy)) => col = col.push(copy),
        (None, None) => {}
    }
    any |= ex.end.is_some() || answer.is_some();
    if !any {
        return said.into();
    }
    said = said.push(
        iced::widget::mouse_area(col)
            .on_enter(Msg::ReplyHover(i, true))
            .on_exit(Msg::ReplyHover(i, false)),
    );
    said.into()
}

/// The height kept under a reply for what can be done with it, shown or not.
const FOOT: f32 = 24.0;

/// The line that stands for the steps of a finished exchange.
fn fold_row<'a>(app: &'a App, i: usize, ex: &'a Exchange, work: &'a [Step], open: bool) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let calls = work.iter().filter(|s| matches!(s, Step::Tool(_))).count();
    let mut parts = Vec::new();
    if let Some(d) = ex.elapsed {
        parts.push(fill(s.worked, "d", s.duration(d)));
    }
    if calls > 0 {
        let mut said = fill(s.calls, "n", calls);
        if ex.failed() > 0 {
            let failed = fill(s.failed, "n", ex.failed());
            said = if s.ja {
                format!("{said}（{failed}）")
            } else {
                format!("{said} ({failed})")
            };
        }
        parts.push(said);
    }
    let toggle = if open { s.hide_steps } else { s.show_steps };
    let label = if parts.is_empty() {
        toggle.to_owned()
    } else {
        parts.join(" · ")
    };
    let mut content = row![
        icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 12.0, t.fg_3),
        text(label).size(12.5).color(t.fg_2),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if !parts.is_empty() {
        content = content
            .push(Space::new().width(Length::Fill))
            .push(text(toggle).size(12).color(t.fg_3));
    }
    button(content)
        .width(Length::Fill)
        .padding([5, 8])
        .style(t.ghost(t.fg_2, false))
        .on_press(Msg::Fold(i))
        .into()
}

/// The line under a run that is going on.
fn live_row<'a>(app: &'a App, ex: &'a Exchange) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let waiting = app.chat.transcript.pending_approval().is_some();
    let (label, color) = if waiting {
        (s.st_waiting, t.warn)
    } else {
        (s.live_work, t.accent)
    };
    let mut line = row![icon(Icon::Dot, 10.0, color), text(label).size(12.5).color(color)]
        .spacing(8)
        .align_y(Alignment::Center);
    if let Some(started) = ex.started {
        line = line.push(
            text(s.duration(app.now.saturating_duration_since(started)))
                .size(12)
                .color(t.fg_3),
        );
    }
    let stop = if s.ja {
        format!("Esc{}", s.to_stop)
    } else {
        format!("Esc {}", s.to_stop)
    };
    line.push(text("·").size(12).color(t.line_2))
        .push(text(stop).size(12).color(t.fg_3))
        .padding([2, 8])
        .into()
}

fn steps<'a>(app: &'a App, i: usize, work: &'a [Step]) -> Column<'a, Msg> {
    // What is still being written is not offered to copy.
    let live = i + 1 == app.chat.transcript.exchanges.len() && app.running();
    let mut col = column![].spacing(2);
    let more = &app.prefs.more;
    for (j, step) in work.iter().enumerate() {
        // As the settings say: calls that went well, and what the model thought, left out.
        match step {
            Step::Thinking(_) if more.hide_thinking => continue,
            Step::Tool(tool) if more.failed_only && tool.state == crate::transcript::ToolState::Done => continue,
            _ => {}
        }
        col = col.push(match step {
            Step::Thinking(thought) => thinking(app, i, j, thought),
            Step::Text(md) => container(reply(app, md, !live)).padding([6, 8]).into(),
            Step::Tool(tool) => tool_row(app, i, j, tool),
            Step::Approval(approval) => approval_row(app, approval),
            Step::Note(note) => note_row(app, i, j, note),
        });
    }
    col
}

/// One line of the steps: an icon and what happened; on the right how long it took. With `press`
/// the line is a button that opens what belongs to it.
fn line<'a>(
    t: Tokens,
    mark: (Icon, Color),
    parts: Vec<El<'a>>,
    meta: Option<String>,
    press: Option<(Msg, bool)>,
) -> El<'a> {
    // What happened takes the room the rest leaves and is cut there, so how long it took and the
    // mark that it opens are always in view.
    let said = container(Row::with_children(parts).spacing(8).align_y(Alignment::Center))
        .width(Length::Fill)
        .clip(true);
    let mut content = row![icon(mark.0, 14.0, mark.1), said]
        .spacing(8)
        .align_y(Alignment::Center);
    if let Some(meta) = meta {
        content = content.push(text(meta).size(11.5).color(t.fg_3).wrapping(text::Wrapping::None));
    }
    match press {
        Some((msg, open)) => button(content.push(icon(
            if open { Icon::ChevronDown } else { Icon::ChevronRight },
            11.0,
            t.fg_3,
        )))
        .width(Length::Fill)
        .padding([4, 8])
        .style(t.ghost(t.fg, false))
        .on_press(msg)
        .into(),
        None => container(content).width(Length::Fill).padding([4, 8]).into(),
    }
}

fn say<'a>(label: impl text::IntoFragment<'a>, color: Color) -> El<'a> {
    text(label).size(13).color(color).wrapping(text::Wrapping::None).into()
}

fn code<'a>(label: impl text::IntoFragment<'a>, color: Color) -> El<'a> {
    text(label).size(12.5).font(mono()).color(color).into()
}

/// What belongs to a row, under it.
fn under<'a>(content: impl Into<El<'a>>) -> El<'a> {
    container(content)
        .width(Length::Fill)
        .padding(Padding {
            top: 2.0,
            right: 8.0,
            bottom: 6.0,
            left: INDENT,
        })
        .into()
}

fn thinking<'a>(app: &'a App, i: usize, j: usize, thought: &'a str) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let open = app.chat.open.contains(&(i, j));
    let mut parts = vec![say(s.thinking, t.fg_2)];
    if !open {
        parts.push(say(clip(first_line(thought.trim()), 64), t.fg_3));
    }
    let head = line(t, (Icon::Bulb, t.fg_3), parts, None, Some((Msg::Open(i, j), open)));
    if !open {
        return head;
    }
    column![head, under(text(thought.trim()).size(13).color(t.fg_2))].into()
}

fn tool_row<'a>(app: &'a App, i: usize, j: usize, tool: &'a Tool) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let kind = tool.kind();
    let (running, done) = (tool.state == ToolState::Running, tool.state == ToolState::Done);
    let faint = matches!(tool.state, ToolState::Waiting | ToolState::Skipped);
    let strong = if faint { t.fg_3 } else { t.fg };
    let soft = if faint { t.fg_3 } else { t.fg_2 };
    let path = || short_path(tool.arg("path").unwrap_or(&tool.summary), &app.cwd);

    let mut parts: Vec<El<'a>> = Vec::new();
    let symbol = match kind {
        Kind::Read => {
            parts.push(say(if running { s.v_read_live } else { s.v_read }, soft));
            parts.push(code(clip(&short_path(&tool.summary, &app.cwd), 60), strong));
            if done {
                parts.push(say(fill(s.lines, "n", tool.lines), t.fg_3));
            }
            Icon::File
        }
        Kind::Write | Kind::Edit => {
            let created = tool.change.as_ref().and_then(|c| c.created);
            let verb = match (kind, running, created) {
                (Kind::Edit, true, _) => s.v_edit_live,
                (Kind::Edit, false, _) if faint => s.v_edit_any,
                (Kind::Edit, false, _) => s.v_edit,
                (_, true, _) => s.v_write_live,
                (_, false, Some(true)) => s.v_write,
                (_, false, Some(false)) if done => s.v_overwrite,
                // A log does not say whether the file was there.
                (_, false, None) if done => s.v_wrote,
                (_, false, _) => s.v_write_any,
            };
            parts.push(say(verb, soft));
            parts.push(code(clip(&path(), 60), strong));
            if let Some(change) = tool.change.as_ref().filter(|_| done) {
                parts.push(plus_minus(t, change.diff.added, change.diff.removed));
                if change.created == Some(true) {
                    parts.push(tag(t, s.new_file, t.accent_wash, t.accent));
                }
            }
            if kind == Kind::Edit { Icon::Pencil } else { Icon::File }
        }
        Kind::Bash => {
            let verb = match (running, faint) {
                (true, _) => s.v_bash_live,
                (false, true) => s.v_bash_any,
                (false, false) => s.v_bash,
            };
            parts.push(say(verb, soft));
            parts.push(code(clip(first_line(&tool.summary), 58), strong));
            if tool.finished() && tool.state != ToolState::Skipped {
                let (said, bad) = match (tool.killed.as_deref(), tool.exit_code) {
                    (Some("timeout"), _) => (s.killed_timeout.to_owned(), true),
                    (Some(_), _) => (s.killed_abort.to_owned(), true),
                    (None, Some(0)) | (None, None) => match tool.lines.saturating_sub(1) {
                        0 => (s.no_output.to_owned(), false),
                        n => (fill(s.out_lines, "n", n), false),
                    },
                    (None, Some(code)) => (fill(s.exit, "n", code), true),
                };
                parts.push(say(said, if bad { t.err } else { t.fg_3 }));
            }
            Icon::Terminal
        }
        Kind::Task => {
            parts.push(say(s.v_task, soft));
            parts.push(say(clip(&tool.summary, 56), strong));
            let calls = tool.stream.lines().count();
            if calls > 0 {
                parts.push(say(fill(s.calls, "n", calls), t.fg_3));
            }
            Icon::Bot
        }
        Kind::Plan => {
            match &tool.plan {
                Some(plan) if plan.first => {
                    parts.push(say(s.plan_made, soft));
                    parts.push(say(fill(s.plan_steps, "n", plan.todos.len()), t.fg_3));
                }
                Some(plan) => {
                    if plan.newly_done.is_empty() {
                        parts.push(say(s.plan_updated, soft));
                    } else {
                        let done = plan.newly_done.join(if s.ja { "、" } else { ", " });
                        parts.push(say(format!("{}{}", s.step_done, clip(&done, 60)), soft));
                    }
                    parts.push(say(format!("{}/{}", plan.done(), plan.todos.len()), t.fg_3));
                }
                None => parts.push(code(tool.name.as_str(), strong)),
            }
            Icon::List
        }
        Kind::Other => {
            parts.push(code(tool.name.as_str(), strong));
            let said = if tool.summary.is_empty() {
                first_line(&tool.output)
            } else {
                &tool.summary
            };
            parts.push(say(clip(said, 56), t.fg_3));
            Icon::Spark
        }
    };
    if tool.repaired.is_some() {
        parts.push(tag(t, s.repaired, t.warn_wash, t.warn));
    }
    if let Some((allowed, _)) = &tool.judged {
        let w = s.w();
        parts.push(tag(
            t,
            if *allowed { w.judged_allow } else { w.judged_ask },
            t.inset,
            t.fg_2,
        ));
    }
    match tool.state {
        ToolState::Waiting => {
            let asked = app
                .chat
                .transcript
                .approval_of(&tool.id)
                .is_some_and(|a| a.answer.is_none());
            parts.push(if asked {
                tag(t, s.st_waiting, t.warn_wash, t.warn)
            } else {
                tag(t, s.tag_waiting, t.inset, t.fg_3)
            });
        }
        ToolState::Failed if kind != Kind::Bash => parts.push(tag(t, s.tag_failed, t.err_wash, t.err)),
        ToolState::Skipped => {
            let denied = tool.denied()
                || matches!(
                    app.chat
                        .transcript
                        .approval_of(&tool.id)
                        .and_then(|a| a.answer.as_ref()),
                    Some(Answer::Denied(_))
                );
            parts.push(tag(
                t,
                if denied { s.tag_denied } else { s.tag_not_run },
                t.inset,
                t.fg_2,
            ));
        }
        _ => {}
    }

    let color = match tool.state {
        ToolState::Running => t.accent,
        ToolState::Failed => t.err,
        _ => t.fg_3,
    };
    let meta = match (tool.elapsed, tool.started) {
        (Some(d), _) if d.as_millis() >= 1000 => Some(s.duration(d)),
        (None, Some(started)) if running => Some(s.duration(app.now.saturating_duration_since(started))),
        _ => None,
    };
    // What runs shows what it writes, and a command that failed shows why, without being asked.
    let by_itself =
        (running && matches!(kind, Kind::Bash | Kind::Task)) || (tool.state == ToolState::Failed && kind == Kind::Bash);
    let open = app.chat.open.contains(&(i, j)) != by_itself;
    let details = tool_details(app, tool);
    let press = details.is_some().then_some((Msg::Open(i, j), open));
    let mut head = line(t, (symbol, color), parts, meta, press);
    // A call that runs long can be stopped alone: its result is an error, and the run goes on.
    // Not one a subagent's tool made: the CLI cannot stop those alone.
    if running && app.running() && app.chat.newer && tool.parent.is_none() {
        head = iced::widget::row![
            container(head).width(Length::Fill),
            super::tip(
                t,
                iced::widget::button(icon(Icon::Stop, 12.0, t.fg_3))
                    .padding(4)
                    .style(t.ghost(t.fg_3, false))
                    .on_press(Msg::StopCall(tool.id.clone())),
                s.w().stop_call.to_owned(),
                iced::widget::tooltip::Position::Left,
            ),
        ]
        .align_y(iced::Alignment::Center)
        .into();
    }
    let row: El<'a> = match details {
        Some(details) if open => column![head, under(details)].into(),
        _ => head,
    };
    // A call another call made sits under it.
    if tool.parent.is_some() {
        container(row).padding(Padding::ZERO.left(INDENT - 8.0)).into()
    } else {
        row
    }
}

/// What a call's row opens to. `None` when there is nothing more to show than the row says.
fn tool_details<'a>(app: &'a App, tool: &'a Tool) -> Option<El<'a>> {
    let (t, s) = (app.t, app.s);
    let mut parts: Vec<El<'a>> = Vec::new();
    let mut push = |el: El<'a>| parts.push(el);
    if let Some(note) = &tool.repaired {
        push(
            text(format!("{}{}{note}", s.repaired, if s.ja { "：" } else { ": " }))
                .size(12)
                .color(t.warn)
                .into(),
        );
    }
    if let Some((allowed, reason)) = tool.judged.as_ref().filter(|(_, r)| !r.is_empty()) {
        let w = s.w();
        let who = if *allowed { w.judged_allow } else { w.judged_ask };
        push(
            text(format!("{who}{}{reason}", if s.ja { "：" } else { ": " }))
                .size(12)
                .color(t.fg_2)
                .into(),
        );
    }
    let said = tool.shown_output();
    match tool.kind() {
        Kind::Write | Kind::Edit => {
            // A change that was not made says why; one that was made shows itself.
            if tool.state != ToolState::Done && !tool.output.is_empty() {
                push(output_block(t, s, &tool.output, 12));
            }
            if let Some(change) = &tool.change
                && !change.diff.is_empty()
            {
                push(diff_view(t, s, &change.diff, 60));
            }
        }
        Kind::Bash => {
            if let Some(command) = tool.arg("command") {
                push(
                    container(
                        text(format!("$ {command}"))
                            .size(12)
                            .font(mono())
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
            let silent = tool.state == ToolState::Running && said.is_empty();
            if tool.state != ToolState::Waiting && !silent {
                push(output_block(t, s, said, 30));
            }
            if tool.arg("command").is_some() {
                push(command_copies(app, tool));
            }
        }
        Kind::Task => {
            if let Some(prompt) = tool.arg("prompt") {
                push(text(prompt).size(12.5).color(t.fg_2).into());
            }
            // The subagent's own calls: all the run says of them is one line each.
            let mut calls = column![].spacing(2);
            for call in tool.stream.lines().filter(|l| !l.trim().is_empty()) {
                calls = calls.push(
                    row![icon(Icon::ChevronRight, 10.0, t.fg_3), code(clip(call, 80), t.fg_2)]
                        .spacing(6)
                        .align_y(Alignment::Center),
                );
            }
            if !tool.stream.trim().is_empty() {
                push(calls.into());
            }
            if !tool.output.is_empty() {
                push(output_block(t, s, &tool.output, 30));
            }
        }
        Kind::Plan => {
            if let Some(plan) = &tool.plan {
                push(todo_list(t, &plan.todos));
            }
        }
        Kind::Read => {
            if !said.is_empty() {
                push(output_block(t, s, said, 20));
            }
        }
        Kind::Other => {
            if tool.input.as_object().is_some_and(|o| !o.is_empty()) {
                let args = serde_json::to_string_pretty(&tool.input).unwrap_or_default();
                push(output_block(t, s, &args, 20));
            }
            if !said.is_empty() {
                push(output_block(t, s, said, 20));
            }
        }
    }
    // The pictures its result holds, small, each larger on a click.
    if !tool.images.is_empty() {
        let mut line = row![].spacing(6);
        for path in &tool.images {
            line = line.push(
                button(
                    container(
                        iced::widget::image(iced::widget::image::Handle::from_path(path))
                            .height(120)
                            .content_fit(iced::ContentFit::Contain),
                    )
                    .max_width(260)
                    .style(t.boxed(t.raised, t.line, RADIUS)),
                )
                .padding(0)
                .style(t.ghost(t.fg, false))
                .on_press(Msg::ViewImage(path.clone())),
            );
        }
        push(line.wrap().into());
    }
    (!parts.is_empty()).then(|| Column::with_children(parts).spacing(6).into())
}

/// Copies a command, and once it finished all it wrote: more than the lines shown of it.
pub(crate) fn command_copies<'a>(app: &App, tool: &Tool) -> El<'a> {
    let s = app.s;
    let mut buttons = row![copy_button(app, Clip::Command(tool.id.clone()), s.copy_cmd)].spacing(4);
    if tool.finished() && !tool.shown_output().trim().is_empty() {
        buttons = buttons.push(copy_button(app, Clip::Output(tool.id.clone()), s.copy_out));
    }
    buttons.into()
}

/// A plan's steps with their state.
pub(crate) fn todo_list<'a>(t: Tokens, todos: &'a [Todo]) -> El<'a> {
    let mut col = column![].spacing(5);
    for todo in todos {
        let (mark, color, ink) = match todo.status {
            TodoStatus::Completed => (Icon::Check, t.accent, t.fg_3),
            TodoStatus::InProgress => (Icon::Half, t.warn, t.fg),
            TodoStatus::Pending => (Icon::Circle, t.fg_3, t.fg_2),
        };
        col = col.push(
            row![
                container(icon(mark, 13.0, color)).padding(Padding::ZERO.top(2.0)),
                text(todo.content.as_str()).size(13).color(ink)
            ]
            .spacing(8),
        );
    }
    col.into()
}

fn approval_row<'a>(app: &'a App, approval: &'a Approval) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let (label, color) = match &approval.answer {
        None => (s.waiting_appr, t.warn),
        Some(Answer::Allowed) => (s.you_allowed, t.fg_3),
        Some(Answer::Always) => (s.you_always, t.fg_3),
        Some(Answer::Denied(_)) => (s.you_denied, t.fg_3),
        Some(Answer::Cancelled) => (s.appr_cancelled, t.fg_3),
    };
    let subject = subject(app, approval);
    let mut parts = vec![say(label, color), code(clip(&subject, 56), t.fg_3)];
    if let Some(Answer::Denied(why)) = &approval.answer
        && !why.is_empty()
    {
        parts.push(say(
            if s.ja {
                format!("「{}」", clip(why, 40))
            } else {
                format!("\"{}\"", clip(why, 40))
            },
            t.fg_2,
        ));
    }
    line(t, (Icon::Shield, color), parts, None, None)
}

/// What an approval is about, in a few words: the command, the file, else the tool.
pub(crate) fn subject(app: &App, approval: &Approval) -> String {
    let arg = |key: &str| approval.args.get(key).and_then(|v| v.as_str());
    match (arg("command"), arg("path")) {
        (Some(command), _) => first_line(command).to_owned(),
        (None, Some(path)) => format!("{} {}", approval.tool, short_path(path, &app.cwd)),
        _ => approval.tool.clone(),
    }
}

fn note_row<'a>(app: &'a App, i: usize, j: usize, note: &'a Note) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let open = app.chat.open.contains(&(i, j));
    let quiet = |mark: Icon, label: &'static str| line(t, (mark, t.fg_3), vec![say(label, t.fg_3)], None, None);
    let folded = |mark: Icon, label: String, body: &'a str| -> El<'a> {
        let head = line(
            t,
            (mark, t.fg_3),
            vec![say(label, t.fg_3)],
            None,
            Some((Msg::Open(i, j), open)),
        );
        if open {
            column![head, under(text(body).size(12.5).color(t.fg_2))].into()
        } else {
            head
        }
    };
    match note {
        Note::Harness(body) => folded(Icon::Leaf, s.note_harness.to_owned(), body),
        Note::Task { head, report } => folded(Icon::Bot, format!("{} · {}", s.note_task, clip(head, 60)), report),
        // All of it can be copied: what went wrong is often pasted somewhere to be looked into.
        Note::Error(body) => container(
            row![
                container(icon(Icon::Alert, 14.0, t.err)).padding(Padding::ZERO.top(1.0)),
                text(body.as_str())
                    .size(13)
                    .color(t.err)
                    .width(Length::Fill)
                    .wrapping(text::Wrapping::WordOrGlyph),
                copy_button(app, Clip::Step(i, j), s.copy),
            ]
            .spacing(8),
        )
        .width(Length::Fill)
        .padding([8, 10])
        .style(t.boxed(t.err_wash, t.err_line, RADIUS))
        .into(),
        Note::ContextFull => line(t, (Icon::Alert, t.warn), vec![say(s.context_full, t.warn)], None, None),
        Note::Compacted(None) => quiet(Icon::Spark, s.compacted),
        Note::Compacted(Some(summary)) => folded(Icon::Spark, s.compacted.to_owned(), summary),
        Note::Retrying => quiet(Icon::Spark, s.discarded),
        Note::Notice(body) => container(
            row![
                container(icon(Icon::Alert, 14.0, t.warn)).padding(Padding::ZERO.top(1.0)),
                text(body.as_str())
                    .size(13)
                    .color(t.fg)
                    .width(Length::Fill)
                    .wrapping(text::Wrapping::WordOrGlyph),
            ]
            .spacing(8),
        )
        .width(Length::Fill)
        .padding([8, 10])
        .style(t.boxed(t.warn_wash, t.warn_line, RADIUS))
        .into(),
        Note::Text { title, text: body } => {
            let w = s.w();
            let label = if title.trim().is_empty() {
                w.plugin_text.to_owned()
            } else {
                format!("{} · {}", w.plugin_text, clip(title, 60))
            };
            folded(Icon::File, label, body)
        }
    }
}

/// A reply, as the Markdown it is written in. With `copy` its code blocks can be copied.
pub(crate) fn reply<'a>(app: &'a App, md: &'a Markdown, copy: bool) -> El<'a> {
    let t = app.t;
    let mut style = markdown::Style::from_palette(t.palette());
    style.inline_code_color = t.fg;
    style.inline_code_highlight = iced::advanced::text::Highlight {
        background: t.inset.into(),
        border: border::rounded(4.0),
    };
    style.link_color = t.link;
    // Named, like the rest of the window: the style's own default is the sans-serif family, which
    // is not the window's font and takes kanji and kana of one bold word from different fonts.
    style.font = ui();
    style.inline_code_font = mono();
    style.code_block_font = mono();
    let mut settings = markdown::Settings::with_text_size(14, style);
    settings.code_size = 12.5.into();
    markdown::view_with(md.content.items(), settings, &Reply { app, copy })
}

/// Draws a reply as iced does, except its code blocks.
struct Reply<'b> {
    app: &'b App,
    copy: bool,
}

impl<'a> markdown::Viewer<'a, Msg> for Reply<'a> {
    fn on_link_click(url: markdown::Uri) -> Msg {
        Msg::Link(url)
    }

    /// As iced draws one, with what the search inside the conversation found marked. One with
    /// links offers them on a right click: iced does not say which of them is under the pointer.
    fn paragraph(&self, settings: markdown::Settings, text: &markdown::Text) -> El<'a> {
        let spans = text.spans(settings.style);
        let drawn: El<'a> = match self.app.find_needle() {
            Some(needle) => iced::widget::rich_text(found_marked(&spans, &needle, found_mark(self.app.t)))
                .size(settings.text_size)
                .on_link_click(Self::on_link_click)
                .into(),
            None => markdown::paragraph(settings, text, Self::on_link_click),
        };
        let mut links: Vec<String> = Vec::new();
        for link in spans.iter().filter_map(|s| s.link.clone()) {
            if !links.contains(&link) {
                links.push(link);
            }
        }
        if links.is_empty() {
            return drawn;
        }
        iced::widget::mouse_area(drawn)
            .on_right_press(Msg::OpenMenu(crate::rows::MenuFor::Links(links)))
            .into()
    }

    /// A picture that is a file here, shown, and larger on a click; any other, its words.
    fn image(
        &self,
        settings: markdown::Settings,
        url: &'a markdown::Uri,
        _title: &'a str,
        alt: &markdown::Text,
    ) -> El<'a> {
        let t = self.app.t;
        match crate::files::local_image(url, &self.app.cwd) {
            Some(path) => button(
                container(
                    iced::widget::image(iced::widget::image::Handle::from_path(&path))
                        .content_fit(iced::ContentFit::ScaleDown),
                )
                .max_width(560)
                .max_height(320)
                .style(t.boxed(t.raised, t.line, RADIUS)),
            )
            .padding(0)
            .style(t.ghost(t.fg, false))
            .on_press(Msg::ViewImage(path))
            .into(),
            None => container(iced::widget::rich_text(alt.spans(settings.style)).on_link_click(Self::on_link_click))
                .padding(settings.spacing.0)
                .style(t.boxed(t.inset, t.line, RADIUS))
                .into(),
        }
    }

    /// A quote; a GitHub alert, `> [!WARNING]`, under a head that names it in its color.
    fn quote(&self, settings: markdown::Settings, contents: &'a [markdown::Item]) -> El<'a> {
        let t = self.app.t;
        let kind = match contents.first() {
            Some(markdown::Item::Paragraph(first)) => {
                let said: String = first.spans(settings.style).iter().map(|s| s.text.as_ref()).collect();
                said.strip_prefix(crate::figure::ALERT).map(str::to_owned)
            }
            _ => None,
        };
        let Some(kind) = kind else {
            return markdown::quote(self, settings, contents);
        };
        let w = self.app.s.w();
        let (mark, label, color) = match kind.as_str() {
            "NOTE" => (Icon::Bot, w.alert_note, t.accent),
            "TIP" => (Icon::Bulb, w.alert_tip, t.accent),
            "IMPORTANT" => (Icon::Spark, w.alert_important, t.accent),
            "WARNING" => (Icon::Alert, w.alert_warning, t.warn),
            _ => (Icon::Alert, w.alert_caution, t.err),
        };
        let body = Column::with_children(
            contents[1..]
                .iter()
                .enumerate()
                .map(|(i, item)| markdown::item(self, settings, item, i)),
        )
        .spacing(settings.spacing.0);
        let head = row![
            icon(mark, 14.0, color),
            text(label).size(13.5).font(super::bold()).color(color)
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        row![
            rule::vertical(3).style(t.rule(color)),
            column![head, body].spacing(settings.spacing.0 * 0.6)
        ]
        .height(Length::Shrink)
        .spacing(settings.spacing.0)
        .into()
    }

    /// The code as it was written, in the monospaced font. iced's own code block draws each line
    /// in the font of the text around it unless it highlights the code, which this window does not.
    /// A head over it names its language and copies it.
    fn code_block(
        &self,
        settings: markdown::Settings,
        language: Option<&'a str>,
        code: &'a str,
        _lines: &'a [markdown::Text],
    ) -> El<'a> {
        let t = self.app.t;
        let code = code.trim_end();
        // Math and Mermaid are drawn; what cannot be, as while it streams, is shown as written.
        let lang = language.map(str::to_ascii_lowercase);
        let math = lang.as_deref().is_some_and(|l| crate::figure::MATH.contains(&l));
        if (math || lang.as_deref() == Some("mermaid"))
            && let Ok(figure) = crate::figure::figure(math, code)
        {
            return self.figure(language, code, math, figure);
        }
        // Colored by its language, when it names one the grammars know.
        let shown: El<'a> = match language.filter(|l| !l.is_empty()) {
            Some(language) => {
                let colored = crate::files::colored(code, language, t.dark);
                let mut spans: Vec<iced::widget::text::Span<'a, (), iced::Font>> = Vec::new();
                for (i, line) in code.lines().enumerate() {
                    if i > 0 {
                        spans.push(iced::widget::span("\n"));
                    }
                    for (range, color) in colored.get(i).into_iter().flatten() {
                        spans.push(iced::widget::span(&line[range.clone()]).color(color.unwrap_or(t.fg)));
                    }
                }
                iced::widget::rich_text(spans)
                    .size(settings.code_size)
                    .font(mono())
                    .wrapping(text::Wrapping::None)
                    .into()
            }
            None => text(code)
                .size(settings.code_size)
                .font(mono())
                .color(t.fg)
                .wrapping(text::Wrapping::None)
                .into(),
        };
        let body = scrollable(container(shown).padding([8, 10]))
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(6).scroller_width(6).margin(1),
            ))
            .style(t.scroll());
        let mut block = column![];
        if self.copy && !code.is_empty() {
            block = block.push(
                row![
                    text(language.unwrap_or_default())
                        .size(11.5)
                        .font(mono())
                        .color(t.fg_3)
                        .wrapping(text::Wrapping::None),
                    Space::new().width(Length::Fill),
                    copy_button(self.app, Clip::Text(code.to_owned()), self.app.s.copy),
                ]
                .align_y(Alignment::Center)
                .padding(Padding {
                    top: 2.0,
                    right: 2.0,
                    bottom: 0.0,
                    left: 10.0,
                }),
            );
        }
        container(block.push(body))
            .width(Length::Fill)
            .style(t.boxed(t.inset, t.inset, RADIUS))
            .into()
    }
}

impl Reply<'_> {
    /// Math, in the color of the text around it; a diagram, on the light ground it was drawn for.
    /// Wider than the reply, it scrolls sideways, as code does.
    fn figure<'a>(
        &self,
        language: Option<&'a str>,
        code: &'a str,
        math: bool,
        figure: crate::figure::Figure,
    ) -> El<'a> {
        let t = self.app.t;
        let picture = iced::widget::svg(figure.handle)
            .width(Length::Fixed(figure.width))
            .height(Length::Fixed(figure.height));
        let picture: El<'a> = if math {
            let fg = t.fg;
            picture
                .style(move |_, _| iced::widget::svg::Style { color: Some(fg) })
                .into()
        } else {
            container(picture)
                .padding(8)
                .style(t.boxed(Color::WHITE, t.line, RADIUS))
                .into()
        };
        let body = scrollable(container(picture).padding([6, 10]))
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(6).scroller_width(6).margin(1),
            ))
            .style(t.scroll());
        let mut block = column![];
        if self.copy && !math {
            block = block.push(
                row![
                    text(language.unwrap_or_default())
                        .size(11.5)
                        .font(mono())
                        .color(t.fg_3)
                        .wrapping(text::Wrapping::None),
                    Space::new().width(Length::Fill),
                    copy_button(self.app, Clip::Text(code.to_owned()), self.app.s.copy),
                ]
                .align_y(Alignment::Center)
                .padding(Padding {
                    top: 2.0,
                    right: 2.0,
                    bottom: 0.0,
                    left: 10.0,
                }),
            );
        }
        let block = block.push(body).width(Length::Fill);
        if math {
            block.into()
        } else {
            container(block)
                .width(Length::Fill)
                .style(t.boxed(t.inset, t.inset, RADIUS))
                .into()
        }
    }
}

/// How a run ended: the quiet line of one that finished, or a card that says why it did not. The
/// reply's copy button goes at the end of the line, or over the card.
fn ending<'a>(app: &'a App, ex: &'a Exchange, end: &'a End, last: bool, copy: Option<El<'a>>) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let files: Vec<&str> = {
        let mut seen: Vec<&str> = ex.changes().map(|(_, c)| c.path.as_str()).collect();
        seen.sort_unstable();
        seen.dedup();
        seen
    };
    let (added, removed) = ex
        .changes()
        .fold((0, 0), |(a, r), (_, c)| (a + c.diff.added, r + c.diff.removed));

    if end.cause == StopCause::Done {
        let mut foot = row![icon(Icon::Check, 13.0, t.accent)]
            .spacing(8)
            .align_y(Alignment::Center);
        if let Some(d) = ex.elapsed {
            foot = foot.push(text(fill(s.done_in, "d", s.duration(d))).size(12).color(t.fg_3));
        }
        if !files.is_empty() {
            foot = foot
                .push(text("·").size(12).color(t.line_2))
                .push(text(fill(s.files_changed, "n", files.len())).size(12).color(t.fg_2))
                .push(plus_minus(t, added, removed))
                .push(
                    button(text(s.review).size(12).color(t.accent))
                        .padding([2, 8])
                        .style(t.ghost(t.accent, false))
                        .on_press(Msg::Tab(Tab::Changes)),
                );
        }
        if let Some(copy) = copy {
            foot = foot.push(Space::new().width(Length::Fill)).push(copy);
        }
        return container(foot).padding(Padding::ZERO.left(8.0)).into();
    }

    let (mark, title, fill_color, line_color, ink) = match end.cause {
        StopCause::Aborted => (Icon::Stop, s.end_aborted.to_owned(), t.inset, t.line_2, t.fg),
        StopCause::Error => (Icon::Alert, s.end_error.to_owned(), t.err_wash, t.err_line, t.err),
        StopCause::MaxTurns => (Icon::Alert, s.end_max.to_owned(), t.warn_wash, t.warn_line, t.warn),
        StopCause::Stopped => (
            Icon::Shield,
            fill(s.end_plugin, "p", end.plugin.as_deref().unwrap_or("plugin")),
            t.warn_wash,
            t.warn_line,
            t.warn,
        ),
        StopCause::Refusal => (Icon::Alert, s.end_refusal.to_owned(), t.warn_wash, t.warn_line, t.warn),
        StopCause::ContextLimit => (Icon::Alert, s.end_context.to_owned(), t.warn_wash, t.warn_line, t.warn),
        _ => (Icon::Alert, s.end_other.to_owned(), t.inset, t.line_2, t.fg),
    };
    let mut body = column![
        row![icon(mark, 14.0, ink), text(title).size(13.5).font(bold()).color(ink)]
            .spacing(8)
            .align_y(Alignment::Center)
    ]
    .spacing(6);
    if let Some(reason) = &end.reason {
        body = body.push(
            text(reason.as_str())
                .size(12.5)
                .font(mono())
                .color(t.fg)
                .wrapping(text::Wrapping::WordOrGlyph),
        );
    }
    let aside = match end.cause {
        StopCause::Aborted if files.is_empty() => Some(s.end_aborted_p0.to_owned()),
        StopCause::Aborted => Some(fill(s.end_aborted_p, "n", files.len())),
        StopCause::MaxTurns | StopCause::Stopped => Some(s.end_saved.to_owned()),
        _ => None,
    };
    if let Some(aside) = aside {
        body = body.push(text(aside).size(12.5).color(t.fg_2));
    }
    // Only where the conversation stands: once something was sent after it, it has moved on.
    if last && !app.busy() && app.chat.next.is_none() && end.cause != StopCause::Refusal {
        body = body.push(
            container(
                button(text(s.continue_btn).size(12.5))
                    .padding([5, 12])
                    .style(t.outline(t.fg))
                    .on_press(Msg::Continue),
            )
            .padding(Padding::ZERO.top(4.0)),
        );
    }
    let card = container(body)
        .width(Length::Fill)
        .padding([10, 12])
        .style(t.boxed(fill_color, line_color, RADIUS_L));
    match copy {
        Some(copy) => column![copy, card].spacing(10).into(),
        None => card.into(),
    }
}
