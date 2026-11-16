//! Drawing. Everything here turns the window's state into widgets and changes nothing.
//!
//! The window, left to right: the folder's conversations, the conversation with the approval card
//! and the input under it, and the inspector (changes, plan, commands, usage). A status bar runs
//! along the bottom.

mod chords;
mod dock;
mod inspector;
mod overlay;
mod sidebar;
mod timeline;

use std::path::Path;

use iced::font::Weight;
use iced::widget::{Space, button, column, container, mouse_area, responsive, row, rule, stack, text, tooltip};
use iced::{Alignment, Color, Element, Font, Length, mouse};

use crate::app::{App, Clip, Edge, Msg, Status, Tab};
use crate::diff::{Diff, Sign};
use crate::i18n::{Strings, compact, fill};
use crate::icons::{Icon, icon};
use crate::theme::{RADIUS, Tokens};

pub(crate) type El<'a> = Element<'a, Msg>;

/// The font of the window's words. On macOS a font by name, so that Latin and Japanese letters are
/// of one design and both have a bold: iced's own choice there sets the two in different fonts and
/// leaves Latin letters light in a bold line. Elsewhere iced's choice.
pub(crate) const UI: Font = if cfg!(target_os = "macos") {
    Font::with_name("Hiragino Sans")
} else {
    Font::DEFAULT
};
pub(crate) const BOLD: Font = Font {
    weight: Weight::Semibold,
    ..UI
};
/// The font of code, paths and output: a monospaced font by name where one is always there.
///
/// `Font::MONOSPACE` is not usable on macOS: what the monospaced font lacks, cosmic-text 0.15 then
/// takes from another monospaced font, and for kanji that is the system's GB18030 Bitmap, whose
/// letters have no width it can read. They come out blank and endlessly wide, and a debug build
/// panics over where to put them. A font named outright is helped out by the usual fonts.
pub(crate) const MONO: Font = if cfg!(target_os = "macos") {
    Font::with_name("Menlo")
} else if cfg!(target_os = "windows") {
    Font::with_name("Consolas")
} else {
    Font::MONOSPACE
};

/// The conversation and the input keep to this width, centred.
const COL_W: f32 = 760.0;
/// How wide a panel's edge is to the pointer: wider than its line, so as to be taken.
const GRIP: f32 = 7.0;

/// How wide the conversation is drawn, between the panels that are shown.
pub(crate) fn room(app: &App) -> f32 {
    let side = if app.shows_side() { app.side_width() + 1.0 } else { 0.0 };
    let insp = if app.insp { app.insp_width() + 1.0 } else { 0.0 };
    app.width - side - insp
}

/// The key that makes a chord: ⌘ on macOS, Ctrl elsewhere.
pub(crate) const MOD: &str = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl+" };

pub fn window(app: &App) -> El<'_> {
    let t = app.t;
    let mut body = row![];
    if app.shows_side() {
        body = body.push(sidebar::sidebar(app)).push(vline(t));
    }
    let mut middle = column![header(app), hline(t)];
    if app.find_in.is_some() {
        middle = middle.push(find_bar(app)).push(hline(t));
    }
    body = body.push(
        middle
            .push(timeline::timeline(app))
            .push(dock::dock(app))
            .width(Length::Fill)
            .height(Length::Fill),
    );
    if app.insp {
        body = body.push(vline(t)).push(inspector::inspector(app));
    }
    // The panels' edges lie over their lines. While one is dragged, a layer over everything keeps
    // the pointer's shape, and nothing under it reacts to the pointer passing; the pointer itself
    // is followed by `app::dragging`, out of the window too.
    let mut panes = stack![body.height(Length::Fill), edges(app)]
        .width(Length::Fill)
        .height(Length::Fill);
    if app.drag.is_some() {
        panes = panes.push(
            mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
                .interaction(mouse::Interaction::ResizingHorizontally),
        );
    }
    let window = container(column![panes, hline(t), status_bar(app)]).style(t.fill(t.bg));
    overlay::over(app, chords::chords(window))
}

