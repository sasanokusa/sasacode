//! The foot of the sheet: what waits for the person (an approval), what waits for the model (a
//! message sent during the run, the agent's task list), and the input. It does not scroll: the
//! question and the place to answer are always where they were.

use iced::keyboard::{Key, key::Named};
use iced::widget::{Space, button, column, container, mouse_area, row, text, text_editor, text_input, tooltip};
use iced::{Alignment, Color, Length, padding};
use sasacode_protocol::Decision;

use super::{COLUMN, El, count, fit, hint, one_line, spaced, units};
use crate::app::{App, EDITOR, Msg};
use crate::icon::{self, icon};
use crate::palette::{Mode, permission_icon};
use crate::style::{BOLD, MEDIUM, MONO, size};
use crate::transcript::{TodoState, change};

const APPROVAL_LINES: usize = 14;
/// What the input's padding takes of its width, with some to spare, and what each of its round
/// buttons takes.
const EDGES: f32 = 28.0;
const ROUND: f32 = 37.0;
/// About the width of a half-width character in a setting's label.
const CHIP_UNIT: f32 = 6.0;

impl App {
    pub(super) fn dock(&self) -> El<'_> {
        let mut dock = column![].spacing(8).max_width(COLUMN);
        if let Some(slip) = self.approval() {
            dock = dock.push(slip);
        }
        for message in &self.queued {
            dock = dock.push(self.queued_line(message));
        }
        if let Some(plan) = self.plan() {
            dock = dock.push(plan);
        }
        dock = dock.push(self.composer()).push(self.status());
        container(dock)
            .padding(padding::all(20).top(4).bottom(10))
            .center_x(Length::Fill)
            .into()
    }

    /// The question the run is waiting on, with exactly what would happen.
    fn approval(&self) -> Option<El<'_>> {
        let c = self.c;
        let a = self.transcript.pending_approval()?;
        let mut title = row![
            icon(icon::HAND, 16.0, c.amber),
            text(self.s.approval_title(&a.tool))
                .size(size::READ)
                .font(BOLD)
                .width(Length::Fill),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let waiting = self.transcript.pending_approvals();
        if waiting > 1 {
            title = title.push(text(self.s.approval_more(waiting - 1)).size(size::SMALL).color(c.ink3));
        }
        let mut slip = column![title].spacing(10);

        let path = a.args.get("path").and_then(|p| p.as_str());
        match (change(&a.tool, &a.args), path) {
            (Some(lines), Some(path)) => {
                slip = slip.push(
                    column![
                        text(path.to_owned()).font(MONO).size(size::SMALL).color(c.ink2),
                        container(self.diff(&lines, APPROVAL_LINES, c.sheet))
                            .padding(1)
                            .style(move |_| c.inset()),
                    ]
                    .spacing(6),
                );
            }
            _ => {
                let shown: String = a.detail.lines().take(APPROVAL_LINES).collect::<Vec<_>>().join("\n");
                slip = slip.push(
                    container(text(spaced(&shown)).font(MONO).size(size::BASE).line_height(1.5))
                        .padding([9, 12])
                        .width(Length::Fill)
                        .style(move |_| c.inset()),
                );
            }
        }
        if !a.reason.is_empty() {
            slip = slip.push(
                text(self.s.approval_reason(&a.reason))
                    .size(size::SMALL)
                    .line_height(1.5)
                    .color(c.ink2),
            );
        }

        let choice = |label: &'static str| text(label).size(size::BASE).font(MEDIUM);
        let mut allow = row![choice(self.s.allow)].spacing(8).align_y(Alignment::Center);
        // The key is taught where there is room for it; a narrow slip keeps the room for the reason.
        if self.column_room() >= 520.0 {
            allow = allow.push(
                text(self.s.key("Enter"))
                    .size(size::TINY)
                    .color(Color { a: 0.75, ..c.on_leaf }),
            );
        }
        slip = slip.push(
            row![
                text_input(self.s.feedback, &self.feedback)
                    .on_input(Msg::Feedback)
                    .on_submit(Msg::Answer(Decision::Deny))
                    .size(size::BASE)
                    .padding([7, 10])
                    .style(c.field()),
                button(choice(self.s.deny))
                    .padding([7, 14])
                    .style(c.outline(c.kuma, c.shu))
                    .on_press(Msg::Answer(Decision::Deny)),
                button(choice(self.s.always))
                    .padding([7, 14])
                    .style(c.outline(c.kuma, c.ink))
                    .on_press(Msg::Answer(Decision::Always)),
                button(allow)
                    .padding([7, 14])
                    .style(c.primary())
                    .on_press(Msg::Answer(Decision::Allow)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
        Some(
            container(slip)
                .padding(padding::all(14).top(12))
                .width(Length::Fill)
                .style(move |_| c.slip())
                .into(),
        )
    }

    fn queued_line<'a>(&'a self, message: &'a str) -> El<'a> {
        let c = self.c;
        container(
            row![
                icon(icon::CLOCK, 14.0, c.ink3),
                one_line(text(message.lines().next().unwrap_or("")).size(size::BASE)),
                text(self.s.queued).size(size::SMALL).color(c.ink3),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([7, 12])
        .width(Length::Fill)
        .style(move |_| c.well(8.0))
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
            icon(icon::LIST_CHECKS, 14.0, c.ink3),
            text(format!("{} {done}/{}", self.s.plan, todos.len()))
                .size(size::SMALL)
                .color(c.ink3),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if let Some(now) = now {
            line = line.push(one_line(text(now.text.as_str()).size(size::BASE).color(c.ink2)));
        }
        Some(container(line).padding([2, 12]).width(Length::Fill).into())
    }

    // ---- the input ----

    fn composer(&self) -> El<'_> {
        let c = self.c;
        let placeholder = if self.running() {
            self.s.placeholder_running
        } else {
            self.s.placeholder
        };
        let empty = self.editor.text().trim().is_empty();
        let editor = text_editor(&self.editor)
            .id(EDITOR)
            .placeholder(placeholder)
            .on_action(Msg::Edit)
            .size(size::READ)
            .line_height(1.5)
            .padding(0)
            .min_height(45)
            .max_height(220)
            .style(c.editor())
            .key_binding(move |press| {
                // Every field hears every key. Only the one that has the focus acts on it.
                if !matches!(press.status, text_editor::Status::Focused { .. }) {
                    return None;
                }
                let command = press.modifiers.command();
                match press.key.as_ref() {
                    // With Ctrl/⌘ it is the window's key (allow, else send), handled there once.
                    Key::Named(Named::Enter) if command => return None,
                    Key::Named(Named::Enter) if !press.modifiers.shift() && !press.modifiers.alt() => {
                        return Some(text_editor::Binding::Custom(Msg::Send));
                    }
                    // Esc puts the command list away; the input keeps the focus.
                    Key::Named(Named::Escape) => return None,
                    Key::Named(Named::ArrowUp) if empty => return Some(text_editor::Binding::Custom(Msg::Recall)),
                    _ => {}
                }
                // A key pressed with Ctrl/⌘ is a command, never a letter to type.
                match text_editor::Binding::from_key_press(press) {
                    Some(text_editor::Binding::Insert(_)) if command => None,
                    binding => binding,
                }
            });

        let mode = self.permission();
        let (model, permission, effort) = (self.model_label(), self.s.permission(mode), self.effort_label());
        // The three settings say their value in words while there is room, the model's name giving
        // way first. In an input too narrow for that they are icons, and say it under the pointer.
        let wide = |label: &str| 33.0 + CHIP_UNIT * units(label) as f32;
        let round = if self.running() { 2.0 * ROUND } else { ROUND };
        let room = self.column_room() - EDGES - round - wide(&permission) - wide(&effort) - wide("");
        let model_units = (room / CHIP_UNIT).floor();
        let words = model_units >= 8.0;
        let chip = |glyph, label: String, ink: Color, mode| -> El<'_> {
            let press = Msg::Find(Some(mode));
            if words {
                let label = text(label)
                    .size(size::SMALL)
                    .color(c.ink2)
                    .wrapping(text::Wrapping::None);
                button(
                    row![icon(glyph, 14.0, ink), label]
                        .spacing(5)
                        .align_y(Alignment::Center),
                )
                .padding([4, 7])
                .style(c.quiet(c.well, false))
                .on_press(press)
                .into()
            } else {
                hint(
                    button(icon(glyph, 15.0, ink))
                        .padding(7)
                        .style(c.quiet(c.well, false))
                        .on_press(press),
                    label,
                    tooltip::Position::Top,
                    c,
                )
            }
        };
        // Allowing everything is the one setting that should not look like the others.
        let guard = if mode == "auto" { c.amber } else { c.ink3 };
        let mut controls = row![
            chip(
                icon::BOX,
                fit(&model, (model_units as usize).min(34)),
                c.ink3,
                Mode::Model
            ),
            chip(permission_icon(mode), permission, guard, Mode::Permission),
            chip(icon::GAUGE, effort, c.ink3, Mode::Effort),
            Space::new().width(Length::Fill),
        ]
        .spacing(2)
        .align_y(Alignment::Center);
        if self.running() {
            controls = controls.push(hint(
                button(icon(icon::SQUARE, 13.0, c.sheet))
                    .padding(9)
                    .style(c.solid())
                    .on_press(Msg::Stop),
                format!("{}  {}", self.s.stop, self.s.key(".")),
                tooltip::Position::Top,
                c,
            ));
        }
        let ink = if empty { c.ink4 } else { c.on_leaf };
        controls = controls.push(hint(
            button(icon(icon::ARROW_UP, 17.0, ink))
                .padding(7)
                .style(if empty { c.quiet(c.well, true) } else { c.primary() })
                .on_press_maybe((!empty).then_some(Msg::Send)),
            format!("{}  Enter", self.s.send),
            tooltip::Position::Top,
            c,
        ));

        // A press anywhere on the input that is not on a control is a press on its text.
        mouse_area(
            container(
                column![
                    container(editor).padding(padding::all(6).bottom(0)),
                    controls.spacing(6)
                ]
                .spacing(8),
            )
            .padding(8)
            .width(Length::Fill)
            .style(move |_| c.input()),
        )
        .on_press(Msg::Write)
        .interaction(iced::mouse::Interaction::Text)
        .into()
    }

    /// Under the input: what was just done (a copy), and what the conversation has used. The row is
    /// there even when it is empty, so the input does not move when the first numbers arrive.
    fn status(&self) -> El<'_> {
        let c = self.c;
        let left: El<'_> = match self.flash {
            Some((flash, _)) => row![
                icon(icon::CHECK, 13.0, c.leaf),
                text(flash).size(size::SMALL).color(c.leaf)
            ]
            .spacing(5)
            .align_y(Alignment::Center)
            .into(),
            None => Space::new().into(),
        };
        let u = &self.transcript.usage;
        let mut line = row![container(left).width(Length::Fill)]
            .spacing(14)
            .align_y(Alignment::Center);
        if u.input + u.output > 0 {
            let part = |label: &str, value: String| text(format!("{label} {value}")).size(size::SMALL).color(c.ink3);
            line = line
                .push(part(self.s.usage_in, count(u.input)))
                .push(part(self.s.usage_out, count(u.output)));
            if u.cache_read > 0 {
                line = line.push(part(self.s.usage_cache, count(u.cache_read)));
            }
            if u.cost > 0.0 {
                line = line.push(text(format!("${:.4}", u.cost)).size(size::SMALL).color(c.ink3));
            }
        }
        container(line)
            .padding([0, 8])
            .height(18)
            .align_y(Alignment::Center)
            .into()
    }
}
