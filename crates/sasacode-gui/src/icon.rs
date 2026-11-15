//! The window's icons: the few Lucide icons it uses, as a font (`assets/icons.ttf`, built by
//! `assets/icons.py`; Lucide's license is beside it). One set, one stroke, colored like text.

use iced::widget::{Text, text};
use iced::{Color, Font};

pub const BYTES: &[u8] = include_bytes!("../assets/icons.ttf");
const FONT: Font = Font::with_name("sasacode-icons");

pub const SQUARE_PEN: char = '\u{e172}';
pub const SEARCH: char = '\u{e151}';
pub const PANEL_LEFT: char = '\u{e12a}';
pub const CHEVRON_RIGHT: char = '\u{e06f}';
pub const CHEVRON_DOWN: char = '\u{e06d}';
pub const CHECK: char = '\u{e06c}';
pub const X: char = '\u{e1b2}';
pub const TERMINAL: char = '\u{e181}';
pub const FILE_TEXT: char = '\u{e0cc}';
pub const PENCIL: char = '\u{e1f9}';
pub const FILE_PLUS: char = '\u{e0c9}';
pub const GLOBE: char = '\u{e0e8}';
pub const BOT: char = '\u{e1bb}';
pub const BRAIN: char = '\u{e3c6}';
pub const WRENCH: char = '\u{e1b1}';
pub const SHIELD: char = '\u{e158}';
pub const SHIELD_ALERT: char = '\u{e1fe}';
pub const SHIELD_CHECK: char = '\u{e1ff}';
pub const HAND: char = '\u{e1d7}';
pub const GAUGE: char = '\u{e1bf}';
pub const BOX: char = '\u{e061}';
pub const ARROW_UP: char = '\u{e04a}';
pub const SQUARE: char = '\u{e167}';
pub const COPY: char = '\u{e09e}';
pub const GIT_BRANCH: char = '\u{e0e2}';
pub const FILE_DIFF: char = '\u{e317}';
pub const CIRCLE_ALERT: char = '\u{e077}';
pub const CORNER_DOWN_LEFT: char = '\u{e0a1}';
pub const LIST_CHECKS: char = '\u{e1d0}';
pub const CLOCK: char = '\u{e087}';
pub const MESSAGE_SQUARE: char = '\u{e117}';
pub const INFO: char = '\u{e0f9}';

/// An icon `size` pixels square.
pub fn icon<'a>(glyph: char, size: f32, color: Color) -> Text<'a> {
    text(glyph)
        .font(FONT)
        .size(size)
        .line_height(1.0)
        .width(size)
        .center()
        .color(color)
}

/// The icon of a tool's step.
pub fn tool(name: &str) -> char {
    match name {
        "bash" | "bg_start" | "bg_output" | "bg_stop" => TERMINAL,
        "read" => FILE_TEXT,
        "edit" => PENCIL,
        "write" => FILE_PLUS,
        "web_fetch" | "web_search" => GLOBE,
        "task" => BOT,
        "todo_write" => LIST_CHECKS,
        "grep" | "glob" | "find" | "ls" => SEARCH,
        _ => WRENCH,
    }
}