/// Where the side panels' inner edges are taken to be dragged: a strip over each one's line, with
/// nothing in between. Drawn always, so that what is under it keeps its place in the window's tree.
fn edges(app: &App) -> El<'_> {
    let mut strip = row![];
    if app.shows_side() {
        // The line is at the list's width; its grip starts a little before it.
        strip = strip
            .push(Space::new().width(app.side_width() - 3.0))
            .push(edge(app, Edge::Side));
    }
    strip = strip.push(Space::new().width(Length::Fill));
    if app.insp {
        // Clear of the conversation's scrollbar, which ends just before the inspector's line.
        strip = strip
            .push(edge(app, Edge::Inspector))
            .push(Space::new().width(app.insp_width() + 1.0 + 2.0 - GRIP));
    }
    strip.height(Length::Fill).into()
}

/// A panel's edge: dragged, it sets the panel's width; double-clicked, the width it opened at. It
/// lights up under the pointer and while it is dragged.
fn edge(app: &App, which: Edge) -> El<'_> {
    let t = app.t;
    let lit = app.drag == Some(which) || app.edge_hover == Some(which);
    let line: El<'_> = if lit {
        container(Space::new())
            .width(3)
            .height(Length::Fill)
            .style(t.fill(t.accent))
            .into()
    } else {
        Space::new().into()
    };
    // Let go here too: a click quick enough is let go before anything follows the drag.
    mouse_area(container(line).center_x(GRIP).height(Length::Fill))
        .interaction(mouse::Interaction::ResizingHorizontally)
        .on_press(Msg::Grab(which))
        .on_release(Msg::Drop)
        .on_double_click(Msg::ResetWidth(which))
        .on_enter(Msg::HoverEdge(Some(which)))
        .on_exit(Msg::HoverEdge(None))
        .into()
}

