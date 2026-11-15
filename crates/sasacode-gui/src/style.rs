//! The window's colors and the look of its parts. One calm surface, one accent (the green of sasa,
//! bamboo grass), and red only where something failed or is about to be refused.

use iced::widget::{button, container, pick_list, scrollable, text_editor, text_input};
use iced::{Background, Border, Color, Shadow, Theme, border, color, theme};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colors {
    pub dark: bool,
    /// Behind the conversation.
    pub page: Color,
    /// Behind the list of conversations.
    pub side: Color,
    /// Cards: the input, an approval, a group of steps.
    pub card: Color,
    /// A quieter fill inside a card: code, output, a hovered row.
    pub well: Color,
    pub line: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub accent: Color,
    /// Text on the accent.
    pub on_accent: Color,
    /// A wash of the accent: the request, the chosen conversation.
    pub tint: Color,
    pub danger: Color,
    pub warning: Color,
    pub added: Color,
    pub removed: Color,
}

pub const LIGHT: Colors = Colors {
    dark: false,
    page: color!(0xfbfaf7),
    side: color!(0xf2f0ea),
    card: color!(0xffffff),
    well: color!(0xf3f2ed),
    line: color!(0xe1ded6),
    text: color!(0x1f2420),
    muted: color!(0x646b65),
    faint: color!(0x979d97),
    accent: color!(0x2f7d4f),
    on_accent: color!(0xffffff),
    tint: color!(0xe6f1e9),
    danger: color!(0xb3402e),
    warning: color!(0x9a6a14),
    added: color!(0xe2f2e6),
    removed: color!(0xfbe6e2),
};

pub const DARK: Colors = Colors {
    dark: true,
    page: color!(0x171a18),
    side: color!(0x121513),
    card: color!(0x1f2421),
    well: color!(0x272d29),
    line: color!(0x333a35),
    text: color!(0xe6e8e3),
    muted: color!(0xa0a7a1),
    faint: color!(0x737a74),
    accent: color!(0x74c48f),
    on_accent: color!(0x102016),
    tint: color!(0x21352a),
    danger: color!(0xe57a66),
    warning: color!(0xd9a441),
    added: color!(0x1d3526),
    removed: color!(0x40231f),
};

impl Colors {
    pub fn theme(self) -> Theme {
        Theme::custom(
            if self.dark { "sasa dark" } else { "sasa light" },
            theme::Palette {
                background: self.page,
                text: self.text,
                primary: self.accent,
                success: self.accent,
                warning: self.warning,
                danger: self.danger,
            },
        )
    }

    fn mix(a: Color, b: Color, t: f32) -> Color {
        Color {
            r: a.r + (b.r - a.r) * t,
            g: a.g + (b.g - a.g) * t,
            b: a.b + (b.b - a.b) * t,
            a: a.a + (b.a - a.a) * t,
        }
    }

    pub fn fill(self, c: Color) -> container::Style {
        container::Style {
            background: Some(c.into()),
            ..container::Style::default()
        }
    }

    /// A card: white (or its dark counterpart), a hairline, round corners.
    pub fn card(self) -> container::Style {
        container::Style {
            background: Some(self.card.into()),
            border: border::rounded(10).color(self.line).width(1),
            ..container::Style::default()
        }
    }

    /// A card that asks for attention: the approval. Set apart by the accent, not by a shadow: the
    /// software renderer leaves a shadow's edge behind when the card goes.
    pub fn raised(self) -> container::Style {
        container::Style {
            background: Some(self.card.into()),
            border: border::rounded(12).color(self.accent).width(1.5),
            ..container::Style::default()
        }
    }

    pub fn well(self) -> container::Style {
        container::Style {
            background: Some(self.well.into()),
            border: border::rounded(6),
            ..container::Style::default()
        }
    }

    pub fn request(self) -> container::Style {
        container::Style {
            background: Some(self.tint.into()),
            border: border::rounded(10),
            ..container::Style::default()
        }
    }

    pub fn alert(self) -> container::Style {
        container::Style {
            background: Some(Self::mix(self.page, self.danger, 0.1).into()),
            border: border::rounded(8),
            text_color: Some(self.danger),
            ..container::Style::default()
        }
    }

