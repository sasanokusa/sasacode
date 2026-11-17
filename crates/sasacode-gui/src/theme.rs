//! The window's colours and the look of its parts. One set of colours for light and one for dark;
//! everything drawn takes its colours from here, not from iced's built-in themes.
//!
//! The look follows Solid Glass (github.com/deeplethe/solid-glass, MIT): neutral greys with no
//! tint of their own, panes finished at the edge with a fine line and a soft shadow under them,
//! one solid "ink" button, and colour kept for what it says (the state of a run, a change, a link).
//! Its colours are oklch and partly clear; here each is the opaque colour it makes over what it is
//! drawn on, since iced draws no blur behind a pane.

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
    /// What is set in: code, output.
    pub inset: Color,
    pub line: Color,
    pub line_2: Color,
    pub fg: Color,
    pub fg_2: Color,
    pub fg_3: Color,
    /// sasacode's own colour: what is running, done, new.
    pub accent: Color,
    pub on_accent: Color,
    pub accent_wash: Color,
    /// The one solid button of a group, and a switch that is on.
    pub ink: Color,
    pub on_ink: Color,
    /// Where a link goes.
    pub link: Color,
    /// What is chosen or under the pointer: a conversation, a tab, a row of a menu.
    pub state: Color,
    /// Text chosen in an input.
    pub select: Color,
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
    bg: hex(0xE4E6EB),
    surface: hex(0xECEDF1),
    raised: hex(0xF5F6F9),
    inset: hex(0xD8DADF),
    line: hex(0xCDD0D6),
    line_2: hex(0xBBBEC4),
    fg: hex(0x0D0D0D),
    fg_2: hex(0x383838),
    fg_3: hex(0x585858),
    accent: hex(0x006C4E),
    on_accent: hex(0xFCFCFC),
    accent_wash: hex(0xD9EDE9),
    ink: hex(0x262727),
    on_ink: hex(0xFAFAFA),
    link: hex(0x0060C1),
    state: hex(0xDADCE1),
    select: hex(0xC4D8EE),
    warn: hex(0x944E00),
    warn_solid: hex(0xD87400),
    warn_wash: hex(0xF1E6D9),
    warn_line: hex(0xEAC295),
    err: hex(0xC51E21),
    err_wash: hex(0xF4DEE1),
    err_line: hex(0xF1ADB2),
    add: hex(0x10703A),
    add_wash: hex(0xD7E9E2),
    del: hex(0xC51E21),
    del_wash: hex(0xF4DEE1),
};

pub const DARK: Tokens = Tokens {
    dark: true,
    bg: hex(0x0D0D0D),
    surface: hex(0x141414),
    raised: hex(0x222222),
    inset: hex(0x1A1A1A),
    line: hex(0x3A3A3A),
    line_2: hex(0x4E4E4E),
    fg: hex(0xF2F2F2),
    fg_2: hex(0xC7C7C7),
    fg_3: hex(0xA4A4A4),
    accent: hex(0x54D2A4),
    on_accent: hex(0x071A13),
    accent_wash: hex(0x293B34),
    ink: hex(0xD5D5D5),
    on_ink: hex(0x121212),
    link: hex(0x73B6FA),
    // Over a card, where a menu's chosen row is: on the panels it is lighter still.
    state: hex(0x333333),
    select: hex(0x2D4964),
    warn: hex(0xF79C40),
    warn_solid: hex(0xED9235),
    warn_wash: hex(0x403326),
    warn_line: hex(0x73502E),
    err: hex(0xFD7273),
    err_wash: hex(0x432C2D),
    err_line: hex(0x763B3C),
    add: hex(0x5BCC80),
    add_wash: hex(0x28392E),
    del: hex(0xFD7273),
    del_wash: hex(0x432C2D),
};

