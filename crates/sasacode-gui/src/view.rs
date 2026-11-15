//! How the window looks.
//!
//! The conversation is laid out as the person who asked reads it: each request, then its answer.
//! The work between the two (thinking, tool calls, approvals) is live while it happens and folds
//! into one line once the answer is there, a click away. What needs the person (an approval) and
//! what the agent is doing now sit above the input, where the eyes already are.

use iced::font::Weight;
use iced::keyboard::{Key, key::Named};
use iced::widget::{
    Space, button, column, container, markdown, pick_list, rich_text, row, scrollable, text, text_editor, text_input,
};
use iced::{Alignment, Element, Font, Length, Padding, border, padding};

use crate::app::{App, EDITOR, EFFORTS, MODEL_ENTRY, Msg, PERMISSIONS, Pick};
use crate::style::Colors;
use crate::transcript::{Approval, Item, TodoState, Tool, Turn, brief, change};
use sasacode_protocol::Decision;

type El<'a> = Element<'a, Msg>;

const BOLD: Font = Font {
    weight: Weight::Bold,
    ..Font::DEFAULT
};
const MONO: Font = Font::MONOSPACE;
/// The width the conversation and the input share: about 45 full-width characters a line.
const COLUMN: f32 = 760.0;
const SIDE: f32 = 256.0;
/// Lines of a change shown before the rest is counted instead.
const CHANGE_LINES: usize = 40;
const APPROVAL_LINES: usize = 16;

impl App {
    pub fn view(&self) -> El<'_> {
        container(row![self.sidebar(), self.main()])
            .style(move |_| self.c.fill(self.c.page))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    // ---- the list of conversations ----

    fn sidebar(&self) -> El<'_> {
        let c = self.c;
        let folder = column![
            one_line(text(self.folder_name()).size(16).font(BOLD)),
            one_line(text(short_path(&self.cwd)).size(11).color(c.faint)),
        ]
        .spacing(2);

        let new = button(text(self.s.new_session).size(14).center().width(Length::Fill))
            .padding([8, 12])
            .width(Length::Fill)
            .style(c.primary())
            .on_press_maybe((!self.running()).then_some(Msg::NewSession));

        let needle = self.search.trim().to_lowercase();
        let shown: Vec<_> = self
            .sessions
            .iter()
            .filter(|s| needle.is_empty() || s.first_prompt.to_lowercase().contains(&needle))
            .collect();
        let mut list = column![].spacing(2);
        if shown.is_empty() {
            let why = if self.sessions.is_empty() {
                self.s.no_sessions
            } else {
                self.s.no_match
            };
            list = list.push(container(text(why).size(12.5).color(c.faint)).padding([8, 10]));
        }
        for s in shown {
            let current = self.session.as_deref() == Some(s.id.as_str());
            let title = if s.first_prompt.is_empty() {
                self.s.untitled
            } else {
                s.first_prompt.as_str()
            };
            let label = column![
                text(fit(title, 28)).size(13.5).wrapping(text::Wrapping::None),
                text(self.s.ago(s.modified, self.clock)).size(11).color(c.faint),
            ]
            .spacing(3);
            list = list.push(
                button(label)
                    .width(Length::Fill)
                    .padding([7, 10])
                    .style(c.ghost(current))
                    .on_press_maybe((!self.running()).then(|| Msg::OpenSession(s.path.clone()))),
            );
        }

