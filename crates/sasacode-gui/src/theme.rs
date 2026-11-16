//! The window's colours and the look of its parts. One set of colours for light and one for dark;
//! everything drawn takes its colours from here, not from iced's built-in themes.

use iced::widget::{button, container, overlay::menu, pick_list, rule, scrollable, text_editor, text_input};
use iced::{Background, Border, Color, Shadow, Theme, Vector, border, theme::Palette};

const fn hex(v: u32) -> Color {
    Color::from_rgb8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    pub dark: bool,
    /// The window behind everything.
    pub bg: Color,
    /// Panels: the sidebar, the inspector, the bars.
    pub surface: Color,
    /// What sits on top: cards, the input.
    pub raised: Color,
    /// What is set in: code, output, the user's message.
    pub inset: Color,
    pub line: Color,
    pub line_2: Color,
    pub fg: Color,
    pub fg_2: Color,
    pub fg_3: Color,
    pub accent: Color,
    pub on_accent: Color,
    pub accent_wash: Color,
    pub warn: Color,
    pub warn_solid: Color,
    pub warn_wash: Color,
    pub warn_line: Color,
    pub err: Color,
    pub err_wash: Color,
    pub err_line: Color,
    pub add: Color,
    pub add_wash: Color,
    pub del: Color,
    pub del_wash: Color,
}

pub const LIGHT: Tokens = Tokens {
    dark: false,
    bg: hex(0xF2F4EF),
    surface: hex(0xFBFCFA),
    raised: hex(0xFFFFFF),
    inset: hex(0xEAEDE6),
    line: hex(0xDDE2D8),
    line_2: hex(0xC6CDC0),
    fg: hex(0x181D19),
    fg_2: hex(0x4D584F),
    fg_3: hex(0x626C63),
    accent: hex(0x2A6B47),
    on_accent: hex(0xFFFFFF),
    accent_wash: hex(0xDFEDE3),
    warn: hex(0x8F5208),
    warn_solid: hex(0xD4861C),
    warn_wash: hex(0xFAF0DB),
    warn_line: hex(0xE8CE98),
    err: hex(0xAC332B),
    err_wash: hex(0xFAE9E6),
    err_line: hex(0xEABDB7),
    add: hex(0x1C6E38),
    add_wash: hex(0xE2F2E6),
    del: hex(0xAC332B),
    del_wash: hex(0xFBEAE7),
};

pub const DARK: Tokens = Tokens {
    dark: true,
    bg: hex(0x111512),
    surface: hex(0x161B17),
    raised: hex(0x1E241F),
    inset: hex(0x222923),
    line: hex(0x29302A),
    line_2: hex(0x3A443B),
    fg: hex(0xE2E7E0),
    fg_2: hex(0xA7B2A7),
    fg_3: hex(0x8C978D),
    accent: hex(0x84C59A),
    on_accent: hex(0x0E1610),
    accent_wash: hex(0x1D2E23),
    warn: hex(0xE6B261),
    warn_solid: hex(0xDFA043),
    warn_wash: hex(0x2C2513),
    warn_line: hex(0x5A4820),
    err: hex(0xEF867B),
    err_wash: hex(0x311A17),
    err_line: hex(0x6A2F29),
    add: hex(0x7BC591),
    add_wash: hex(0x15271B),
    del: hex(0xEF867B),
    del_wash: hex(0x301917),
};

pub const RADIUS: f32 = 6.0;
pub const RADIUS_L: f32 = 10.0;

impl Tokens {
    /// The iced theme the built-in parts (markdown, selections, menus) take their colours from.
    pub fn theme(&self) -> Theme {
        Theme::custom(
            if self.dark { "sasacode dark" } else { "sasacode light" },
            self.palette(),
        )
    }

    pub fn palette(&self) -> Palette {
        Palette {
            background: self.bg,
            text: self.fg,
            primary: self.accent,
            success: self.add,
            warning: self.warn_solid,
            danger: self.err,
        }
    }