/// A control's corners, and a pane's.
pub const RADIUS: f32 = 10.0;
pub const RADIUS_L: f32 = 14.0;

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
            // A switch that is on, a box that is ticked: ink, as the solid button.
            primary: self.ink,
            success: self.add,
            warning: self.warn_solid,
            danger: self.err,
        }
    }

    /// A shadow of black at `a` in the light (`a_dark` in the dark), `y` down, `blur` wide.
    fn shade(self, a: f32, a_dark: f32, y: f32, blur: f32) -> Shadow {
        Shadow {
            color: Color {
                a: if self.dark { a_dark } else { a },
                ..Color::BLACK
            },
            offset: Vector::new(0.0, y),
            blur_radius: blur,
        }
    }

    /// Where a pane meets what it stands on: a line of shadow just under it.
    fn contact(self) -> Shadow {
        self.shade(0.07, 0.4, 1.0, 3.0)
    }

    /// A filled area with no border.
    pub fn fill(self, color: Color) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style {
            background: Some(Background::Color(color)),
            text_color: Some(self.fg),
            ..container::Style::default()
        }
    }

    /// A bordered box. One that stands up (a card, filled `raised`) has a shadow under it; one set
    /// in has none.
    pub fn boxed(self, fill: Color, line: Color, radius: f32) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style {
            background: Some(Background::Color(fill)),
            text_color: Some(self.fg),
            border: Border {
                color: line,
                width: 1.0,
                radius: border::radius(radius),
            },
            shadow: if fill == self.raised {
                self.contact()
            } else {
                Shadow::default()
            },
            ..container::Style::default()
        }
    }

    /// A box that floats above the rest: the approval card, the palette. Thick glass: a deep, soft
    /// shadow under it.
    pub fn floating(self, fill: Color, line: Color) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style {
            background: Some(Background::Color(fill)),
            text_color: Some(self.fg),
            border: Border {
                color: line,
                width: 1.0,
                radius: border::radius(RADIUS_L),
            },
            shadow: self.shade(0.16, 0.6, 14.0, 36.0),
            ..container::Style::default()
        }
    }

    /// A small label with a tinted background, round at the ends.
    pub fn tag(self, fill: Color, text: Color) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style {
            background: Some(Background::Color(fill)),
            text_color: Some(text),
            border: border::rounded(7.0),
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

    /// The one solid button of a group: ink, with a fine edge and a shadow under it.
    pub fn primary(self) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, status| {
            let base = button::Style {
                background: Some(Background::Color(self.ink)),
                text_color: self.on_ink,
                border: Border {
                    color: mix(self.ink, self.fg, 0.3),
                    width: 1.0,
                    radius: border::radius(RADIUS),
                },
                shadow: self.shade(0.22, 0.45, 1.0, 3.0),
                ..button::Style::default()
            };
            match status {
                button::Status::Hovered | button::Status::Pressed => button::Style {
                    background: Some(Background::Color(mix(self.ink, self.bg, 0.14))),
                    ..base
                },
                button::Status::Disabled => button::Style {
                    background: Some(Background::Color(self.inset)),
                    text_color: self.fg_3,
                    border: Border {
                        color: self.line,
                        ..base.border
                    },
                    shadow: Shadow::default(),
                    ..base
                },
                button::Status::Active => base,
            }
        }
    }

    /// A button of glass: the card's colour, a fine edge, standing a little off what it is on.
    pub fn outline(self, text: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, status| {
            let base = button::Style {
                background: Some(Background::Color(self.raised)),
                text_color: text,
                border: Border {
                    color: self.line,
                    width: 1.0,
                    radius: border::radius(RADIUS),
                },
                shadow: self.contact(),
                ..button::Style::default()
            };
            match status {
                button::Status::Hovered | button::Status::Pressed => button::Style {
                    background: Some(Background::Color(mix(self.raised, self.state, 0.6))),
                    border: Border {
                        color: self.line_2,
                        ..base.border
                    },
                    ..base
                },
                button::Status::Disabled => button::Style {
                    text_color: self.fg_3,
                    shadow: Shadow::default(),
                    ..base
                },
                button::Status::Active => base,
            }
        }
    }

    /// A button that is only its content until the pointer is over it. `on` keeps it lit (the
    /// open conversation, the chosen tab): one state, the same grey for both.
    pub fn ghost(self, text: Color, on: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, status| {
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let fill = if on {
                Some(self.state)
            } else if hot {
                Some(Color { a: 0.7, ..self.state })
            } else {
                None
            };
            button::Style {
                background: fill.map(Background::Color),
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
            selection: self.select,
        }
    }

    /// A field: the card's colour set into what holds it; focused, its edge darkens to a line of
    /// ink rather than taking a colour.
    pub fn input(self) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
        move |_, status| text_input::Style {
            background: Background::Color(self.raised),
            border: Border {
                color: match status {
                    text_input::Status::Focused { .. } => mix(self.ink, self.raised, 0.45),
                    text_input::Status::Hovered => self.line_2,
                    _ => self.line,
                },
                width: 1.0,
                radius: border::radius(RADIUS),
            },
            icon: self.fg_3,
            placeholder: self.fg_3,
            value: self.fg,
            selection: self.select,
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
            selection: self.select,
        }
    }

    pub fn pick(self, text: Color) -> impl Fn(&Theme, pick_list::Status) -> pick_list::Style {
        move |_, status| pick_list::Style {
            text_color: text,
            placeholder_color: self.fg_3,
            handle_color: self.fg_3,
            background: Background::Color(match status {
                pick_list::Status::Active => Color::TRANSPARENT,
                _ => self.state,
            }),
            border: border::rounded(RADIUS),
        }
    }

    /// A menu that opens over the rest: a popover, the thickest glass.
    pub fn menu(self) -> impl Fn(&Theme) -> menu::Style {
        move |_| menu::Style {
            background: Background::Color(self.raised),
            border: Border {
                color: self.line,
                width: 1.0,
                radius: border::radius(RADIUS),
            },
            text_color: self.fg,
            selected_text_color: self.fg,
            selected_background: Background::Color(self.state),
            shadow: self.shade(0.14, 0.55, 10.0, 28.0),
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
                (t.on_ink, t.ink),
                (t.link, t.raised),
                (t.link, t.bg),
                (t.fg, t.state),
                (t.fg_3, t.state),
                (t.fg, t.select),
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