fn header(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let changed = app.chat.transcript.changes().count();
    let mut inspector = row![icon(Icon::Panel, 16.0, if app.insp { t.accent } else { t.fg_2 })]
        .spacing(6)
        .align_y(Alignment::Center);
    if changed > 0 && !app.insp {
        inspector = inspector.push(tag(t, changed.to_string(), t.accent_wash, t.accent));
    }
    container(
        row![
            tip(
                t,
                button(icon(
                    Icon::Sidebar,
                    16.0,
                    if app.shows_side() { t.accent } else { t.fg_2 }
                ))
                .padding(6)
                .style(t.ghost(t.fg_2, false))
                .on_press(Msg::ToggleSide),
                format!("{}  {MOD}B", s.a_side),
                tooltip::Position::Bottom,
            ),
            // The title is cut where the header runs out of room, so the state and the buttons stay.
            responsive(move |room| {
                let width = (room.width - pill_width(app)).max(0.0);
                container(
                    row![container(title(app)).max_width(width).clip(true), pill(app)]
                        .spacing(10)
                        .align_y(Alignment::Center),
                )
                .height(Length::Fill)
                .align_y(Alignment::Center)
                .into()
            }),
            tip(
                t,
                button(icon(Icon::List, 16.0, t.fg_2))
                    .padding(6)
                    .style(t.ghost(t.fg_2, false))
                    .on_press(Msg::Show(Some(crate::app::Overlay::palette()))),
                format!("{}  {MOD}⇧P", s.c_palette),
                tooltip::Position::Bottom,
            ),
            tip(
                t,
                button(icon(Icon::Gear, 16.0, t.fg_2))
                    .padding(6)
                    .style(t.ghost(t.fg_2, false))
                    .on_press(Msg::Show(Some(crate::app::Overlay::Settings))),
                format!("{}  {MOD},", s.settings),
                tooltip::Position::Bottom,
            ),
            tip(
                t,
                button(inspector)
                    .padding(6)
                    .style(t.ghost(t.fg_2, false))
                    .on_press(Msg::ToggleInspector),
                format!("{}  {MOD}I", s.a_insp),
                tooltip::Position::Bottom,
            ),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .padding([0, 10])
    .height(46)
    .align_y(Alignment::Center)
    .style(t.fill(t.surface))
    .into()
}

/// The search inside the conversation: what it looks for, how many exchanges have it, and the way
/// to put it away.
fn find_bar(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let typed = app.find_in.as_deref().unwrap_or_default();
    let needle = typed.trim().to_lowercase();
    let found = if needle.is_empty() {
        None
    } else {
        Some(
            app.chat
                .transcript
                .exchanges
                .iter()
                .filter(|ex| timeline::has(ex, &needle))
                .count(),
        )
    };
    let mut line = row![
        icon(Icon::Search, 14.0, t.fg_3),
        iced::widget::text_input(s.find_in_ph, typed)
            .id(crate::app::FIND_IN)
            .on_input(Msg::FindInText)
            .on_submit(Msg::FocusInput)
            .size(13)
            .padding(0)
            .style(t.input_bare()),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if let Some(n) = found {
        line = line.push(
            text(fill(s.found_n, "n", n))
                .size(12)
                .color(if n == 0 { t.err } else { t.fg_3 }),
        );
    }
    line = line.push(
        button(icon(Icon::Close, 13.0, t.fg_3))
            .padding(4)
            .style(t.ghost(t.fg_3, false))
            .on_press(Msg::FindInClose),
    );
    container(line).padding([6, 14]).style(t.fill(t.surface)).into()
}

/// The state of the conversation, next to its title.
/// The conversation's title: its first message, cut short. A title that had to be cut says all of
/// itself under the pointer, and so does one long enough to be cut by the header's edge.
fn title(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let title_text = |label: String| {
        text(label)
            .size(14)
            .font(BOLD)
            .color(t.fg)
            .wrapping(text::Wrapping::None)
    };
    match app.chat.transcript.title() {
        Some(prompt) => {
            let shown = clip(first_line(prompt), 56);
            if shown == prompt.trim() && shown.chars().count() <= 24 {
                title_text(shown).into()
            } else {
                tip(
                    t,
                    title_text(shown),
                    clip(prompt.trim(), 600),
                    tooltip::Position::Bottom,
                )
            }
        }
        None => title_text(s.new_session.to_owned()).into(),
    }
}

/// What the state beside the title says, its colours, and whether it has a dot: nothing for a
/// conversation with nothing in it.
fn state(app: &App) -> Option<(&'static str, Color, Color, bool)> {
    let (t, s) = (app.t, app.s);
    Some(match app.status() {
        Status::Blank => return None,
        Status::Running => (s.st_running, t.accent_wash, t.accent, true),
        Status::Waiting => (s.st_waiting, t.warn_wash, t.warn, true),
        Status::Done => (s.st_done, t.inset, t.fg_2, false),
        Status::Aborted => (s.st_aborted, t.inset, t.fg_2, false),
        Status::Error => (s.st_error, t.err_wash, t.err, false),
        Status::MaxTurns => (s.st_max, t.warn_wash, t.warn, false),
        Status::Plugin => (s.st_plugin, t.warn_wash, t.warn, false),
        Status::Loaded => (s.st_loaded, t.inset, t.fg_3, false),
    })
}

/// About how wide the state beside the title is, with the space before it.
fn pill_width(app: &App) -> f32 {
    let Some((label, _, _, dot)) = state(app) else {
        return 0.0;
    };
    let letters: f32 = label.chars().map(|c| if c.is_ascii() { 7.0 } else { 12.5 }).sum();
    letters + 16.0 + if dot { 15.0 } else { 0.0 } + 10.0
}

fn pill(app: &App) -> El<'_> {
    let Some((label, fill, color, dot)) = state(app) else {
        return Space::new().into();
    };
    let mut content = row![].spacing(5).align_y(Alignment::Center);
    if dot {
        content = content.push(icon(Icon::Dot, 10.0, color));
    }
    container(content.push(text(label).size(12).color(color)))
        .padding([2, 8])
        .style(move |_| container::Style {
            background: Some(fill.into()),
            border: iced::border::rounded(999.0),
            ..container::Style::default()
        })
        .into()
}

fn status_bar(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    // On one line: what does not fit on the left is cut at the bar's middle, not wrapped.
    let item = |label: String| text(label).size(11.5).color(t.fg_3).wrapping(text::Wrapping::None);
    let dot = || text("·").size(11.5).color(t.line_2);
    let cwd = app.cwd.to_string_lossy();
    let shown = clip_start(&cwd, 46);
    let folder: El<'_> = if shown == cwd {
        item(shown).font(MONO).into()
    } else {
        tip(t, item(shown).font(MONO), cwd.into_owned(), tooltip::Position::Top)
    };
    let mut left = row![icon(Icon::Folder, 12.0, t.fg_3), folder]
        .spacing(6)
        .align_y(Alignment::Center);
    // The branch checked out; a click copies its name.
    if let Some(branch) = &app.git.branch {
        let label = if app.copied == Some(Clip::Text(branch.clone())) {
            item(s.copied.to_owned()).color(t.accent)
        } else {
            item(clip(branch, 28)).font(MONO)
        };
        left = left.push(dot()).push(tip(
            t,
            button(
                row![icon(Icon::Branch, 11.0, t.fg_3), label]
                    .spacing(4)
                    .align_y(Alignment::Center),
            )
            .padding([1, 4])
            .style(t.ghost(t.fg_3, false))
            .on_press(Msg::Copy(Clip::Text(branch.clone()))),
            format!("{branch}\n{}", s.copy_branch),
            tooltip::Position::Top,
        ));
    }
    if let Some(id) = &app.chat.session {
        // The start of the id, as `sasacode -r` takes it; a click copies all of it.
        let label = if app.copied == Some(Clip::Session) {
            item(s.copied.to_owned()).color(t.accent)
        } else {
            item(clip(id, 8).trim_end_matches('…').to_owned()).font(MONO)
        };
        left = left.push(dot()).push(tip(
            t,
            button(label)
                .padding([1, 4])
                .style(t.ghost(t.fg_3, false))
                .on_press(Msg::Copy(Clip::Session)),
            format!("{id}\n{}", s.copy_id),
            tooltip::Position::Top,
        ));
    }
    // The model that answered, else the one asked for; what the config decides is not known here,
    // and is not guessed at.
    let model = match (app.model.trim(), &app.chat.transcript.model) {
        ("", used) => used.clone(),
        (typed, _) => Some(typed.to_owned()),
    };
    if let Some(model) = model {
        left = left.push(dot()).push(item(model).font(MONO));
    }
    if let Some(mode) = app.permission {
        let color = if mode == "auto" { t.err } else { t.fg_3 };
        left = left.push(dot()).push(item(mode.to_owned()).color(color));
    }
    if let Some(effort) = app.effort {
        left = left.push(dot()).push(item(effort.to_owned()));
    }
    // How full the conversation is, as the model last read it, of what it can take.
    let tr = &app.chat.transcript;
    if let Some(window) = tr.context_window.filter(|w| *w > 0 && tr.last_input > 0) {
        let share = tr.last_input as f64 / window as f64;
        let color = if share >= 0.9 {
            t.err
        } else if share >= 0.75 {
            t.warn
        } else {
            t.fg_3
        };
        left = left.push(dot()).push(tip(
            t,
            // Under one percent is not nothing.
            item(s.context.replace(
                "{n}",
                &if share < 0.01 {
                    "<1".to_owned()
                } else {
                    format!("{:.0}", share * 100.0)
                },
            ))
            .color(color),
            format!("{} / {}", compact(tr.last_input), compact(window)),
            tooltip::Position::Top,
        ));
    }
    let u = &app.chat.transcript.usage;
    let usage = format!(
        "in {} · out {} · cache {} · ${:.4}",
        compact(u.input),
        compact(u.output),
        compact(u.cache_read),
        u.cost
    );
    let mut right = row![].spacing(8).align_y(Alignment::Center);
    if let Some(toast) = &app.toast {
        right = right.push(item(clip(toast, 80)).color(t.fg_2));
    }
    container(
        row![
            container(left).width(Length::Fill).clip(true),
            right,
            button(item(usage).font(MONO))
                .padding([1, 6])
                .style(t.ghost(t.fg_3, false))
                .on_press(Msg::Tab(Tab::Usage)),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([3, 12])
    .style(t.fill(t.surface))
    .into()
}

pub(crate) fn vline<'a>(t: Tokens) -> El<'a> {
    rule::vertical(1).style(t.rule(t.line)).into()
}

pub(crate) fn hline<'a>(t: Tokens) -> El<'a> {
    rule::horizontal(1).style(t.rule(t.line)).into()
}

/// A small label on a tinted background.
pub(crate) fn tag<'a>(t: Tokens, label: impl text::IntoFragment<'a>, fill: Color, color: Color) -> El<'a> {
    container(text(label).size(11).color(color).wrapping(text::Wrapping::None))
        .padding([1, 6])
        .style(t.tag(fill, color))
        .into()
}

/// `content` with a hint that shows while the pointer rests on it.
pub(crate) fn tip<'a>(t: Tokens, content: impl Into<El<'a>>, hint: String, position: tooltip::Position) -> El<'a> {
    tooltip(
        content,
        container(text(hint).size(12).color(t.fg))
            .padding([4, 8])
            .max_width(320)
            .style(t.boxed(t.raised, t.line_2, RADIUS)),
        position,
    )
    .gap(6)
    .into()
}