    /// A filled area with no border.
    pub fn fill(self, color: Color) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style {
            background: Some(Background::Color(color)),
            text_color: Some(self.fg),
            ..container::Style::default()
        }
    }

    /// A bordered box.
    pub fn boxed(self, fill: Color, line: Color, radius: f32) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style {
            background: Some(Background::Color(fill)),
            text_color: Some(self.fg),
            border: Border {
                color: line,
                width: 1.0,
                radius: border::radius(radius),
            },
            ..container::Style::default()
        }
    }

    /// A box that floats above the rest: the approval card, the palette.
    pub fn floating(self, fill: Color, line: Color) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style {
            background: Some(Background::Color(fill)),
            text_color: Some(self.fg),
            border: Border {
                color: line,
                width: 1.0,
                radius: border::radius(RADIUS_L),
            },
            shadow: Shadow {
                color: Color {
                    a: if self.dark { 0.45 } else { 0.12 },
                    ..Color::BLACK
                },
                offset: Vector::new(0.0, 6.0),
                blur_radius: 22.0,
            },
            ..container::Style::default()
        }
    }

    /// A small label with a tinted background.
    pub fn tag(self, fill: Color, text: Color) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style {
            background: Some(Background::Color(fill)),
            text_color: Some(text),
            border: border::rounded(4.0),
            ..container::Style::default()
        }
    }

    pub fn rule(self, color: Color) -> impl Fn(&Theme) -> rule::Style {
        move |_| rule::Style {
            color,
            radius: border::radius(0.0),
            fill_mode: rule::FillMode::Full,
            snap: true,
        }
    }

    /// The one filled button of a group.
    pub fn primary(self) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, status| {
            let base = button::Style {
                background: Some(Background::Color(self.accent)),
                text_color: self.on_accent,
                border: border::rounded(RADIUS),
                ..button::Style::default()
            };
            match status {
                button::Status::Hovered | button::Status::Pressed => button::Style {
                    background: Some(Background::Color(mix(self.accent, self.fg, 0.14))),
                    ..base
                },
                button::Status::Disabled => button::Style {
                    background: Some(Background::Color(self.line)),
                    text_color: self.fg_3,
                    ..base
                },
                button::Status::Active => base,
            }
        }
    }

    /// A button with an outline.
    pub fn outline(self, text: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, status| {
            let base = button::Style {
                background: Some(Background::Color(self.raised)),
                text_color: text,
                border: Border {
                    color: self.line_2,
                    width: 1.0,
                    radius: border::radius(RADIUS),
                },
                ..button::Style::default()
            };
            match status {
                button::Status::Hovered | button::Status::Pressed => button::Style {
                    background: Some(Background::Color(self.inset)),
                    ..base
                },
                button::Status::Disabled => button::Style {
                    text_color: self.fg_3,
                    border: Border {
                        color: self.line,
                        ..base.border
                    },
                    ..base
                },
                button::Status::Active => base,
            }
        }
    }

    /// A button that is only its content until the pointer is over it. `on` keeps it lit (the
    /// open conversation, the chosen tab).
    pub fn ghost(self, text: Color, on: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, status| {
            let lit = on || matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: lit.then_some(Background::Color(if on { self.accent_wash } else { self.inset })),
                text_color: if status == button::Status::Disabled {
                    self.fg_3
                } else {
                    text
                },
                border: border::rounded(RADIUS),
                ..button::Style::default()
            }
        }
    }

    pub fn editor(self) -> impl Fn(&Theme, text_editor::Status) -> text_editor::Style {
        move |_, _| text_editor::Style {
            background: Background::Color(Color::TRANSPARENT),
            border: Border::default(),
            placeholder: self.fg_3,
            value: self.fg,
            selection: self.accent_wash,
        }
    }

    pub fn input(self) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
        move |_, status| text_input::Style {
            background: Background::Color(self.raised),
            border: Border {
                color: match status {
                    text_input::Status::Focused { .. } => self.accent,
                    text_input::Status::Hovered => self.line_2,
                    _ => self.line,
                },
                width: 1.0,
                radius: border::radius(RADIUS),
            },
            icon: self.fg_3,
            placeholder: self.fg_3,
            value: self.fg,
            selection: self.accent_wash,
        }
    }

    /// An input with no box of its own: what holds it draws the box.
    pub fn input_bare(self) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
        move |_, _| text_input::Style {
            background: Background::Color(Color::TRANSPARENT),
            border: Border::default(),
            icon: self.fg_3,
            placeholder: self.fg_3,
            value: self.fg,
            selection: self.accent_wash,
        }
    }

    pub fn pick(self, text: Color) -> impl Fn(&Theme, pick_list::Status) -> pick_list::Style {
        move |_, status| pick_list::Style {
            text_color: text,
            placeholder_color: self.fg_3,
            handle_color: self.fg_3,
            background: Background::Color(match status {
                pick_list::Status::Active => Color::TRANSPARENT,
                _ => self.inset,
            }),
            border: border::rounded(RADIUS),
        }
    }

    pub fn menu(self) -> impl Fn(&Theme) -> menu::Style {
        move |_| menu::Style {
            background: Background::Color(self.raised),
            border: Border {
                color: self.line_2,
                width: 1.0,
                radius: border::radius(RADIUS),
            },
            text_color: self.fg,
            selected_text_color: self.fg,
            selected_background: Background::Color(self.accent_wash),
            shadow: Shadow {
                color: Color {
                    a: if self.dark { 0.4 } else { 0.1 },
                    ..Color::BLACK
                },
                offset: Vector::new(0.0, 4.0),
                blur_radius: 14.0,
            },
        }
    }

    /// A thin scrollbar that shows only where there is something to scroll.
    pub fn scroll(self) -> impl Fn(&Theme, scrollable::Status) -> scrollable::Style {
        move |theme, status| {
            let mut style = scrollable::default(theme, status);
            let hot = matches!(
                status,
                scrollable::Status::Hovered { .. } | scrollable::Status::Dragged { .. }
            );
            for rail in [&mut style.vertical_rail, &mut style.horizontal_rail] {
                rail.background = None;
                rail.border = Border::default();
                rail.scroller.background = Background::Color(if hot { self.line_2 } else { self.line });
                rail.scroller.border = border::rounded(4.0);
            }
            style.container = container::Style::default();
            style
        }
    }
}

/// `a` with `t` of `b` mixed in.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance.
    fn luminance(c: Color) -> f32 {
        let f = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
    }

    fn contrast(a: Color, b: Color) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn text_is_readable_on_what_it_is_drawn_on() {
        for t in [LIGHT, DARK] {
            let pairs = [
                (t.fg, t.bg),
                (t.fg, t.inset),
                (t.fg_2, t.bg),
                (t.fg_2, t.surface),
                (t.fg_3, t.surface),
                (t.fg_3, t.bg),
                (t.fg_3, t.inset),
                (t.fg_3, t.raised),
                (t.warn, t.raised),
                (t.err, t.raised),
                (t.accent, t.accent_wash),
                (t.accent, t.surface),
                (t.on_accent, t.accent),
                (t.warn, t.warn_wash),
                (t.err, t.err_wash),
                (t.add, t.add_wash),
                (t.del, t.del_wash),
            ];
            for (text, on) in pairs {
                assert!(
                    contrast(text, on) >= 4.5,
                    "dark={} {text:?} on {on:?}: {}",
                    t.dark,
                    contrast(text, on)
                );
            }
        }
    }
}