        let mut side = column![folder, new].spacing(14);
        // A handful of conversations is found by eye; the field appears when the list outgrows that.
        if self.sessions.len() > 5 {
            side = side.push(
                text_input(self.s.search, &self.search)
                    .on_input(Msg::Search)
                    .size(13)
                    .padding([6, 10])
                    .style(c.field()),
            );
        }
        side = side.push(scrollable(list).height(Length::Fill).style(c.scroll()));
        container(side)
            .padding(Padding::new(12.0).top(16))
            .width(SIDE)
            .height(Length::Fill)
            .style(move |_| c.fill(c.side))
            .into()
    }

    // ---- the conversation ----

    fn main(&self) -> El<'_> {
        let mut dock = column![].spacing(8).max_width(COLUMN);
        if let Some(card) = self.approval() {
            dock = dock.push(card);
        }
        for q in &self.queued {
            dock = dock.push(self.queued_line(q));
        }
        if let Some(plan) = self.plan() {
            dock = dock.push(plan);
        }
        dock = dock.push(self.composer()).push(self.status());
        column![
            self.conversation(),
            container(dock)
                .padding(padding::all(24).top(4).bottom(10))
                .center_x(Length::Fill),
        ]
        .width(Length::Fill)
        .into()
    }

    fn conversation(&self) -> El<'_> {
        let c = self.c;
        if self.transcript.items.is_empty() {
            return container(
                column![
                    text(self.folder_name()).size(26).font(BOLD),
                    text(self.s.empty_lead).size(15).color(c.muted),
                    text(self.s.empty_note).size(12.5).color(c.faint),
                ]
                .spacing(10)
                .align_x(Alignment::Center),
            )
            .center(Length::Fill)
            .into();
        }
        let turns = self.transcript.turns();
        let last = turns.len() - 1;
        let views = turns
            .iter()
            .enumerate()
            .map(|(n, turn)| self.turn(*turn, n == last && self.running()));
        scrollable(
            container(column(views).spacing(28).max_width(COLUMN))
                .padding(padding::all(24).bottom(16))
                .center_x(Length::Fill),
        )
        .anchor_bottom()
        .height(Length::Fill)
        .style(c.scroll())
        .into()
    }

    fn turn(&self, turn: Turn, live: bool) -> El<'_> {
        let c = self.c;
        let items = &self.transcript.items;
        let mut col = column![].spacing(12);
        let mut from = turn.start;
        if let Item::User(request) = &items[from] {
            col = col.push(
                container(text(request.as_str()).size(15).line_height(1.6))
                    .padding([10, 14])
                    .width(Length::Fill)
                    .style(move |_| c.request()),
            );
            from += 1;
        }
        let answer = if live { None } else { self.transcript.answer(turn) };
        let Some(answer) = answer else {
            // Still running, or cut short: everything is shown as it came.
            col = self.range(col, from, turn.end);
            if !live {
                col = col.extend(self.turn_footer(turn, None));
            }
            return col.into();
        };

        let steps: Vec<&Tool> = items[from..answer]
            .iter()
            .filter_map(|i| match i {
                Item::Tool(t) if !is_plan(t) => Some(t),
                _ => None,
            })
            .collect();
        if answer > from {
            let open = self.open_work.contains(&turn.start);
            let mut head = row![caret(open, c), text(self.s.steps(steps.len())).size(13).color(c.muted)]
                .spacing(8)
                .align_y(Alignment::Center);
            if !open {
                head = head.push(one_line(text(self.breakdown(&steps)).size(12.5).color(c.faint)));
            }
            col = col.push(
                button(head)
                    .padding([5, 8])
                    .width(Length::Fill)
                    .style(c.ghost(false))
                    .on_press(Msg::ToggleWork(turn.start)),
            );
            if open {
                col = self.range(col, from, answer);
            }
        }
        col = self.range(col, answer, turn.end);
        let reply = match &items[answer] {
            Item::Assistant(md) => Some(md.raw.clone()),
            _ => None,
        };
        col.extend(self.turn_footer(turn, reply)).into()
    }

    /// "Run 2, Edit 1": what the folded work was, in the order the tools were first used.
    fn breakdown(&self, steps: &[&Tool]) -> String {
        let mut counts: Vec<(&str, usize)> = Vec::new();
        for t in steps {
            let label = self.s.tool(&t.name);
            match counts.iter_mut().find(|(l, _)| *l == label) {
                Some((_, n)) => *n += 1,
                None => counts.push((label, 1)),
            }
        }
        counts
            .iter()
            .map(|(label, n)| format!("{label} {n}"))
            .collect::<Vec<_>>()
            .join(self.s.list_separator())
    }

    /// The line under a finished turn: how long it took, which files it changed, and a way to copy
    /// the answer.
    fn turn_footer(&self, turn: Turn, reply: Option<String>) -> Option<El<'_>> {
        let c = self.c;
        let mut parts: Vec<String> = Vec::new();
        if let Some(Item::Done { seconds }) = self.transcript.items[turn.start..turn.end]
            .iter()
            .rev()
            .find(|i| matches!(i, Item::Done { .. }))
        {
            parts.push(self.s.duration(*seconds));
        }
        let files = self.transcript.changed_files(turn);
        if !files.is_empty() {
            parts.push(format!("{}: {}", self.s.changed, files.join(self.s.list_separator())));
        }
        if parts.is_empty() && reply.is_none() {
            return None;
        }
        let mut line = row![text(parts.join(" · ")).size(12).color(c.faint).width(Length::Fill)]
            .spacing(8)
            .align_y(Alignment::Center);
        if let Some(reply) = reply {
            line = line.push(self.copy_button(reply));
        }
        Some(line.into())
    }

    fn copy_button(&self, what: String) -> El<'_> {
        button(text(self.s.copy).size(12).color(self.c.muted))
            .padding([3, 8])
            .style(self.c.ghost(false))
            .on_press(Msg::Copy(what))
            .into()
    }

    /// Items `from..to` in order, with each run of steps gathered into one card.
    fn range<'a>(
        &'a self,
        mut col: iced::widget::Column<'a, Msg>,
        from: usize,
        to: usize,
    ) -> iced::widget::Column<'a, Msg> {
        let c = self.c;
        let mut steps: Vec<El<'a>> = Vec::new();
        let flush = |col: iced::widget::Column<'a, Msg>, steps: &mut Vec<El<'a>>| {
            if steps.is_empty() {
                return col;
            }
            col.push(
                container(column(std::mem::take(steps)).spacing(1))
                    .padding(4)
                    .width(Length::Fill)
                    .style(move |_| c.card()),
            )
        };
        for i in from..to {
            match &self.transcript.items[i] {
                Item::Thinking(t) => steps.push(self.thinking(i, t)),
                // The task list has its own place above the input; its updates are not steps.
                Item::Tool(t) if is_plan(t) => {}
                Item::Tool(t) => steps.push(self.step(i, t)),
                // An allowed call is the step that follows; only a wait or a refusal is worth a line.
                Item::Approval(a)
                    if a.answer
                        .as_deref()
                        .is_some_and(|x| x == self.s.allowed || x == self.s.always_allowed) => {}
                Item::Approval(a) => steps.push(self.approval_line(a)),
                Item::Done { .. } => {}
                Item::User(_) => {}
                Item::Assistant(md) => {
                    col = flush(col, &mut steps);
                    col = col.push(self.markdown(md.content.items()));
                }
                Item::Notice { text: t, error } => {
                    col = flush(col, &mut steps);
                    col = col.push(if *error {
                        El::from(
                            container(text(t.as_str()).size(13).line_height(1.5))
                                .padding([8, 12])
                                .width(Length::Fill)
                                .style(move |_| c.alert()),
                        )
                    } else {
                        text(t.as_str()).size(12.5).color(c.faint).into()
                    });
                }
            }
        }
        flush(col, &mut steps)
    }

    fn markdown<'a>(&'a self, items: &'a [markdown::Item]) -> El<'a> {
        let c = self.c;
        let mut style = markdown::Style::from_palette(self.theme().palette());
        style.inline_code_highlight = iced::advanced::text::Highlight {
            background: c.well.into(),
            border: border::rounded(4),
        };
        style.inline_code_color = c.text;
        style.inline_code_padding = padding::left(3).right(3);
        style.link_color = c.accent;
        let mut settings = markdown::Settings::with_text_size(15, style);
        settings.code_size = 13.0.into();
        settings.spacing = 12.0.into();
        settings.h1_size = 22.0.into();
        settings.h2_size = 19.0.into();
        settings.h3_size = 17.0.into();
        settings.h4_size = 15.0.into();
        markdown::view_with(items, settings, &Reply { c, copy: self.s.copy })
    }

    fn thinking<'a>(&'a self, i: usize, thought: &'a str) -> El<'a> {
        let c = self.c;
        let open = self.open.contains(&i);
        let mut head = row![
            text("·").size(13).color(c.faint).width(14).center(),
            text(self.s.thinking).size(13).color(c.muted),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if !open {
            let first = thought.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            head = head.push(one_line(text(first).size(12.5).color(c.faint)));
        } else {
            head = head.push(Space::new().width(Length::Fill));
        }
        let head = button(head.push(caret(open, c)))
            .padding([5, 8])
            .width(Length::Fill)
            .style(c.ghost(false))
            .on_press(Msg::Toggle(i));
        if !open {
            return head.into();
        }
        column![
            head,
            container(text(thought).size(13).line_height(1.6).color(c.muted)).padding(padding::all(8).left(30)),
        ]
        .into()
    }

    fn step<'a>(&'a self, i: usize, tool: &'a Tool) -> El<'a> {
        let c = self.c;
        let open = self.open.contains(&i);
        let mark = match tool.is_error {
            // A call without a result in a conversation that is not running was never answered.
            None if self.running() => text("●").color(if self.blink() { c.accent } else { c.tint }),
            None => text("·").color(c.faint),
            Some(false) => text("✓").color(c.accent),
            Some(true) => text("✕").color(c.danger),
        };
        let head = row![
            mark.size(13).width(14).center(),
            text(self.s.tool(&tool.name)).size(13),
            one_line(text(tool.summary.as_str()).font(MONO).size(12.5).color(c.muted)),
            caret(open, c),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let head = button(head)
            .padding([5, 8])
            .width(Length::Fill)
            .style(c.ghost(false))
            .on_press(Msg::Toggle(i));
        if !open {
            return head.into();
        }
        let mut detail = column![].spacing(6);
        if let Some(lines) = change(&tool.name, &tool.input) {
            detail = detail.push(self.change(&lines, CHANGE_LINES));
        }
        if !tool.output.is_empty() {
            detail = detail.push(
                container(text(tool.output.as_str()).font(MONO).size(12).line_height(1.45))
                    .padding(10)
                    .width(Length::Fill)
                    .style(move |_| c.well()),
            );
        }
        column![head, container(detail).padding(padding::all(6).left(30))].into()
    }

    fn approval_line<'a>(&'a self, a: &'a Approval) -> El<'a> {
        let c = self.c;
        let state = a.answer.as_deref().unwrap_or(self.s.awaiting);
        let first = brief(&a.args);
        container(
            row![
                text("·").size(13).color(c.faint).width(14).center(),
                text(state).size(13).color(c.muted),
                one_line(text(first).font(MONO).size(12.5).color(c.faint)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([5, 8])
        .into()
    }

    /// A change as lines that leave and lines that come.
    fn change<'a>(&'a self, lines: &[(char, String)], limit: usize) -> El<'a> {
        let c = self.c;
        let mut col = column![];
        for (sign, line) in lines.iter().take(limit) {
            let fill = if *sign == '-' { c.removed } else { c.added };
            col = col.push(
                container(
                    row![
                        text(sign.to_string()).font(MONO).size(12).color(c.muted).width(14),
                        text(line.clone()).font(MONO).size(12).line_height(1.45),
                    ]
                    .spacing(4),
                )
                .padding([1, 8])
                .width(Length::Fill)
                .style(move |_| c.fill(fill)),
            );
        }
        if lines.len() > limit {
            let more = self.s.more_lines.replace("{n}", &(lines.len() - limit).to_string());
            col = col.push(container(text(more).size(12).color(c.faint)).padding([4, 8]));
        }
        container(col)
            .clip(true)
            .width(Length::Fill)
            .style(move |_| c.well())
            .into()
    }

    // ---- above the input ----

    /// The question the run is waiting on, with what exactly would happen.
    fn approval(&self) -> Option<El<'_>> {
        let c = self.c;
        let a = self.transcript.pending_approval()?;
        let mut card = column![text(self.s.approval_title(&a.tool)).size(15).font(BOLD)].spacing(10);
        let path = a.args.get("path").and_then(|p| p.as_str());
        match (change(&a.tool, &a.args), path) {
            (Some(lines), Some(path)) => {
                card = card
                    .push(text(path.to_owned()).font(MONO).size(13))
                    .push(self.change(&lines, APPROVAL_LINES));
            }
            _ => {
                let shown: String = a.detail.lines().take(APPROVAL_LINES).collect::<Vec<_>>().join("\n");
                card = card.push(
                    container(text(shown).font(MONO).size(13).line_height(1.45))
                        .padding(10)
                        .width(Length::Fill)
                        .style(move |_| c.well()),
                );
            }
        }
        if !a.reason.is_empty() {
            card = card.push(
                text(format!("{}: {}", self.s.approval_why, a.reason))
                    .size(12)
                    .color(c.faint),
            );
        }
        let choice = |label: &'static str| text(label).size(14);
        card = card.push(
            row![
                text_input(self.s.feedback, &self.feedback)
                    .on_input(Msg::Feedback)
                    .on_submit(Msg::Answer(Decision::Deny))
                    .size(13)
                    .padding([7, 10])
                    .style(c.field()),
                button(choice(self.s.deny))
                    .padding([7, 14])
                    .style(c.danger_outline())
                    .on_press(Msg::Answer(Decision::Deny)),
                button(choice(self.s.always))
                    .padding([7, 14])
                    .style(c.outline())
                    .on_press(Msg::Answer(Decision::Always)),
                button(choice(self.s.allow))
                    .padding([7, 18])
                    .style(c.primary())
                    .on_press(Msg::Answer(Decision::Allow)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
        let keys = if cfg!(target_os = "macos") {
            self.s.approval_keys.replace("Ctrl+", "⌘")
        } else {
            self.s.approval_keys.to_owned()
        };
        card = card.push(container(text(keys).size(11.5).color(c.faint)).align_right(Length::Fill));
        Some(
            container(card)
                .padding(16)
                .width(Length::Fill)
                .style(move |_| c.raised())
                .into(),
        )
    }

    fn queued_line<'a>(&'a self, message: &'a str) -> El<'a> {
        let c = self.c;
        container(
            row![
                one_line(text(message.lines().next().unwrap_or("")).size(13)),
                text(self.s.queued).size(11.5).color(c.faint),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .padding([7, 12])
        .width(Length::Fill)
        .style(move |_| c.well())
        .into()
    }

    /// The agent's own task list while it works: how far it is and what it is on.
    fn plan(&self) -> Option<El<'_>> {
        let c = self.c;
        let todos = &self.transcript.todos;
        if !self.running() || todos.is_empty() {
            return None;
        }
        let done = todos.iter().filter(|t| t.state == TodoState::Done).count();
        let now = todos
            .iter()
            .find(|t| t.state == TodoState::Active)
            .or_else(|| todos.iter().find(|t| t.state == TodoState::Pending));
        let mut line = row![
            text(format!("{} {done}/{}", self.s.plan, todos.len()))
                .size(12)
                .color(c.muted)
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        if let Some(now) = now {
            line = line.push(one_line(text(now.text.as_str()).size(13)));
        }
        Some(
            container(line)
                .padding([7, 12])
                .width(Length::Fill)
                .style(move |_| c.well())
                .into(),
        )
    }

    // ---- the input ----

    fn composer(&self) -> El<'_> {
        let c = self.c;
        let placeholder = if self.running() {
            self.s.placeholder_running
        } else {
            self.s.placeholder
        };
        let editor = text_editor(&self.editor)
            .id(EDITOR)
            .placeholder(placeholder)
            .on_action(Msg::Edit)
            .size(15)
            .line_height(1.5)
            .padding(0)
            .min_height(46)
            .max_height(240)
            .style(c.editor())
            .key_binding(|press| {
                if press.key == Key::Named(Named::Enter) {
                    // With Ctrl/⌘ it is the window's shortcut (`app::shortcut`), not a second send.
                    if press.modifiers.command() {
                        return None;
                    }
                    if !press.modifiers.shift() && !press.modifiers.alt() {
                        return Some(text_editor::Binding::Custom(Msg::Send));
                    }
                }
                text_editor::Binding::from_key_press(press)
            });

        let mut controls = row![self.model_pick(), self.permission_pick(), self.effort_pick()]
            .spacing(2)
            .align_y(Alignment::Center);
        controls = controls.push(Space::new().width(Length::Fill));
        if self.running() {
            controls = controls.push(
                button(text(self.s.stop).size(13.5))
                    .padding([6, 14])
                    .style(c.danger_outline())
                    .on_press(Msg::Stop),
            );
        }
        let can_send = !self.editor.text().trim().is_empty();
        controls = controls.push(
            button(text(self.s.send).size(13.5))
                .padding([6, 16])
                .style(c.primary())
                .on_press_maybe(can_send.then_some(Msg::Send)),
        );

        let mut card = column![container(editor).padding(padding::all(4).bottom(0))].spacing(10);
        if let Some(entry) = &self.model_entry {
            card = card.push(
                row![
                    text_input(self.s.model_placeholder, entry)
                        .id(MODEL_ENTRY)
                        .on_input(Msg::ModelEntry)
                        .on_submit(Msg::ModelCommit)
                        .font(MONO)
                        .size(13)
                        .padding([6, 10])
                        .style(c.field()),
                    button(text(self.s.model_set).size(13))
                        .padding([6, 12])
                        .style(c.outline())
                        .on_press(Msg::ModelCommit),
                ]
                .spacing(8),
            );
        }
        container(card.push(controls.spacing(8)))
            .padding(padding::all(10).top(12))
            .width(Length::Fill)
            .style(move |_| c.card())
            .into()
    }

    fn model_pick(&self) -> El<'_> {
        let mut options = vec![Pick::new("", self.s.model_default)];
        options.extend(self.known_models().iter().map(|m| Pick::new(m, m.as_str())));
        options.push(Pick::new(Pick::OTHER, self.s.model_other));
        let chosen = match &self.prefs.model {
            Some(m) => Pick::new(m, m.as_str()),
            None => Pick::new("", self.s.model_default),
        };
        self.chip(options, chosen, Msg::Model)
    }

    fn permission_pick(&self) -> El<'_> {
        let pick = |mode: &str| Pick::new(mode, self.s.permission(mode));
        // What the CLI does when it is told nothing.
        let chosen = pick(self.prefs.permission.as_deref().unwrap_or("edits"));
        self.chip(PERMISSIONS.map(pick).to_vec(), chosen, Msg::Permission)
    }

    fn effort_pick(&self) -> El<'_> {
        let pick = |level: &str| Pick::new(level, self.s.effort(level));
        let default = Pick::new("", self.s.effort_default);
        let mut options = vec![default.clone()];
        options.extend(EFFORTS.map(pick));
        let chosen = self.prefs.effort.as_deref().map(pick).unwrap_or(default);
        self.chip(options, chosen, Msg::Effort)
    }

    fn chip<'a>(&'a self, options: Vec<Pick>, chosen: Pick, on_pick: fn(Pick) -> Msg) -> El<'a> {
        pick_list(options, Some(chosen), on_pick)
            .text_size(12.5)
            .padding([5, 8])
            .style(self.c.chip())
            .menu_style(self.c.menu())
            .into()
    }

    /// Under the input: what the agent is doing and for how long, else how the keys work; and what
    /// the conversation has used.
    fn status(&self) -> El<'_> {
        let c = self.c;
        let left: El<'_> = if let Some((flash, _)) = self.flash {
            text(flash).size(12).color(c.accent).into()
        } else if self.running() {
            let awaiting = self.transcript.pending_approval().is_some();
            let doing = self.s.activity(&self.transcript.activity, awaiting);
            row![
                text("●").size(10).color(if self.blink() { c.accent } else { c.tint }),
                text(format!("{doing} · {}", self.s.duration(self.elapsed())))
                    .size(12)
                    .color(c.muted),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into()
        } else {
            text(self.s.keys).size(12).color(c.faint).into()
        };
        let u = &self.transcript.usage;
        let mut line = row![left, Space::new().width(Length::Fill)].align_y(Alignment::Center);
        if u.input + u.output > 0 {
            let mut usage = format!(
                "{} {} · {} {}",
                self.s.usage_in,
                count(u.input),
                self.s.usage_out,
                count(u.output)
            );
            if u.cache_read > 0 {
                usage.push_str(&format!(" · {} {}", self.s.usage_cache, count(u.cache_read)));
            }
            if u.cost > 0.0 {
                usage.push_str(&format!(" · ${:.4}", u.cost));
            }
            line = line.push(text(usage).size(12).color(c.faint));
        }
        container(line).padding([0, 6]).into()
    }

    /// On for one tick, off for the next: the mark of something in progress.
    fn blink(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|r| (self.now.saturating_duration_since(r.started).as_millis() / 500).is_multiple_of(2))
    }
}