/// A button that copies `clip`, and says for a moment that it did. Its word is always there: not
/// an icon to guess at, nor a hint to hover for.
pub(crate) fn copy_button<'a>(app: &App, clip: Clip, label: &'static str) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let (mark, said, color) = if app.copied.as_ref() == Some(&clip) {
        (Icon::Check, s.copied, t.accent)
    } else {
        (Icon::Copy, label, t.fg_3)
    };
    button(
        row![
            icon(mark, 12.0, color),
            text(said).size(11.5).color(color).wrapping(text::Wrapping::None)
        ]
        .spacing(5)
        .align_y(Alignment::Center),
    )
    .padding([3, 8])
    .style(t.ghost(t.fg_2, false))
    .on_press(Msg::Copy(clip))
    .into()
}

/// `+3 −1`, each in its colour. Nothing for a side that is zero, unless both are.
pub(crate) fn plus_minus<'a>(t: Tokens, added: usize, removed: usize) -> El<'a> {
    let mut out = row![].spacing(6);
    if added > 0 || removed == 0 {
        out = out.push(text(format!("+{added}")).size(12).font(MONO).color(t.add));
    }
    if removed > 0 {
        out = out.push(text(format!("−{removed}")).size(12).font(MONO).color(t.del));
    }
    out.into()
}

