//! The window's colors, type and the look of its parts.
//!
//! The colors come from sasa (bamboo grass): a pale ground the shade of the back of a leaf, ink that
//! is nearly black with green in it, the leaf's green where the window acts or says "done", the straw
//! of a leaf's dried edge where it asks, and vermilion only where something failed or is refused.
//! Depth is told by the surface a thing sits on (ground, sheet, well), never by a shadow.

use iced::font::Weight;
use iced::widget::{button, container, scrollable, text_editor, text_input};
use iced::{Background, Border, Color, Font, Theme, border, color, theme};

/// The family every word is set in. A Japanese family is named outright: left to the system, a Mac
/// whose language is not Japanese draws kanji with Chinese shapes.
#[cfg(target_os = "macos")]
const FAMILY: &str = "Hiragino Sans";
#[cfg(windows)]
const FAMILY: &str = "Yu Gothic UI";
#[cfg(not(any(target_os = "macos", windows)))]
const FAMILY: &str = "Noto Sans CJK JP";

pub const UI: Font = Font::with_name(FAMILY);
pub const MEDIUM: Font = Font {
    weight: Weight::Medium,
    ..UI
};
pub const BOLD: Font = Font {
    weight: Weight::Semibold,
    ..UI
};

/// Code, commands and paths.
#[cfg(target_os = "macos")]
pub const MONO: Font = Font::with_name("Menlo");
#[cfg(windows)]
pub const MONO: Font = Font::with_name("Consolas");
#[cfg(not(any(target_os = "macos", windows)))]
pub const MONO: Font = Font::MONOSPACE;

/// Type sizes. Reading text is the largest thing on a working screen; nothing shouts above it.
pub mod size {
    /// Key hints and counts.
    pub const TINY: f32 = 11.0;
    /// Notes under and beside things.
    pub const SMALL: f32 = 12.0;
    /// The window's own words: rows, buttons, labels.
    pub const BASE: f32 = 13.0;
    /// What is read: requests and answers.
    pub const READ: f32 = 15.0;
    /// The one line that opens an empty conversation.
    pub const LEAD: f32 = 20.0;
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colors {
    pub dark: bool,
    /// The window itself, and the list of conversations that sits on it.
    pub ground: Color,
    /// The sheet the conversation is on.
    pub sheet: Color,
    /// A hollow in the sheet: the input, code, output, a hovered row.
    pub well: Color,
    /// A pressed or chosen row.
    pub press: Color,
    /// A line that separates.
    pub hair: Color,
    /// A line that outlines.
    pub edge: Color,
    pub ink: Color,
    pub ink2: Color,
    /// The quietest ink that is still read.
    pub ink3: Color,
    /// Placeholders and marks, never sentences.
    pub ink4: Color,
    pub leaf: Color,
    pub leaf_hover: Color,
    pub on_leaf: Color,
    pub leaf_tint: Color,
    /// The slip that asks for approval.
    pub kuma: Color,
    pub kuma_edge: Color,
    pub amber: Color,
    pub shu: Color,
    pub shu_tint: Color,
    pub added: Color,
    pub removed: Color,
    pub added_ink: Color,
    pub removed_ink: Color,
}

pub const LIGHT: Colors = Colors {
    dark: false,
    ground: color!(0xe7ece6),
    sheet: color!(0xfcfdfb),
    well: color!(0xf0f3ef),
    press: color!(0xe3e9e2),
    hair: color!(0xdee4dd),
    edge: color!(0xc6cfc7),
    ink: color!(0x17201a),
    ink2: color!(0x44504a),
    ink3: color!(0x5f6b64),
    ink4: color!(0x8a948d),
    leaf: color!(0x2a6a47),
    leaf_hover: color!(0x225a3b),
    on_leaf: color!(0xf5f7ee),
    leaf_tint: color!(0xdceadf),
    kuma: color!(0xf5f0d6),
    kuma_edge: color!(0xddd3a0),
    amber: color!(0x7d5600),
    shu: color!(0xb03a26),
    shu_tint: color!(0xf8e4df),
    added: color!(0xdaeedf),
    removed: color!(0xf8dfda),
    added_ink: color!(0x1e6a3e),
    removed_ink: color!(0xa03322),
};

pub const DARK: Colors = Colors {
    dark: true,
    ground: color!(0x0d120f),
    sheet: color!(0x141a16),
    well: color!(0x1b231f),
    press: color!(0x242e28),
    hair: color!(0x242d28),
    edge: color!(0x34413a),
    ink: color!(0xe4eae5),
    ink2: color!(0xb7c2ba),
    ink3: color!(0x8e9a92),
    ink4: color!(0x5f6b64),
    leaf: color!(0x7cc49a),
    leaf_hover: color!(0x92d2ac),
    on_leaf: color!(0x0d1f15),
    leaf_tint: color!(0x1c3226),
    kuma: color!(0x292716),
    kuma_edge: color!(0x4c4622),
    amber: color!(0xe3b655),
    shu: color!(0xf0876f),
    shu_tint: color!(0x3a211c),
    added: color!(0x173322),
    removed: color!(0x3c211c),
    added_ink: color!(0x7cc49a),
    removed_ink: color!(0xf0876f),
};

/// A scrollbar that is a thin line at the edge until it is needed.
pub fn thin() -> scrollable::Direction {
    scrollable::Direction::Vertical(scrollable::Scrollbar::new().width(4).scroller_width(4).margin(3))
}

pub fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

fn faded(c: Color, a: f32) -> Color {
    Color { a: c.a * a, ..c }
}

type ButtonStyle = Box<dyn Fn(&Theme, button::Status) -> button::Style>;

impl Colors {
    pub fn theme(self) -> Theme {
        Theme::custom(
            if self.dark { "sasa dark" } else { "sasa light" },
            theme::Palette {
                background: self.ground,
                text: self.ink,
                primary: self.leaf,
                success: self.leaf,
                warning: self.amber,
                danger: self.shu,
            },
        )
    }