    fn button(self, status: button::Status, base: Color, hover: Color, text: Color, radius: f32) -> button::Style {
        let (background, text) = match status {
            button::Status::Active => (base, text),
            button::Status::Hovered | button::Status::Pressed => (hover, text),
            button::Status::Disabled => (
                Color {
                    a: base.a * 0.45,
                    ..base
                },
                Color { a: 0.45, ..text },
            ),
        };
        button::Style {
            background: (background.a > 0.0).then_some(Background::Color(background)),
            text_color: text,
            border: border::rounded(radius),
            ..button::Style::default()
        }
    }

    /// The one filled button of a group: send, allow, new conversation.
    pub fn primary(self) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, status| {
            let hover = Self::mix(self.accent, self.text, 0.14);
            self.button(status, self.accent, hover, self.on_accent, 8.0)
        }
    }

    /// A second choice next to a primary one.
    pub fn outline(self) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, status| {
            let mut style = self.button(status, self.card, self.well, self.text, 8.0);
            style.border = border::rounded(8).color(self.line).width(1);
            style
        }
    }

    pub fn danger_outline(self) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, status| {
            let hover = Self::mix(self.card, self.danger, 0.1);
            let mut style = self.button(status, self.card, hover, self.danger, 8.0);
            style.border = border::rounded(8)
                .color(Self::mix(self.line, self.danger, 0.5))
                .width(1);
            style
        }
    }

    /// A row that can be clicked: a conversation, a step, a small action. `chosen` keeps it lit.
    pub fn ghost(self, chosen: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, status| {
            let clear = Color::TRANSPARENT;
            let hover = Color {
                a: if self.dark { 0.07 } else { 0.05 },
                ..self.text
            };
            let base = if chosen { self.tint } else { clear };
            self.button(status, base, if chosen { self.tint } else { hover }, self.text, 7.0)
        }
    }

    pub fn editor(self) -> impl Fn(&Theme, text_editor::Status) -> text_editor::Style {
        move |_, _| text_editor::Style {
            background: Color::TRANSPARENT.into(),
            border: Border::default(),
            placeholder: self.faint,
            value: self.text,
            selection: Self::mix(self.card, self.accent, 0.3),
        }
    }

    pub fn field(self) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
        move |_, status| text_input::Style {
            background: self.card.into(),
            border: border::rounded(8)
                .color(if matches!(status, text_input::Status::Focused { .. }) {
                    self.accent
                } else {
                    self.line
                })
                .width(1),
            icon: self.muted,
            placeholder: self.faint,
            value: self.text,
            selection: Self::mix(self.card, self.accent, 0.3),
        }
    }

    /// A setting shown as a quiet word until it is opened.
    pub fn chip(self) -> impl Fn(&Theme, pick_list::Status) -> pick_list::Style {
        move |_, status| pick_list::Style {
            text_color: self.muted,
            placeholder_color: self.muted,
            handle_color: self.faint,
            background: match status {
                pick_list::Status::Active => Color::TRANSPARENT.into(),
                _ => self.well.into(),
            },
            border: border::rounded(7),
        }
    }

    pub fn menu(self) -> impl Fn(&Theme) -> iced::overlay::menu::Style {
        move |_| iced::overlay::menu::Style {
            background: self.card.into(),
            border: border::rounded(8).color(self.line).width(1),
            text_color: self.text,
            selected_text_color: self.text,
            selected_background: self.tint.into(),
            shadow: Shadow::default(),
        }
    }

    pub fn scroll(self) -> impl Fn(&Theme, scrollable::Status) -> scrollable::Style {
        move |theme, status| {
            let mut style = scrollable::default(theme, status);
            let lit = !matches!(status, scrollable::Status::Active { .. });
            let rail = scrollable::Rail {
                background: None,
                border: Border::default(),
                scroller: scrollable::Scroller {
                    background: Color {
                        a: if lit { 0.35 } else { 0.16 },
                        ..self.text
                    }
                    .into(),
                    border: border::rounded(4),
                },
            };
            style.vertical_rail = rail;
            style.horizontal_rail = rail;
            style
        }
    }
}
