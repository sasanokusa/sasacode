//! Colours and styles (`theme.ts`). Colours drop out under NO_COLOR (no-color.org); attributes
//! such as bold and inverse stay, so the selection and the cursor remain visible.

use std::sync::atomic::{AtomicBool, Ordering};

static NO_COLOR: AtomicBool = AtomicBool::new(false);

/// Set from the environment once, when the UI starts.
pub fn set_no_color(on: bool) {
    NO_COLOR.store(on, Ordering::Relaxed);
}

pub fn no_color() -> bool {
    NO_COLOR.load(Ordering::Relaxed)
}

/// NO_COLOR as the environment sets it: present and not empty.
pub fn no_color_from_env() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

fn sgr(open: &str, close: &str, s: &str) -> String {
    format!("\x1b[{open}m{s}\x1b[{close}m")
}

fn color(open: &str, close: &str, s: &str) -> String {
    if no_color() { s.to_string() } else { sgr(open, close, s) }
}

pub fn bold(s: &str) -> String {
    sgr("1", "22", s)
}
pub fn dim(s: &str) -> String {
    sgr("2", "22", s)
}
pub fn italic(s: &str) -> String {
    sgr("3", "23", s)
}
pub fn underline(s: &str) -> String {
    sgr("4", "24", s)
}
pub fn strike(s: &str) -> String {
    sgr("9", "29", s)
}
pub fn inverse(s: &str) -> String {
    sgr("7", "27", s)
}
pub fn red(s: &str) -> String {
    color("31", "39", s)
}
pub fn green(s: &str) -> String {
    color("32", "39", s)
}
pub fn yellow(s: &str) -> String {
    color("33", "39", s)
}
pub fn blue(s: &str) -> String {
    color("34", "39", s)
}
pub fn magenta(s: &str) -> String {
    color("35", "39", s)
}
pub fn cyan(s: &str) -> String {
    color("36", "39", s)
}
pub fn gray(s: &str) -> String {
    color("90", "39", s)
}
pub fn bg_user(s: &str) -> String {
    color("48;5;236", "49", s)
}
pub fn bg_add(s: &str) -> String {
    color("48;5;22", "49", s)
}
pub fn bg_del(s: &str) -> String {
    color("48;5;52", "49", s)
}
pub fn identity(s: &str) -> String {
    s.to_string()
}

/// A colour function, as themes pass them around.
pub type Style = fn(&str) -> String;

/// How lists of choices look (pi-tui `SelectListTheme`).
#[derive(Clone, Copy)]
pub struct SelectTheme {
    pub selected_prefix: Style,
    pub selected_text: Style,
    pub description: Style,
    pub scroll_info: Style,
    pub no_match: fn() -> String,
}

pub fn selected_text(s: &str) -> String {
    bold(&cyan(s))
}

/// The list says "No matching commands" whatever it lists (models, sessions), and in English.
fn no_match() -> String {
    gray(&format!("  {}", sasacode_host::t("一致するものがありません", &[])))
}

pub const SELECT_THEME: SelectTheme = SelectTheme {
    selected_prefix: cyan,
    selected_text,
    description: gray,
    scroll_info: gray,
    no_match,
};

/// How the input box looks (pi-tui `EditorTheme`).
#[derive(Clone, Copy)]
pub struct EditorTheme {
    pub border: Style,
    pub select: SelectTheme,
}

pub const EDITOR_THEME: EditorTheme = EditorTheme {
    border: gray,
    select: SELECT_THEME,
};

/// How Markdown looks (pi-tui `MarkdownTheme`).
#[derive(Clone, Copy)]
pub struct MarkdownTheme {
    pub heading: Style,
    pub link: Style,
    pub link_url: Style,
    pub code: Style,
    pub code_block: Style,
    pub code_block_border: Style,
    pub quote: Style,
    pub quote_border: Style,
    pub hr: Style,
    pub list_bullet: Style,
    pub bold: Style,
    pub italic: Style,
    pub strikethrough: Style,
    pub underline: Style,
}

fn heading(s: &str) -> String {
    bold(&cyan(s))
}

pub const MARKDOWN_THEME: MarkdownTheme = MarkdownTheme {
    heading,
    link: blue,
    link_url: gray,
    code: yellow,
    code_block: identity,
    code_block_border: gray,
    quote: italic,
    quote_border: gray,
    hr: gray,
    list_bullet: cyan,
    bold,
    italic,
    strikethrough: strike,
    underline,
};