    // ---- surfaces ----

    pub fn fill(self, c: Color) -> container::Style {
        container::Style {
            background: Some(c.into()),
            ..container::Style::default()
        }
    }

    /// The sheet the conversation is on, set into the ground.
    pub fn sheet(self) -> container::Style {
        container::Style {
            background: Some(self.sheet.into()),
            border: border::rounded(10).color(self.hair).width(1),
            ..container::Style::default()
        }
    }

    pub fn well(self, radius: f32) -> container::Style {
        container::Style {
            background: Some(self.well.into()),
            border: border::rounded(radius),
            ..container::Style::default()
        }
    }

    /// The input: a well with an outline, so where to write is never in doubt.
    pub fn input(self) -> container::Style {
        container::Style {
            background: Some(self.well.into()),
            border: border::rounded(14).color(self.hair).width(1),
            ..container::Style::default()
        }
    }

    /// The slip that asks.
    pub fn slip(self) -> container::Style {
        container::Style {
            background: Some(self.kuma.into()),
            border: border::rounded(14).color(self.kuma_edge).width(1),
            ..container::Style::default()
        }
    }

    /// Code and changes on the slip: the sheet's color, so the slip's own stays the frame.
    pub fn inset(self) -> container::Style {
        container::Style {
            background: Some(self.sheet.into()),
            border: border::rounded(8).color(self.kuma_edge).width(1),
            ..container::Style::default()
        }
    }

    pub fn alert(self) -> container::Style {
        container::Style {
            background: Some(self.shu_tint.into()),
            border: border::rounded(8),
            text_color: Some(self.shu),
            ..container::Style::default()
        }
    }

    /// The command list, above the dimmed window.
    pub fn floating(self) -> container::Style {
        container::Style {
            background: Some(self.sheet.into()),
            border: border::rounded(14).color(self.edge).width(1),
            ..container::Style::default()
        }
    }

    /// One ring of the soft edge around the command list, `step` rings out from it. Rings and not a
    /// shadow: the software renderer paints a shadow again on every partial redraw.
    pub fn halo(self, step: u8) -> container::Style {
        let (alpha, radius) = match step {
            0 => (0.10, 15.0),
            1 => (0.06, 17.0),
            _ => (0.035, 21.0),
        };
        container::Style {
            background: Some(
                Color {
                    a: if self.dark { alpha * 2.2 } else { alpha },
                    ..Color::BLACK
                }
                .into(),
            ),
            border: border::rounded(radius),
            ..container::Style::default()
        }
    }

    pub fn scrim(self) -> container::Style {
        self.fill(Color {
            a: if self.dark { 0.5 } else { 0.22 },
            ..color!(0x0a120d)
        })
    }

    pub fn tooltip(self) -> container::Style {
        container::Style {
            background: Some(self.ink.into()),
            text_color: Some(self.sheet),
            border: border::rounded(6),
            ..container::Style::default()
        }
    }

    /// A key's name, as on a keycap.
    pub fn keycap(self) -> container::Style {
        container::Style {
            border: border::rounded(4).color(self.edge).width(1),
            text_color: Some(self.ink3),
            ..container::Style::default()
        }
    }

    /// A mark on the stalk: a disc, or a ring when `hollow`. The sheet-colored rim keeps the stalk's
    /// line from touching it.
    pub fn node(self, c: Color, hollow: bool) -> (container::Style, container::Style) {
        let rim = container::Style {
            background: Some(self.sheet.into()),
            border: border::rounded(7),
            ..container::Style::default()
        };
        let mark = container::Style {
            background: Some(if hollow { self.sheet } else { c }.into()),
            border: border::rounded(5).color(c).width(if hollow { 1.5 } else { 0.0 }),
            ..container::Style::default()
        };
        (rim, mark)
    }

