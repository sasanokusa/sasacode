//! One conversation, drawn as a stalk: a mark for each request, a line that runs down through what
//! was done for it, and the answer under it. Every row is a cell of the stalk and then its content,
//! so the line is whole however tall a row gets.

use iced::widget::{Column, Space, button, column, container, markdown, rich_text, row, scrollable, stack, text};
use iced::{Alignment, Color, Length, border, padding};

use super::{COLUMN, El, fit, keycap, one_line, spaced};
use crate::app::{App, Msg, SCROLL};
use crate::icon::{self, icon};
use crate::palette::{Mode, permission_icon};
use crate::style::{BOLD, Colors, MEDIUM, MONO, UI, mix, size, thin};
use crate::transcript::{Approval, Item, Standing, Tool, Turn, Verdict, brief, change};

/// The stalk's column.
const RAIL: f32 = 24.0;
/// How far under a row's top its first line of text has its middle: where a mark sits.
const MARK_Y: f32 = 15.0;
/// Closed turns kept in sight above the open one: the request just before, so what led here is one
/// press away. Older ones wait behind one line.
pub(super) const RECENT: usize = 1;
/// Lines of a change or of output shown before the rest is counted instead.
const CHANGE_LINES: usize = 40;
const OUTPUT_LINES: usize = 30;