/// How a reply's Markdown is drawn where the default does not fit: paragraphs get the line height
/// Japanese text needs, and a code block is a quiet box with a way to copy it.
struct Reply {
    c: Colors,
    copy: &'static str,
}

impl<'a> markdown::Viewer<'a, Msg> for Reply {
    fn on_link_click(url: markdown::Uri) -> Msg {
        Msg::Link(url)
    }

    fn paragraph(&self, settings: markdown::Settings, text: &markdown::Text) -> El<'a> {
        rich_text(text.spans(settings.style))
            .size(settings.text_size)
            .line_height(1.65)
            .on_link_click(Msg::Link)
            .into()
    }

    fn code_block(
        &self,
        settings: markdown::Settings,
        _language: Option<&'a str>,
        code: &'a str,
        lines: &'a [markdown::Text],
    ) -> El<'a> {
        let c = self.c;
        let _ = lines;
        let code_lines = text(code.trim_end())
            .font(settings.style.code_block_font)
            .size(settings.code_size)
            .line_height(1.5)
            .wrapping(text::Wrapping::None);
        let copy = button(text(self.copy).size(11.5).color(c.muted))
            .padding([2, 8])
            .style(c.ghost(false))
            .on_press(Msg::Copy(code.to_owned()));
        container(
            row![
                scrollable(container(code_lines).padding(padding::all(12).bottom(14)))
                    .direction(scrollable::Direction::Horizontal(
                        scrollable::Scrollbar::default().width(6).scroller_width(6),
                    ))
                    .width(Length::Fill)
                    .style(c.scroll()),
                container(copy).padding(6),
            ]
            .align_y(Alignment::Start),
        )
        .width(Length::Fill)
        .style(move |_| c.well())
        .into()
    }
}

