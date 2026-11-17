//! Drawing. Everything here turns the window's state into widgets and changes nothing.
//!
//! The window, left to right: the folder's conversations, the conversation with the approval card
//! and the input under it, and the inspector (changes, plan, commands, usage). A status bar runs
//! along the bottom.

mod chords;
pub(crate) mod dock;
mod inspector;
mod menu;
pub(crate) use timeline::has;
pub(crate) mod overlay;
mod sidebar;
mod split;
mod timeline;
mod usage;

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
/// How wide the conversation and the input are drawn at most, as the settings say.
pub(crate) fn col_w(app: &App) -> f32 {
    crate::extras::column_width(app.prefs.more.chat_width)
}
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

/// A shortcut's key as it is bound now, or nothing for one the user unbound.
pub(crate) fn keys_for(act: crate::keys::Act) -> String {
    crate::keys::bound()
        .chord(act)
        .map(crate::keys::Chord::show)
        .unwrap_or_default()
}

pub fn window(app: &App) -> El<'_> {
    let t = app.t;
    // What comes and goes in front of the conversation keeps its place, empty while it is not
    // shown: a part that came in front would make iced take the conversation and the input for new
    // widgets, and the place read up to and the keys would be lost.
    let (side, finding) = (app.shows_side(), app.find_in.is_some());
    let nothing = || -> El<'_> { Space::new().into() };
    let mut body = row![
        if side { sidebar::sidebar(app) } else { nothing() },
        if side { vline(t) } else { nothing() },
    ];
    let middle = column![
        header(app),
        hline(t),
        if finding { find_bar(app) } else { nothing() },
        if finding { hline(t) } else { nothing() },
        timeline::timeline(app),
        dock::dock(app),
    ];
    body = body.push(middle.width(Length::Fill).height(Length::Fill));
    // Another conversation beside, as wide.
    if let Some(chat) = app.split_chat() {
        body = body.push(vline(t)).push(split::pane(app, chat));
    }
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
    // A menu a right click opened lies over all of it.
    let window: El<'_> = match menu::layer(app) {
        Some(menu) => stack![window, menu].into(),
        None => window.into(),
    };
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
                format!("{}  {}", s.a_side, keys_for(crate::keys::Act::Side)),
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
                button(icon(Icon::Toc, 16.0, t.fg_2))
                    .padding(6)
                    .style(t.ghost(t.fg_2, false))
                    .on_press_maybe(
                        (!app.chat.transcript.exchanges.is_empty())
                            .then_some(Msg::Command(crate::input::Command::Outline))
                    ),
                format!("{}  {}", s.w().c_outline, keys_for(crate::keys::Act::Outline)),
                tooltip::Position::Bottom,
            ),
            tip(
                t,
                button(icon(
                    Icon::Play,
                    15.0,
                    if app.script_runs.iter().any(|r| r.running()) {
                        t.accent
                    } else {
                        t.fg_2
                    }
                ))
                .padding(6)
                .style(t.ghost(t.fg_2, false))
                .on_press(Msg::Command(crate::input::Command::Scripts)),
                format!("{}  {}", s.w().c_scripts, keys_for(crate::keys::Act::Scripts)),
                tooltip::Position::Bottom,
            ),
            tip(
                t,
                button(icon(Icon::List, 16.0, t.fg_2))
                    .padding(6)
                    .style(t.ghost(t.fg_2, false))
                    .on_press(Msg::Show(Some(crate::app::Overlay::palette()))),
                format!("{}  {}", s.c_palette, keys_for(crate::keys::Act::Palette)),
                tooltip::Position::Bottom,
            ),
            tip(
                t,
                button(icon(Icon::Gear, 16.0, t.fg_2))
                    .padding(6)
                    .style(t.ghost(t.fg_2, false))
                    .on_press(Msg::Show(Some(crate::app::Overlay::Settings))),
                format!("{}  {}", s.settings, keys_for(crate::keys::Act::Settings)),
                tooltip::Position::Bottom,
            ),
            tip(
                t,
                button(inspector)
                    .padding(6)
                    .style(t.ghost(t.fg_2, false))
                    .on_press(Msg::ToggleInspector),
                format!("{}  {}", s.a_insp, keys_for(crate::keys::Act::Inspector)),
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
            .on_submit(Msg::FindStep(1))
            .size(13)
            .padding(0)
            .style(t.input_bare()),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if let Some(n) = found {
        let w = s.w();
        let said = if n == 0 {
            fill(s.found_n, "n", 0)
        } else {
            fill(fill(w.found_at, "at", app.find_at.min(n - 1) + 1).as_str(), "n", n)
        };
        line = line.push(text(said).size(12).color(if n == 0 { t.err } else { t.fg_3 }));
        // The last and the next of those found: also ⇧↵ / ↵ and ⌘⇧G / ⌘G.
        let step = |mark: Icon, hint: &'static str, by: i8| {
            tip(
                t,
                button(icon(mark, 13.0, t.fg_3))
                    .padding(4)
                    .style(t.ghost(t.fg_3, false))
                    .on_press_maybe((n > 0).then_some(Msg::FindStep(by))),
                hint.to_owned(),
                tooltip::Position::Bottom,
            )
        };
        line = line
            .push(step(Icon::ChevronUp, w.find_prev, -1))
            .push(step(Icon::ChevronDown, w.find_next, 1));
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
    // A scratch conversation says it goes once it is left.
    if app.chat.scratch {
        return row![
            tag(t, s.w().scratch_tag.to_owned(), t.warn_wash, t.warn),
            title_text(
                app.chat
                    .transcript
                    .title()
                    .map_or_else(|| s.new_session.to_owned(), |p| clip(first_line(p), 40))
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into();
    }
    // The name it was given in the list, as the list shows it; else its first message.
    match app.name().or(app.chat.transcript.title()) {
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
    // What the items after the folder take, in pixels, so that the folder gives way first: a
    // narrow window keeps the branch, the model and how full the conversation is, and loses the
    // start of a long path, which its tooltip still shows.
    let mut rest = BAR_EDGES;
    let mut tail = row![].spacing(6).align_y(Alignment::Center);
    // The branch checked out; a click copies its name.
    if let Some(branch) = &app.git.branch {
        let label = if app.copied == Some(Clip::Text(branch.clone())) {
            rest += prose_width(s.copied);
            item(s.copied.to_owned()).color(t.accent)
        } else {
            let shown = clip(branch, 28);
            rest += mono_width(&shown);
            item(shown).font(MONO)
        };
        rest += BAR_GAP + 8.0 + 11.0 + 4.0;
        tail = tail.push(dot()).push(tip(
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
            rest += prose_width(s.copied);
            item(s.copied.to_owned()).color(t.accent)
        } else {
            let shown = clip(id, 8).trim_end_matches('…').to_owned();
            rest += mono_width(&shown);
            item(shown).font(MONO)
        };
        rest += BAR_GAP + 8.0;
        tail = tail.push(dot()).push(tip(
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
        rest += BAR_GAP + mono_width(&model);
        tail = tail.push(dot()).push(item(model).font(MONO));
    }
    if let Some(mode) = app.permission {
        let color = if mode == "auto" { t.err } else { t.fg_3 };
        rest += BAR_GAP + prose_width(mode);
        tail = tail.push(dot()).push(item(mode.to_owned()).color(color));
    }
    if let Some(effort) = app.effort {
        rest += BAR_GAP + prose_width(effort);
        tail = tail.push(dot()).push(item(effort.to_owned()));
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
        // Under one percent is not nothing.
        let label = s.context.replace(
            "{n}",
            &if share < 0.01 {
                "<1".to_owned()
            } else {
                format!("{:.0}", share * 100.0)
            },
        );
        rest += BAR_GAP + prose_width(&label);
        tail = tail.push(dot()).push(tip(
            t,
            item(label).color(color),
            match app.compact_at {
                Some(at) => format!(
                    "{} / {}\n{}",
                    compact(tr.last_input),
                    compact(window),
                    crate::i18n::fill(
                        &crate::i18n::fill(s.w().compact_at, "n", format!("{:.0}", at * 100.0)),
                        "tokens",
                        compact((window as f64 * at) as u64)
                    )
                ),
                None => format!(
                    "{} / {}\n{}",
                    compact(tr.last_input),
                    compact(window),
                    s.w().compact_off
                ),
            },
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
    rest += 12.0 + mono_width(&usage);
    let mut right = row![].spacing(8).align_y(Alignment::Center);
    // Copied from the palette or `/export`, where there is no button to say so.
    if app.copied == Some(Clip::Conversation) {
        rest += 8.0 + prose_width(s.copied);
        right = right.push(item(s.copied.to_owned()).color(t.accent));
    }
    if let Some(toast) = &app.toast {
        let shown = clip(toast, 80);
        rest += 8.0 + prose_width(&shown);
        right = if shown == *toast {
            right.push(item(shown).color(t.fg_2))
        } else {
            right.push(tip(
                t,
                item(shown).color(t.fg_2),
                clip(toast, 600),
                tooltip::Position::Top,
            ))
        };
    }
    // A change of the list just made, taken back from here.
    if app.toast.is_some() && app.toast == app.undo_toast && !app.undo.is_empty() {
        let w = s.w();
        rest += 8.0 + prose_width(w.undo) + 24.0;
        right = right.push(
            button(
                row![
                    icon(Icon::Undo, 11.0, t.accent),
                    item(w.undo.to_owned()).color(t.accent)
                ]
                .spacing(4)
                .align_y(Alignment::Center),
            )
            .padding([1, 6])
            .style(t.ghost(t.accent, false))
            .on_press(Msg::Undo),
        );
    }
    let cwd = app.cwd.to_string_lossy();
    let shown = clip_start(&cwd, folder_cells(app.width, rest));
    let folder: El<'_> = if shown == cwd {
        item(shown).font(MONO).into()
    } else {
        tip(t, item(shown).font(MONO), cwd.into_owned(), tooltip::Position::Top)
    };
    let left = row![icon(Icon::Folder, 12.0, t.fg_3), folder, tail]
        .spacing(6)
        .align_y(Alignment::Center);
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

/// What the bottom bar takes besides its items: its padding, the folder's icon and the gaps
/// between its parts.
const BAR_EDGES: f32 = 24.0 + 12.0 + 6.0 + 6.0 + 16.0;
/// What an item after the folder takes besides its text: the dot before it and the gaps around it.
const BAR_GAP: f32 = 6.0 + 5.0 + 6.0;

/// About how wide a line of the bar's monospaced text is: a half-width cell is about 7 pixels at
/// its size. Only for sharing out the bar; what is drawn is measured by iced.
fn mono_width(s: &str) -> f32 {
    s.chars().map(cells).sum::<usize>() as f32 * 7.0
}

/// [`mono_width`] for the bar's proportional text, a little narrower.
fn prose_width(s: &str) -> f32 {
    s.chars().map(cells).sum::<usize>() as f32 * 6.0
}

/// How many cells of the folder's path the bottom bar shows, when `rest` of a bar `width` wide
/// goes to everything else on it: all that is left, between a short tail of the path and 46.
fn folder_cells(width: f32, rest: f32) -> usize {
    ((width - rest) / 7.0).clamp(14.0, 46.0) as usize
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

/// Text a program wrote: its last `max` lines, set in. A file named with its line in it opens in
/// the files tab at that line.
pub(crate) fn output_block<'a>(t: Tokens, s: Strings, out: &str, max: usize) -> El<'a> {
    let lines: Vec<&str> = out.trim_end().lines().collect();
    let skip = lines.len().saturating_sub(max);
    let mut body = column![].spacing(2);
    if skip > 0 {
        body = body.push(text(fill(s.more_lines, "n", skip)).size(11.5).color(t.fg_3));
    }
    let shown: El<'a> = if lines.is_empty() {
        text(s.no_output).size(12).font(MONO).color(t.fg_3).into()
    } else {
        let mut spans: Vec<iced::widget::text::Span<'a, String, Font>> = Vec::new();
        for (n, line) in lines[skip..].iter().enumerate() {
            if n > 0 {
                spans.push(iced::widget::span("\n"));
            }
            let mut at = 0;
            for (range, link) in crate::talk::refs(line) {
                if range.start > at {
                    spans.push(iced::widget::span(line[at..range.start].to_owned()));
                }
                spans.push(
                    iced::widget::span(line[range.clone()].to_owned())
                        .color(t.accent)
                        .underline(true)
                        .link(link),
                );
                at = range.end;
            }
            if at < line.len() {
                spans.push(iced::widget::span(line[at..].to_owned()));
            }
        }
        iced::widget::rich_text(spans)
            .size(12)
            .font(MONO)
            .color(t.fg_2)
            .wrapping(text::Wrapping::WordOrGlyph)
            .on_link_click(Msg::OpenRef)
            .into()
    };
    body = body.push(shown);
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

    #[test]
    fn the_folder_gives_way_first_in_the_bottom_bar() {
        // A wide window shows as much of the path as before; a narrow one what the rest leaves;
        // the narrowest still the path's end.
        assert_eq!(folder_cells(1440.0, 500.0), 46);
        assert_eq!(folder_cells(760.0, 600.0), 22);
        assert_eq!(folder_cells(500.0, 600.0), 14);
    }
}