impl App {
    pub(super) fn conversation(&self) -> El<'_> {
        let c = self.c;
        let rows = if self.transcript.items.is_empty() {
            self.empty()
        } else {
            let turns = self.transcript.turns();
            let total = turns.len();
            let hidden = if self.earlier {
                0
            } else {
                total.saturating_sub(RECENT + 1)
            };
            let mut rows = column![];
            if total > RECENT + 1 {
                rows = rows.push(self.earlier_line(total - RECENT - 1));
            }
            for (n, turn) in turns.iter().enumerate().skip(hidden) {
                let first = n == 0 && total <= RECENT + 1;
                rows = self.turn(rows, *turn, first, n == total - 1);
            }
            rows
        };
        scrollable(
            container(rows.max_width(COLUMN))
                .padding(padding::all(20).top(8).bottom(24))
                .center_x(Length::Fill),
        )
        .id(SCROLL)
        .direction(thin())
        .on_scroll(|view| {
            let end = view.content_bounds().height - view.bounds().height;
            let at = view.absolute_offset().y;
            Msg::Scrolled(at > 1.0, at >= end - 24.0)
        })
        .height(Length::Fill)
        .style(c.scroll())
        .into()
    }

    // ---- the stalk ----

    /// The stalk's part of a row: its line, from the row's top when `above` and to its bottom when
    /// `below`, and a mark on it when the row has one.
    fn cell<'a>(&self, mark: Option<El<'a>>, above: bool, below: bool) -> El<'a> {
        let c = self.c;
        let line = move |on: bool| {
            container(Space::new())
                .width(1)
                .style(move |_| c.fill(if on { c.edge } else { Color::TRANSPARENT }))
        };
        let lines = column![line(above).height(MARK_Y), line(below).height(Length::Fill)]
            .width(RAIL)
            .align_x(Alignment::Center);
        let cell: El<'a> = match mark {
            Some(mark) => stack![
                lines,
                container(mark)
                    .center_x(RAIL)
                    .height(MARK_Y * 2.0)
                    .align_y(Alignment::Center)
            ]
            .into(),
            None => lines.into(),
        };
        container(cell).width(RAIL).height(Length::Fill).into()
    }

    /// A row along the stalk, with nothing on the line.
    fn along<'a>(&self, content: impl Into<El<'a>>, below: bool) -> El<'a> {
        row![self.cell(None, true, below), content.into()].into()
    }

    /// A turn's mark: a disc in the color of how it stands, a ring when it ended without an answer.
    fn mark<'a>(&self, standing: Standing) -> El<'a> {
        let c = self.c;
        let (color, hollow) = match standing {
            Standing::Running => (mix(c.sheet, c.leaf, 0.35 + 0.65 * self.breath()), false),
            Standing::Asking => (c.amber, false),
            Standing::Done => (c.leaf, false),
            Standing::Failed => (c.shu, false),
            Standing::Cut => (c.ink4, true),
        };
        let (rim, disc) = c.node(color, hollow);
        container(container(Space::new()).width(9).height(9).style(move |_| disc))
            .padding(2)
            .style(move |_| rim)
            .into()
    }

    /// The line that stands for the requests that are not listed.
    fn earlier_line(&self, n: usize) -> El<'_> {
        let c = self.c;
        let glyph = if self.earlier {
            icon::CHEVRON_DOWN
        } else {
            icon::CHEVRON_RIGHT
        };
        let (rim, _) = c.node(c.ink4, true);
        let mark = container(icon(glyph, 12.0, c.ink3)).style(move |_| rim);
        row![
            self.cell(Some(mark.into()), false, true),
            container(
                button(text(self.s.earlier(n)).size(size::BASE).color(c.ink3))
                    .padding([5, 8])
                    .style(c.quiet(c.sheet, false))
                    .on_press(Msg::Earlier),
            )
            .padding(padding::bottom(10)),
        ]
        .into()
    }

    // ---- a turn ----

    fn turn<'a>(&'a self, mut rows: Column<'a, Msg>, turn: Turn, first: bool, last: bool) -> Column<'a, Msg> {
        let c = self.c;
        let live = last && self.running();
        let open = self.turns.get(&turn.start).copied().unwrap_or(last);
        let standing = self.transcript.standing(turn, live);
        let request = self.transcript.request(turn).unwrap_or(self.s.untitled);

        if !open {
            // A closed turn: what was asked, and how the answer began.
            let mut lines = column![one_line(
                text(fit(request.lines().next().unwrap_or(""), 84))
                    .size(14)
                    .font(MEDIUM)
                    .color(c.ink2)
            )]
            .spacing(3);
            let after = self.transcript.preview(turn).unwrap_or_else(|| {
                let steps = self.transcript.layout(turn, false);
                self.s.steps(steps.folded.len() + steps.shown.len())
            });
            lines = lines.push(one_line(text(fit(&after, 96)).size(size::BASE).color(c.ink3)));
            let entry = button(lines)
                .padding([5, 8])
                .width(Length::Fill)
                .style(c.quiet(c.sheet, false))
                .on_press(Msg::Turn(turn.start));
            return rows.push(row![
                self.cell(Some(self.mark(standing)), !first, !last),
                container(entry).padding(padding::bottom(if last { 0 } else { 12 })),
            ]);
        }

        let head = button(text(request).size(size::READ).font(BOLD).line_height(1.5))
            .padding([4, 8])
            .width(Length::Fill)
            .style(c.quiet(c.sheet, false))
            .on_press(Msg::Turn(turn.start));
        rows = rows.push(row![
            self.cell(Some(self.mark(standing)), !first, true),
            container(head).padding(padding::bottom(4))
        ]);

        let layout = self.transcript.layout(turn, live);
        if !layout.folded.is_empty() {
            let shown = self.open_work.contains(&turn.start);
            rows = rows.push(self.along(self.fold_line(turn, &layout.folded, shown), true));
            if shown {
                for i in &layout.folded {
                    rows = rows.push(self.along(self.item(*i), true));
                }
            }
        }
        for i in &layout.shown {
            rows = rows.push(self.along(self.item(*i), true));
        }
        if live {
            rows.push(self.tip())
        } else {
            let foot = self.receipt(turn).unwrap_or_else(|| Space::new().into());
            rows.push(self.along(
                container(foot).padding(padding::bottom(if last { 0 } else { 22 })),
                !last,
            ))
        }
    }

    /// The one line the work before an answer folds into: how much there was, and of what kinds.
    fn fold_line(&self, turn: Turn, folded: &[usize], open: bool) -> El<'_> {
        let c = self.c;
        let tools: Vec<&Tool> = folded
            .iter()
            .filter_map(|i| match &self.transcript.items[*i] {
                Item::Tool(t) => Some(t),
                _ => None,
            })
            .collect();
        let mut kinds: Vec<(&str, usize)> = Vec::new();
        for t in &tools {
            let label = self.s.tool(&t.name);
            match kinds.iter_mut().find(|(l, _)| *l == label) {
                Some((_, n)) => *n += 1,
                None => kinds.push((label, 1)),
            }
        }
        let kinds = kinds
            .iter()
            .map(|(label, n)| format!("{label} {n}"))
            .collect::<Vec<_>>()
            .join(self.s.list_separator());
        let n = if tools.is_empty() { folded.len() } else { tools.len() };
        let glyph = if open { icon::CHEVRON_DOWN } else { icon::CHEVRON_RIGHT };
        button(
            row![
                icon(glyph, 14.0, c.ink3),
                text(self.s.steps(n)).size(size::BASE).color(c.ink2),
                one_line(text(kinds).size(size::SMALL).color(c.ink3)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([5, 8])
        .width(Length::Fill)
        .style(c.quiet(c.sheet, false))
        .on_press(Msg::ToggleWork(turn.start))
        .into()
    }

    fn item(&self, i: usize) -> El<'_> {
        let c = self.c;
        match &self.transcript.items[i] {
            Item::Assistant(md) => container(self.markdown(md.content.items()))
                .padding(padding::all(8).top(6).bottom(10))
                .into(),
            Item::Thinking(thought) => self.thinking(i, thought),
            Item::Tool(tool) => self.step(i, tool),
            Item::Approval(approval) => self.asked(approval),
            Item::Notice { text: t, error: true } => container(
                container(
                    row![
                        container(icon(icon::CIRCLE_ALERT, 15.0, c.shu)).padding(padding::top(2)),
                        text(t.as_str()).size(size::BASE).line_height(1.55),
                    ]
                    .spacing(8),
                )
                .padding([8, 12])
                .width(Length::Fill)
                .style(move |_| c.alert()),
            )
            .padding(padding::all(8).top(4).bottom(4))
            .into(),
            Item::Notice { text: t, error: false } => container(
                row![
                    icon(icon::INFO, 14.0, c.ink4),
                    text(t.as_str()).size(size::SMALL).color(c.ink3)
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding([5, 8])
            .into(),
            Item::User(_) | Item::Done { .. } => Space::new().into(),
        }
    }

    /// The growing end of the stalk: what the agent is doing right now, and for how long.
    fn tip(&self) -> El<'_> {
        let c = self.c;
        let asking = self.transcript.pending_approval().is_some();
        let doing = self.s.activity(&self.transcript.activity, asking);
        let (rim, _) = c.node(c.ink4, true);
        let mark = container(self.dot(self.live_color(), if asking { 1.0 } else { self.breath() }))
            .padding(3)
            .style(move |_| rim);
        row![
            self.cell(Some(mark.into()), true, false),
            container(
                row![
                    text(doing)
                        .size(size::BASE)
                        .color(if asking { c.amber } else { c.ink2 }),
                    text(self.s.duration(self.elapsed())).size(size::SMALL).color(c.ink3),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .padding([6, 8])
            .height(MARK_Y * 2.0)
            .align_y(Alignment::Center),
        ]
        .into()
    }

    /// Under a finished turn: how long it took, which files it changed, and a way to copy the answer.
    fn receipt(&self, turn: Turn) -> Option<El<'_>> {
        let c = self.c;
        let items = &self.transcript.items;
        let mut line = row![].spacing(14).align_y(Alignment::Center);
        let mut any = false;
        if let Some(Item::Done { seconds }) = items[turn.start..turn.end]
            .iter()
            .rev()
            .find(|i| matches!(i, Item::Done { .. }))
        {
            line = line.push(text(self.s.duration(*seconds)).size(size::SMALL).color(c.ink3));
            any = true;
        }
        let files = self.transcript.changed_files(turn);
        if !files.is_empty() {
            line = line.push(
                button(
                    row![
                        icon(icon::FILE_DIFF, 13.0, c.ink3),
                        text(self.s.changed(&files)).size(size::SMALL).color(c.ink2),
                    ]
                    .spacing(5)
                    .align_y(Alignment::Center),
                )
                .padding([3, 6])
                .style(c.quiet(c.sheet, false))
                .on_press_maybe((!self.changes).then_some(Msg::Changes)),
            );
            any = true;
        }
        line = line.push(Space::new().width(Length::Fill));
        if let Some(Item::Assistant(md)) = self.transcript.answer(turn).map(|i| &items[i]) {
            line = line.push(self.copy_button(md.raw.clone(), c.sheet));
            any = true;
        }
        any.then(|| container(line).padding(padding::left(8).right(4)).into())
    }

    pub(super) fn copy_button<'a>(&self, what: String, on: Color) -> El<'a> {
        let c = self.c;
        button(
            row![
                icon(icon::COPY, 13.0, c.ink3),
                text(self.s.copy).size(size::SMALL).color(c.ink2)
            ]
            .spacing(5)
            .align_y(Alignment::Center),
        )
        .padding([3, 6])
        .style(c.quiet(on, false))
        .on_press(Msg::Copy(what))
        .into()
    }

    // ---- rows of work ----

    fn thinking<'a>(&'a self, i: usize, thought: &'a str) -> El<'a> {
        let c = self.c;
        let open = self.open.contains(&i);
        let mut head = row![
            icon(icon::BRAIN, 14.0, c.ink3),
            text(self.s.thinking).size(size::BASE).color(c.ink2),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if open {
            head = head.push(Space::new().width(Length::Fill));
        } else {
            let first = thought.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            head = head.push(one_line(text(first).size(size::SMALL).color(c.ink3)));
        }
        let head = button(head.push(chevron(open, c)))
            .padding([5, 8])
            .width(Length::Fill)
            .style(c.quiet(c.sheet, false))
            .on_press(Msg::Toggle(i));
        if !open {
            return head.into();
        }
        column![
            head,
            container(text(thought).size(size::BASE).line_height(1.65).color(c.ink2))
                .padding(padding::all(8).left(30).top(2)),
        ]
        .into()
    }

    fn step<'a>(&'a self, i: usize, tool: &'a Tool) -> El<'a> {
        let c = self.c;
        let open = self.open.contains(&i);
        let glyph = icon::tool(&tool.name);
        let lead = match tool.is_error {
            // A call without a result in a conversation that is not running was never answered.
            None if self.running() => icon(glyph, 14.0, mix(c.sheet, c.leaf, 0.4 + 0.6 * self.breath())),
            Some(true) => icon(glyph, 14.0, c.shu),
            _ => icon(glyph, 14.0, c.ink3),
        };
        let mut head = row![
            lead,
            text(self.s.tool(&tool.name)).size(size::BASE).color(c.ink2),
            one_line(text(tool.summary.as_str()).font(MONO).size(size::SMALL).color(c.ink3)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if tool.is_error == Some(true) {
            head = head.push(text(self.s.failed).size(size::SMALL).color(c.shu));
        }
        let head = button(head.push(chevron(open, c)))
            .padding([5, 8])
            .width(Length::Fill)
            .style(c.quiet(c.sheet, false))
            .on_press(Msg::Toggle(i));
        if !open {
            return head.into();
        }
        let mut detail = column![].spacing(6);
        if let Some(lines) = change(&tool.name, &tool.input) {
            detail = detail.push(self.diff(&lines, CHANGE_LINES, c.well));
        }
        if !tool.output.is_empty() {
            let lines: Vec<&str> = tool.output.lines().collect();
            let skipped = lines.len().saturating_sub(OUTPUT_LINES);
            let mut out = column![].spacing(6);
            if skipped > 0 {
                let more = self.s.more_lines.replace("{n}", &skipped.to_string());
                out = out.push(text(more).size(size::SMALL).color(c.ink3));
            }
            out = out.push(
                text(spaced(&lines[skipped..].join("\n")))
                    .font(MONO)
                    .size(size::SMALL)
                    .line_height(1.5)
                    .color(c.ink2),
            );
            detail = detail.push(
                container(out)
                    .padding([8, 10])
                    .width(Length::Fill)
                    .style(move |_| c.well(8.0)),
            );
        }
        column![head, container(detail).padding(padding::all(8).left(30).top(2))].into()
    }

    /// An approval that was refused or withdrawn.
    fn asked<'a>(&'a self, a: &'a Approval) -> El<'a> {
        let c = self.c;
        let (ink, state) = match a.answer {
            Some(Verdict::Cancelled) => (c.ink3, self.s.cancelled),
            _ => (c.shu, self.s.denied),
        };
        container(
            row![
                icon(icon::X, 14.0, ink),
                text(state).size(size::BASE).color(c.ink2),
                one_line(text(brief(&a.args)).font(MONO).size(size::SMALL).color(c.ink3)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([5, 8])
        .into()
    }

    /// A change as lines that leave and lines that come, on the surface `on`.
    pub(super) fn diff<'a>(&self, lines: &[(char, String)], limit: usize, on: Color) -> El<'a> {
        let c = self.c;
        let mut col = column![].spacing(1);
        for (sign, line) in lines.iter().take(limit) {
            let (fill, ink, shown) = if *sign == '-' {
                (c.removed, c.removed_ink, '\u{2212}')
            } else {
                (c.added, c.added_ink, '+')
            };
            col = col.push(
                container(
                    row![
                        text(shown).font(MONO).size(size::SMALL).color(ink).width(12),
                        text(spaced(line)).font(MONO).size(size::SMALL).line_height(1.5),
                    ]
                    .spacing(6),
                )
                .padding([1, 8])
                .width(Length::Fill)
                .style(move |_| container::Style {
                    background: Some(fill.into()),
                    border: border::rounded(4),
                    ..container::Style::default()
                }),
            );
        }
        if lines.len() > limit {
            let more = self.s.more_lines.replace("{n}", &(lines.len() - limit).to_string());
            col = col.push(container(text(more).size(size::SMALL).color(c.ink3)).padding([4, 8]));
        }
        container(col)
            .padding(4)
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(on.into()),
                border: border::rounded(8),
                ..container::Style::default()
            })
            .into()
    }

    // ---- an answer ----

    fn markdown<'a>(&'a self, items: &'a [markdown::Item]) -> El<'a> {
        let c = self.c;
        let style = markdown::Style {
            font: UI,
            inline_code_highlight: iced::advanced::text::Highlight {
                background: c.well.into(),
                border: border::rounded(4),
            },
            inline_code_padding: padding::left(4).right(4),
            inline_code_color: c.ink,
            inline_code_font: MONO,
            code_block_font: MONO,
            link_color: c.leaf,
        };
        let mut settings = markdown::Settings::with_text_size(size::READ, style);
        settings.code_size = 12.5.into();
        settings.spacing = 12.0.into();
        settings.h1_size = 19.0.into();
        settings.h2_size = 17.0.into();
        settings.h3_size = size::READ.into();
        settings.h4_size = size::READ.into();
        settings.h5_size = size::READ.into();
        settings.h6_size = size::READ.into();
        markdown::view_with(items, settings, &Reply { app: self })
    }

    // ---- before the first request ----

    /// Where a conversation starts: what to do, what the agent will be allowed, and the keys.
    fn empty(&self) -> Column<'_, Msg> {
        let c = self.c;
        let (rim, ring) = c.node(c.ink4, true);
        let mark = container(container(Space::new()).width(9).height(9).style(move |_| ring))
            .padding(2)
            .style(move |_| rim);
        let lead = column![
            text(self.s.empty_lead).size(size::LEAD).font(BOLD),
            text(self.s.empty_note(&self.folder_name()))
                .size(size::BASE)
                .line_height(1.6)
                .color(c.ink2),
        ]
        .spacing(8);

        // The names of the three settings share a column: three characters in Japanese, a word in English.
        let names = if self.s.ja { 52 } else { 80 };
        let setting = |glyph, label: &'static str, value: String, note: &'static str, mode| {
            let mut lines = column![text(value).size(size::BASE)].spacing(3);
            if !note.is_empty() {
                lines = lines.push(text(note).size(size::SMALL).line_height(1.55).color(c.ink3));
            }
            button(
                row![
                    container(icon(glyph, 15.0, c.ink3)).padding(padding::top(1)),
                    text(label).size(size::BASE).color(c.ink3).width(names),
                    lines,
                ]
                .spacing(10),
            )
            .padding([7, 8])
            .width(Length::Fill)
            .style(c.quiet(c.sheet, false))
            .on_press(Msg::Find(Some(mode)))
        };
        let mode = self.permission();
        let settings = column![
            setting(icon::BOX, self.s.model, self.model_label(), "", Mode::Model),
            setting(
                permission_icon(mode),
                self.s.permission_label,
                self.s.permission(mode),
                self.s.permission_note(mode),
                Mode::Permission,
            ),
            setting(icon::GAUGE, self.s.effort_label, self.effort_label(), "", Mode::Effort),
        ]
        .spacing(1);

        let key = |name: String, does: &'static str| {
            row![
                container(keycap(name, c)).width(96),
                text(does).size(size::SMALL).color(c.ink2)
            ]
            .align_y(Alignment::Center)
            .width(Length::FillPortion(1))
        };
        let side = if self.sidebar {
            self.s.hide_sidebar
        } else {
            self.s.show_sidebar
        };
        let s = &self.s;
        let pairs = [
            (key("Enter".into(), s.send), key("Shift+Enter".into(), s.newline)),
            (key(s.key("Enter"), s.allow), key(s.key("."), s.stop)),
            (key(s.key("K"), s.find), key(s.key("N"), s.new_session)),
            (key(s.key("D"), s.show_changes), key(s.key("B"), side)),
        ];
        // Two to a line while both fit, else one under the other.
        let paired = self.column_room() >= 600.0;
        let mut keys = column![].spacing(9);
        for (left, right) in pairs {
            keys = if paired {
                keys.push(row![left, right])
            } else {
                keys.push(left).push(right)
            };
        }

        column![
            row![
                self.cell(Some(mark.into()), false, false),
                container(lead).padding(padding::all(8).top(2).bottom(22))
            ],
            container(settings).padding(padding::left(RAIL)),
            container(keys).padding(padding::left(RAIL + 8.0).top(26)),
        ]
        .padding(padding::top(28))
    }
}

fn chevron<'a>(open: bool, c: Colors) -> El<'a> {
    let glyph = if open { icon::CHEVRON_DOWN } else { icon::CHEVRON_RIGHT };
    icon(glyph, 13.0, c.ink4).into()
}

/// How an answer's Markdown is drawn where the default does not fit: paragraphs get the line height
/// Japanese text needs, headings are told by weight more than by size, and a code block is a well
/// with a way to copy it.
struct Reply<'a> {
    app: &'a App,
}

impl<'a> markdown::Viewer<'a, Msg> for Reply<'a> {
    fn on_link_click(url: markdown::Uri) -> Msg {
        Msg::Link(url)
    }

    fn paragraph(&self, settings: markdown::Settings, text: &markdown::Text) -> El<'a> {
        rich_text(text.spans(settings.style))
            .size(settings.text_size)
            .line_height(1.7)
            .on_link_click(Msg::Link)
            .into()
    }

    fn heading(
        &self,
        settings: markdown::Settings,
        level: &'a markdown::HeadingLevel,
        text: &'a markdown::Text,
        index: usize,
    ) -> El<'a> {
        let size = match level {
            markdown::HeadingLevel::H1 => settings.h1_size,
            markdown::HeadingLevel::H2 => settings.h2_size,
            _ => settings.h3_size,
        };
        let style = markdown::Style {
            font: BOLD,
            ..settings.style
        };
        container(
            rich_text(text.spans(style))
                .size(size)
                .line_height(1.45)
                .on_link_click(Msg::Link),
        )
        .padding(padding::top(if index > 0 { 8 } else { 0 }))
        .into()
    }

    fn code_block(
        &self,
        settings: markdown::Settings,
        language: Option<&'a str>,
        code: &'a str,
        _lines: &'a [markdown::Text],
    ) -> El<'a> {
        let c = self.app.c;
        let code_lines = text(spaced(code.trim_end()))
            .font(settings.style.code_block_font)
            .size(settings.code_size)
            .line_height(1.55)
            .wrapping(text::Wrapping::None);
        // The name of the language and the copy are a line of their own: a long line of code runs
        // under neither.
        let head = row![
            text(language.unwrap_or(""))
                .size(size::SMALL)
                .color(c.ink3)
                .width(Length::Fill),
            self.app.copy_button(code.to_owned(), c.well),
        ]
        .align_y(Alignment::Center);
        container(column![
            container(head).padding(padding::left(12).right(4).top(4)),
            scrollable(container(code_lines).padding(padding::all(12).top(2).bottom(14)))
                .direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::default().width(4).scroller_width(4).margin(3),
                ))
                .width(Length::Fill)
                .style(c.scroll()),
        ])
        .width(Length::Fill)
        .style(move |_| c.well(8.0))
        .into()
    }
}