fn caret<'a>(open: bool, c: Colors) -> El<'a> {
    text(if open { "▾" } else { "▸" }).size(12).color(c.faint).into()
}

/// Text that takes the room left in its row and is cut at the edge instead of wrapping.
fn one_line<'a>(t: iced::widget::Text<'a>) -> El<'a> {
    container(t.wrapping(text::Wrapping::None))
        .clip(true)
        .width(Length::Fill)
        .into()
}

fn is_plan(tool: &Tool) -> bool {
    tool.name == "todo_write"
}

/// `s` cut to about `units` half-width columns (a full-width character counts two), with an
/// ellipsis when something was cut.
fn fit(s: &str, units: usize) -> String {
    let mut used = 0;
    for (at, ch) in s.char_indices() {
        used += if ch.is_ascii() { 1 } else { 2 };
        if used > units {
            return format!("{}…", &s[..at]);
        }
    }
    s.to_owned()
}

/// 1234 as 1.2k: a count is read at a glance, not to the token.
fn count(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..1_000_000 => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// The folder's path with the home directory as `~` and, when still long, its start cut.
fn short_path(path: &std::path::Path) -> String {
    let mut full = path.to_string_lossy().into_owned();
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
        && let Some(rest) = full.strip_prefix(&*home.to_string_lossy())
        && (rest.is_empty() || rest.starts_with(['/', '\\']))
    {
        full = format!("~{rest}");
    }
    let chars: Vec<char> = full.chars().collect();
    if chars.len() <= 34 {
        return full;
    }
    format!("…{}", chars[chars.len() - 33..].iter().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_shortened_and_a_long_path_keeps_its_end() {
        assert_eq!(fit("CIが落ちている", 30), "CIが落ちている");
        assert_eq!(
            fit("依存パッケージを最新にして、壊れたところを直して", 12),
            "依存パッケー…"
        );
        assert_eq!(count(999), "999");
        assert_eq!(count(1234), "1.2k");
        assert_eq!(count(2_500_000), "2.5M");
        let long = std::path::PathBuf::from(format!("/srv/{}/project", "x".repeat(60)));
        let short = short_path(&long);
        assert!(short.starts_with('…') && short.ends_with("/project") && short.chars().count() == 34);
    }
}
