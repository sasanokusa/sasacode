//! Components: things that render themselves as lines for a width (pi-tui's model). A line is an
//! ANSI string no wider than the width; the renderer turns lines into screen updates.

pub mod autocomplete;
pub mod editing;
pub mod editor;
pub mod input;
pub mod markdown;
pub mod select_list;

use crate::text::{apply_background_to_line, js_trim, visible_width, wrap_text_with_ansi};

/// Something that renders as lines.
pub trait Component {
    fn render(&mut self, width: usize) -> Vec<String>;
    /// Drop cached lines (the theme or a global display setting changed).
    fn invalidate(&mut self) {}
}

/// Wrapped text with horizontal and vertical padding, each line filled to the width (pi-tui
/// `Text`). Empty or blank text renders as no lines.
pub struct Text {
    text: String,
    padding_x: usize,
    padding_y: usize,
    bg: Option<fn(&str) -> String>,
    cache: Option<(String, usize, Vec<String>)>,
}

impl Text {
    pub fn new(text: impl Into<String>, padding_x: usize, padding_y: usize) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
            bg: None,
            cache: None,
        }
    }

    pub fn with_bg(mut self, bg: fn(&str) -> String) -> Self {
        self.bg = Some(bg);
        self
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cache = None;
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Component for Text {
    fn render(&mut self, width: usize) -> Vec<String> {
        if let Some((t, w, lines)) = &self.cache
            && *t == self.text
            && *w == width
        {
            return lines.clone();
        }
        if js_trim(&self.text).is_empty() {
            self.cache = Some((self.text.clone(), width, Vec::new()));
            return Vec::new();
        }
        let normalized = self.text.replace('\t', "   ");
        let padding_x = self.padding_x.min(width.saturating_sub(1) / 2);
        let content_width = width.saturating_sub(padding_x * 2).max(1);
        let margin = " ".repeat(padding_x);
        let mut content = Vec::new();
        for line in wrap_text_with_ansi(&normalized, content_width) {
            let with_margins = format!("{margin}{line}{margin}");
            match self.bg {
                Some(bg) => content.push(apply_background_to_line(&with_margins, width, bg)),
                None => {
                    let pad = width.saturating_sub(visible_width(&with_margins));
                    content.push(format!("{with_margins}{}", " ".repeat(pad)));
                }
            }
        }
        let empty = " ".repeat(width);
        let blank = match self.bg {
            Some(bg) => apply_background_to_line(&empty, width, bg),
            None => empty,
        };
        let mut out = vec![blank.clone(); self.padding_y];
        out.extend(content);
        out.extend(std::iter::repeat_n(blank, self.padding_y));
        if out.is_empty() {
            out.push(String::new());
        }
        self.cache = Some((self.text.clone(), width, out.clone()));
        out
    }

    fn invalidate(&mut self) {
        self.cache = None;
    }
}

/// Blank lines (pi-tui `Spacer`).
pub struct Spacer(pub usize);

impl Component for Spacer {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec![String::new(); self.0]
    }
}
