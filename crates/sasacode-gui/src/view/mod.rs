//! Drawing. Everything here turns the window's state into widgets and changes nothing.
//!
//! The window, left to right: the folder's conversations, the conversation with the approval card
//! and the input under it, and the inspector (changes, plan, commands, usage). A status bar runs
//! along the bottom.

mod dock;
mod inspector;
mod sidebar;
mod timeline;

use std::path::Path;

use iced::font::Weight;
use iced::widget::{Space, button, column, container, row, rule, text, tooltip};
use iced::{Alignment, Color, Element, Font, Length};

use crate::app::{App, Msg, Status, Tab};
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

const SIDE_W: f32 = 244.0;
const INSP_W: f32 = 380.0;
/// The conversation and the input keep to this width, centred.
const COL_W: f32 = 760.0;

/// How wide the inspector is drawn: narrower in a narrow window.
fn insp_w(app: &App) -> f32 {
    if app.width < 900.0 { 320.0 } else { INSP_W }
}

/// How wide the conversation is drawn, between the panels that are shown.
pub(crate) fn room(app: &App) -> f32 {
    let side = if app.shows_side() { SIDE_W + 1.0 } else { 0.0 };
    let insp = if app.insp { insp_w(app) + 1.0 } else { 0.0 };
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
    body = body.push(
        column![header(app), hline(t), timeline::timeline(app), dock::dock(app)]
            .width(Length::Fill)
            .height(Length::Fill),
    );
    if app.insp {
        body = body.push(vline(t)).push(inspector::inspector(app));
    }
    container(column![body.height(Length::Fill), hline(t), status_bar(app)])
        .style(t.fill(t.bg))
        .into()
}

fn header(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let title = match app.transcript.title() {
        Some(prompt) => clip(first_line(prompt), 56),
        None => s.new_session.to_owned(),
    };
    let changed = app.transcript.changes().count();
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
            text(title)
                .size(14)
                .font(BOLD)
                .color(t.fg)
                .wrapping(text::Wrapping::None),
            pill(app),
            Space::new().width(Length::Fill),
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

/// The state of the conversation, next to its title.
fn pill(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let (label, fill, color, dot) = match app.status() {
        Status::Blank => return Space::new().into(),
        Status::Running => (s.st_running, t.accent_wash, t.accent, true),
        Status::Waiting => (s.st_waiting, t.warn_wash, t.warn, true),
        Status::Done => (s.st_done, t.inset, t.fg_2, false),
        Status::Aborted => (s.st_aborted, t.inset, t.fg_2, false),
        Status::Error => (s.st_error, t.err_wash, t.err, false),
        Status::MaxTurns => (s.st_max, t.warn_wash, t.warn, false),
        Status::Plugin => (s.st_plugin, t.warn_wash, t.warn, false),
        Status::Loaded => (s.st_loaded, t.inset, t.fg_3, false),
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
    let t = app.t;
    let item = |label: String| text(label).size(11.5).color(t.fg_3);
    let dot = || text("·").size(11.5).color(t.line_2);
    let mut left = row![
        icon(Icon::Folder, 12.0, t.fg_3),
        item(clip_start(&app.cwd.to_string_lossy(), 46)).font(MONO),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if let Some(id) = &app.session {
        left = left
            .push(dot())
            .push(item(clip(id, 8).trim_end_matches('…').to_owned()).font(MONO));
    }
    // The model that answered, else the one asked for; what the config decides is not known here,
    // and is not guessed at.
    let model = match (app.model.trim(), &app.transcript.model) {
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
    let u = &app.transcript.usage;
    let usage = format!(
        "in {} · out {} · cache {} · ${:.4}",
        compact(u.input),
        compact(u.output),
        compact(u.cache_read),
        u.cost
    );
    container(
        row![
            left,
            Space::new().width(Length::Fill),
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