    // ---- buttons ----

    fn button(
        self,
        status: button::Status,
        base: Color,
        hover: Color,
        press: Color,
        ink: Color,
        r: f32,
    ) -> button::Style {
        let (background, ink) = match status {
            button::Status::Active => (base, ink),
            button::Status::Hovered => (hover, ink),
            button::Status::Pressed => (press, ink),
            button::Status::Disabled => (faded(base, 0.5), faded(ink, 0.45)),
        };
        button::Style {
            background: (background.a > 0.0).then_some(Background::Color(background)),
            text_color: ink,
            border: border::rounded(r),
            ..button::Style::default()
        }
    }

    /// The one filled button of a group: send, allow.
    pub fn primary(self) -> ButtonStyle {
        Box::new(move |_, status| {
            let press = mix(self.leaf_hover, self.ink, 0.12);
            self.button(status, self.leaf, self.leaf_hover, press, self.on_leaf, 8.0)
        })
    }

    /// Stop: the heaviest thing on the sheet while a run is on.
    pub fn solid(self) -> ButtonStyle {
        Box::new(move |_, status| {
            let hover = mix(self.ink, self.sheet, 0.18);
            self.button(status, self.ink, hover, self.ink2, self.sheet, 8.0)
        })
    }

    /// A second choice beside a primary one. `on` is the surface it sits on.
    pub fn outline(self, on: Color, ink: Color) -> ButtonStyle {
        Box::new(move |_, status| {
            let hover = mix(on, ink, 0.07);
            let press = mix(on, ink, 0.13);
            let mut style = self.button(status, Color::TRANSPARENT, hover, press, ink, 8.0);
            style.border = border::rounded(8).color(mix(on, ink, 0.32)).width(1);
            style
        })
    }

    /// Anything else that can be pressed: a row, a small action, a setting. `chosen` keeps it lit.
    /// `on` is the surface it sits on; the lit color is one step away from it.
    pub fn quiet(self, on: Color, chosen: bool) -> ButtonStyle {
        Box::new(move |_, status| {
            let (hover, press) = if on == self.ground {
                (mix(self.ground, self.sheet, 0.55), self.sheet)
            } else if on == self.well {
                (self.press, mix(self.press, self.ink, 0.06))
            } else if on == self.kuma {
                (mix(self.kuma, self.ink, 0.06), mix(self.kuma, self.ink, 0.11))
            } else {
                (self.well, self.press)
            };
            let base = if chosen { press } else { Color::TRANSPARENT };
            self.button(status, base, if chosen { press } else { hover }, press, self.ink, 6.0)
        })
    }

    // ---- fields ----

    pub fn editor(self) -> impl Fn(&Theme, text_editor::Status) -> text_editor::Style {
        move |_, _| text_editor::Style {
            background: Color::TRANSPARENT.into(),
            border: Border::default(),
            placeholder: self.ink4,
            value: self.ink,
            selection: self.selection(),
        }
    }

    /// A field with nothing drawn around it: the surface it sits on is its frame.
    pub fn bare(self) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
        move |_, _| text_input::Style {
            background: Color::TRANSPARENT.into(),
            border: Border::default(),
            icon: self.ink3,
            placeholder: self.ink4,
            value: self.ink,
            selection: self.selection(),
        }
    }

    /// A field on the slip.
    pub fn field(self) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
        move |_, status| text_input::Style {
            background: self.sheet.into(),
            border: border::rounded(8)
                .color(if matches!(status, text_input::Status::Focused { .. }) {
                    self.leaf
                } else {
                    self.kuma_edge
                })
                .width(1),
            icon: self.ink3,
            placeholder: self.ink4,
            value: self.ink,
            selection: self.selection(),
        }
    }

    fn selection(self) -> Color {
        mix(self.sheet, self.leaf, if self.dark { 0.38 } else { 0.26 })
    }

    pub fn scroll(self) -> impl Fn(&Theme, scrollable::Status) -> scrollable::Style {
        move |theme, status| {
            let mut style = scrollable::default(theme, status);
            let lit = !matches!(status, scrollable::Status::Active { .. });
            let rail = scrollable::Rail {
                background: None,
                border: Border::default(),
                scroller: scrollable::Scroller {
                    background: faded(self.ink, if lit { 0.32 } else { 0.14 }).into(),
                    border: border::rounded(4),
                },
            };
            style.container = container::Style::default();
            style.vertical_rail = rail;
            style.horizontal_rail = rail;
            style
        }
    }
}