/// The lines of a change, at most `max` of them.
pub(crate) fn diff_view<'a>(t: Tokens, s: Strings, diff: &'a Diff, max: usize) -> El<'a> {
    let mut lines = column![];
    for line in diff.lines.iter().take(max) {
        let (mark, color, fill) = match line.sign {
            Sign::Add => ("+", t.add, Some(t.add_wash)),
            Sign::Del => ("−", t.del, Some(t.del_wash)),
            Sign::Context => (" ", t.fg_2, None),
            Sign::Hunk => ("", t.fg_3, None),
        };
        let body = text(line.text.as_str())
            .size(12)
            .font(MONO)
            .color(if line.sign == Sign::Hunk { t.fg_3 } else { t.fg })
            .wrapping(text::Wrapping::WordOrGlyph);
        lines = lines.push(
            container(row![text(mark).size(12).font(MONO).color(color).width(14), body].spacing(2))
                .width(Length::Fill)
                .padding([1, 8])
                .style(move |_| container::Style {
                    background: fill.map(Into::into),
                    ..container::Style::default()
                }),
        );
    }
    let hidden = diff.lines.len().saturating_sub(max) + diff.cut;
    if hidden > 0 {
        lines = lines.push(container(text(fill(s.more_lines, "n", hidden)).size(11.5).color(t.fg_3)).padding([3, 8]));
    }
    container(lines)
        .width(Length::Fill)
        .padding([4, 0])
        .clip(true)
        .style(t.boxed(t.raised, t.line, RADIUS))
        .into()
}

