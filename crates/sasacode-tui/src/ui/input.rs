//! A one-line text field with horizontal scrolling (pi-tui `Input`). Positions are byte offsets.

use crossterm::event::KeyEvent;

use crate::keys::{Keybindings, matches_key, printable};
use crate::text::width::is_js_whitespace;
use crate::text::{CURSOR_MARKER, graphemes, slice_by_column, truncate_to_width, visible_width};
use crate::ui::editing::{
    KillRing, LastAction, find_word_backward, find_word_forward, next_grapheme, prev_grapheme, word_segments,
};

/// What a key did to the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    None,
    Submit(String),
    Escape,
}

pub struct Input {
    value: String,
    cursor: usize,
    prompt: String,
    placeholder: String,
    placeholder_style: fn(&str) -> String,
    rendered_start_column: usize,
    pub focused: bool,
    kill_ring: KillRing,
    last_action: Option<LastAction>,
    undo: Vec<(String, usize)>,
}

fn no_atomic(_: &str) -> bool {
    false
}

impl Default for Input {
    fn default() -> Self {
        Self::new("> ", "", crate::theme::identity)
    }
}

impl Input {
    pub fn new(
        prompt: impl Into<String>,
        placeholder: impl Into<String>,
        placeholder_style: fn(&str) -> String,
    ) -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            prompt: prompt.into(),
            placeholder: placeholder.into(),
            placeholder_style,
            rendered_start_column: 0,
            focused: false,
            kill_ring: KillRing::default(),
            last_action: None,
            undo: Vec::new(),
        }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn set_value(&mut self, value: &str) {
        self.value = value.to_string();
        self.cursor = self.cursor.min(self.value.len());
        while !self.value.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_placeholder(&mut self, placeholder: impl Into<String>) {
        self.placeholder = placeholder.into();
    }

    pub fn handle_key(&mut self, ev: &KeyEvent, kb: &Keybindings) -> InputAction {
        if kb.matches(ev, "tui.select.cancel") {
            return InputAction::Escape;
        }
        if kb.matches(ev, "tui.editor.undo") {
            if let Some((value, cursor)) = self.undo.pop() {
                self.value = value;
                self.cursor = cursor;
                self.last_action = None;
            }
            return InputAction::None;
        }
        if kb.matches(ev, "tui.input.submit") || matches_key(ev, "ctrl+j") {
            return InputAction::Submit(self.value.clone());
        }
        if kb.matches(ev, "tui.editor.deleteCharBackward") {
            self.last_action = None;
            if self.cursor > 0 {
                self.push_undo();
                let start = prev_grapheme(&self.value, self.cursor);
                self.value.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            return InputAction::None;
        }
        if kb.matches(ev, "tui.editor.deleteCharForward") {
            self.last_action = None;
            if self.cursor < self.value.len() {
                self.push_undo();
                let end = next_grapheme(&self.value, self.cursor);
                self.value.replace_range(self.cursor..end, "");
            }
            return InputAction::None;
        }
        if kb.matches(ev, "tui.editor.deleteWordBackward") {
            if self.cursor > 0 {
                let was_kill = self.last_action == Some(LastAction::Kill);
                self.push_undo();
                let from = find_word_backward(&self.value, self.cursor, &word_segments, &no_atomic);
                let deleted = self.value[from..self.cursor].to_string();
                self.kill_ring.push(&deleted, true, was_kill);
                self.last_action = Some(LastAction::Kill);
                self.value.replace_range(from..self.cursor, "");
                self.cursor = from;
            }
            return InputAction::None;
        }
        if kb.matches(ev, "tui.editor.deleteWordForward") {
            if self.cursor < self.value.len() {
                let was_kill = self.last_action == Some(LastAction::Kill);
                self.push_undo();
                let to = find_word_forward(&self.value, self.cursor, &word_segments, &no_atomic);
                let deleted = self.value[self.cursor..to].to_string();
                self.kill_ring.push(&deleted, false, was_kill);
                self.last_action = Some(LastAction::Kill);
                self.value.replace_range(self.cursor..to, "");
            }
            return InputAction::None;
        }
        if kb.matches(ev, "tui.editor.deleteToLineStart") {
            if self.cursor > 0 {
                self.push_undo();
                let deleted = self.value[..self.cursor].to_string();
                self.kill_ring
                    .push(&deleted, true, self.last_action == Some(LastAction::Kill));
                self.last_action = Some(LastAction::Kill);
                self.value.replace_range(..self.cursor, "");
                self.cursor = 0;
            }
            return InputAction::None;
        }
        if kb.matches(ev, "tui.editor.deleteToLineEnd") {
            if self.cursor < self.value.len() {
                self.push_undo();
                let deleted = self.value[self.cursor..].to_string();
                self.kill_ring
                    .push(&deleted, false, self.last_action == Some(LastAction::Kill));
                self.last_action = Some(LastAction::Kill);
                self.value.truncate(self.cursor);
            }
            return InputAction::None;
        }
        if kb.matches(ev, "tui.editor.yank") {
            if let Some(text) = self.kill_ring.peek().filter(|t| !t.is_empty()).map(str::to_string) {
                self.push_undo();
                self.value.insert_str(self.cursor, &text);
                self.cursor += text.len();
                self.last_action = Some(LastAction::Yank);
            }
            return InputAction::None;
        }
        if kb.matches(ev, "tui.editor.yankPop") {
            if self.last_action == Some(LastAction::Yank) && self.kill_ring.len() > 1 {
                self.push_undo();
                let prev = self.kill_ring.peek().unwrap_or("").to_string();
                let start = self.cursor.saturating_sub(prev.len());
                self.value.replace_range(start..self.cursor, "");
                self.cursor = start;
                self.kill_ring.rotate();
                let text = self.kill_ring.peek().unwrap_or("").to_string();
                self.value.insert_str(self.cursor, &text);
                self.cursor += text.len();
                self.last_action = Some(LastAction::Yank);
            }
            return InputAction::None;
        }
        if kb.matches(ev, "tui.editor.cursorLeft") {
            self.last_action = None;
            self.cursor = prev_grapheme(&self.value, self.cursor);
            return InputAction::None;
        }
        if kb.matches(ev, "tui.editor.cursorRight") {
            self.last_action = None;
            self.cursor = next_grapheme(&self.value, self.cursor);
            return InputAction::None;
        }
        if kb.matches(ev, "tui.editor.cursorLineStart") {
            self.last_action = None;
            self.cursor = 0;
            return InputAction::None;
        }
        if kb.matches(ev, "tui.editor.cursorLineEnd") {
            self.last_action = None;
            self.cursor = self.value.len();
            return InputAction::None;
        }
        if kb.matches(ev, "tui.editor.cursorWordLeft") {
            if self.cursor > 0 {
                self.last_action = None;
                self.cursor = find_word_backward(&self.value, self.cursor, &word_segments, &no_atomic);
            }
            return InputAction::None;
        }
        if kb.matches(ev, "tui.editor.cursorWordRight") {
            if self.cursor < self.value.len() {
                self.last_action = None;
                self.cursor = find_word_forward(&self.value, self.cursor, &word_segments, &no_atomic);
            }
            return InputAction::None;
        }
        if let Some(c) = printable(ev) {
            self.insert_character(c);
        }
        InputAction::None
    }

    fn insert_character(&mut self, c: char) {
        if is_js_whitespace(c) || self.last_action != Some(LastAction::TypeWord) {
            self.push_undo();
        }
        self.last_action = Some(LastAction::TypeWord);
        self.value.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn push_undo(&mut self) {
        self.undo.push((self.value.clone(), self.cursor));
    }

    /// A paste goes in on one line: line breaks dropped, tabs widened.
    pub fn handle_paste(&mut self, text: &str) {
        self.last_action = None;
        self.push_undo();
        let clean = text.replace(['\r', '\n'], "").replace('\t', "    ");
        self.value.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    /// A left click at column `x` of the rendered line puts the cursor there.
    pub fn click(&mut self, x: usize) {
        let target = self.rendered_start_column + x.saturating_sub(visible_width(&self.prompt));
        let mut col = 0;
        self.cursor = self.value.len();
        let mut index = 0;
        for g in graphemes(&self.value) {
            let next = col + visible_width(g);
            if target < next {
                self.cursor = index;
                break;
            }
            col = next;
            index += g.len();
        }
        self.last_action = None;
    }

    pub fn render(&mut self, width: usize) -> Vec<String> {
        let prompt_width = visible_width(&self.prompt);
        if width <= prompt_width {
            return vec![truncate_to_width(&self.prompt, width, "", false)];
        }
        let available = width - prompt_width;
        let marker = if self.focused { CURSOR_MARKER } else { "" };
        if self.value.is_empty() && !self.placeholder.is_empty() {
            let placeholder = truncate_to_width(&self.placeholder, available, "", false);
            let at = graphemes(&placeholder).next().unwrap_or(" ").to_string();
            let after = placeholder.get(at.len()..).unwrap_or("");
            let style = self.placeholder_style;
            let text = format!("{marker}\x1b[7m{}\x1b[27m{}", style(&at), style(after));
            let padding = " ".repeat(available.saturating_sub(visible_width(&text)));
            return vec![format!("{}{text}{padding}", self.prompt)];
        }
        let mut visible = self.value.clone();
        let mut cursor_display = self.cursor;
        self.rendered_start_column = 0;
        let total = visible_width(&self.value);
        if total >= available {
            let scroll_width = if self.cursor == self.value.len() {
                available - 1
            } else {
                available
            };
            let cursor_col = visible_width(&self.value[..self.cursor]);
            if scroll_width > 0 {
                let half = scroll_width / 2;
                let start = if cursor_col < half {
                    0
                } else if cursor_col > total.saturating_sub(half) {
                    total.saturating_sub(scroll_width)
                } else {
                    cursor_col.saturating_sub(half)
                };
                self.rendered_start_column = start;
                visible = slice_by_column(&self.value, start, scroll_width, true);
                cursor_display = slice_by_column(&self.value, start, cursor_col.saturating_sub(start), true).len();
            } else {
                visible.clear();
                cursor_display = 0;
            }
        }
        let cursor_display = cursor_display.min(visible.len());
        let before = &visible[..cursor_display];
        let rest = &visible[cursor_display..];
        let at = graphemes(rest).next().unwrap_or(" ");
        let after = rest.get(at.len()..).unwrap_or("");
        let text = format!("{before}{marker}\x1b[7m{at}\x1b[27m{after}");
        let padding = " ".repeat(available.saturating_sub(visible_width(&text)));
        vec![format!("{}{text}{padding}", self.prompt)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::key_event;

    #[test]
    fn typing_editing_and_submit() {
        let kb = Keybindings::new();
        let mut i = Input::default();
        for c in "hello world".chars() {
            i.handle_key(&key_event(&c.to_string()), &kb);
        }
        i.handle_key(&key_event("ctrl+w"), &kb);
        assert_eq!(i.value(), "hello ");
        i.handle_paste("a\nb\tc");
        assert_eq!(i.value(), "hello ab    c");
        assert_eq!(
            i.handle_key(&key_event("enter"), &kb),
            InputAction::Submit("hello ab    c".into())
        );
        assert_eq!(i.handle_key(&key_event("escape"), &kb), InputAction::Escape);
    }

    #[test]
    fn a_long_value_scrolls_to_keep_the_cursor_in_view() {
        let mut i = Input::default();
        i.set_value(&"x".repeat(50));
        i.handle_key(&key_event("end"), &Keybindings::new());
        let line = &i.render(20)[0];
        assert_eq!(visible_width(line), 20);
        assert!(line.contains("\x1b[7m \x1b[27m"));
    }

    #[test]
    fn the_placeholder_shows_when_empty() {
        let mut i = Input::new(" ", "find", crate::theme::dim);
        let line = &i.render(10)[0];
        assert!(line.contains("ind"));
        assert_eq!(visible_width(line), 10);
    }
}