/// Text a program wrote: its last `max` lines, set in.
pub(crate) fn output_block<'a>(t: Tokens, s: Strings, out: &str, max: usize) -> El<'a> {
    let lines: Vec<&str> = out.trim_end().lines().collect();
    let skip = lines.len().saturating_sub(max);
    let mut body = column![].spacing(2);
    if skip > 0 {
        body = body.push(text(fill(s.more_lines, "n", skip)).size(11.5).color(t.fg_3));
    }
    let shown = if lines.is_empty() {
        s.no_output.to_owned()
    } else {
        lines[skip..].join("\n")
    };
    body = body.push(
        text(shown)
            .size(12)
            .font(MONO)
            .color(if lines.is_empty() { t.fg_3 } else { t.fg_2 })
            .wrapping(text::Wrapping::WordOrGlyph),
    );
    container(body)
        .width(Length::Fill)
        .padding([6, 10])
        .style(t.boxed(t.inset, t.inset, RADIUS))
        .into()
}

pub(crate) fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

/// How wide a character is, in half-width cells.
fn cells(c: char) -> usize {
    if c.is_ascii() || ('\u{ff61}'..='\u{ff9f}').contains(&c) {
        1
    } else {
        2
    }
}

/// `s` cut to `max` half-width cells, with `…` when it was cut. A text widget cannot do this itself.
pub(crate) fn clip(s: &str, max: usize) -> String {
    let mut used = 0;
    for (at, c) in s.char_indices() {
        used += cells(c);
        if used > max {
            return format!("{}…", &s[..at]);
        }
    }
    s.to_owned()
}

/// [`clip`] that keeps the end: for paths, whose end says the most.
pub(crate) fn clip_start(s: &str, max: usize) -> String {
    let mut used = 0;
    for (at, c) in s.char_indices().rev() {
        used += cells(c);
        if used > max {
            return format!("…{}", &s[at + c.len_utf8()..]);
        }
    }
    s.to_owned()
}

/// A path as the conversation shows it: relative to the folder when it is inside it.
pub(crate) fn short_path(path: &str, cwd: &Path) -> String {
    let p = Path::new(path);
    match p.strip_prefix(cwd) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().into_owned(),
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_cut_by_how_wide_it_is_drawn() {
        assert_eq!(clip("abcdef", 6), "abcdef");
        assert_eq!(clip("abcdefg", 6), "abcdef…");
        assert_eq!(clip("再試行を指数バックオフにして", 8), "再試行を…");
        assert_eq!(clip_start("/Users/me/src/orders-api", 10), "…orders-api");
        assert_eq!(clip_start("短い", 10), "短い");
        assert_eq!(
            short_path("/work/project/src/a.ts", Path::new("/work/project")),
            "src/a.ts"
        );
        assert_eq!(short_path("src/a.ts", Path::new("/work/project")), "src/a.ts");
        assert_eq!(short_path("/etc/hosts", Path::new("/work/project")), "/etc/hosts");
    }
}
