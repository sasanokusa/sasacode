//! The multi-line input box (pi-tui `Editor`, with the sasacode patches): word wrap, history,
//! kill ring, undo, folded pastes shown as `[paste #n …]` markers, and completion of commands,
//! arguments and paths. Positions are byte offsets on grapheme boundaries.

use std::collections::{BTreeMap, BTreeSet};

use crossterm::event::KeyEvent;

use crate::keys::{Keybindings, matches_key, printable};
use crate::text::width::{is_cjk_break, is_js_whitespace};
use crate::text::{CURSOR_MARKER, js_trim, slice_by_column, utf16_len, visible_width};
use crate::theme::EditorTheme;
use crate::ui::autocomplete::{Applied, AutocompleteProvider, Suggest};
use crate::ui::editing::{
    KillRing, LastAction, Seg, SegMode, find_word_backward, find_word_forward, has_whitespace, is_paste_marker,
    paste_marker, paste_marker_id, replace_markers, segment_with_markers, word_wrap_line,
};
use crate::ui::select_list::{SelectItem, SelectLayout, SelectList};

const SLASH_LAYOUT: SelectLayout = SelectLayout {
    min_primary: Some(12),
    max_primary: Some(32),
};
const ARGUMENT_LAYOUT: SelectLayout = SelectLayout {
    min_primary: Some(12),
    max_primary: Some(64),
};
const TRIGGER_CHARACTERS: [char; 2] = ['@', '#'];

#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
}

impl Default for State {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct Snapshot {
    state: State,
    pastes: BTreeMap<u32, String>,
    paste_counter: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcState {
    Regular,
    Force,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Jump {
    Forward,
    Backward,
}

/// An argument completion to ask a command for; the answer goes to [`Editor::resolve_argument`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgumentRequest {
    pub command: String,
    pub argument: String,
    generation: u64,
    snapshot: (String, usize, usize),
    force: bool,
    explicit_tab: bool,
}

struct LayoutLine {
    text: String,
    cursor: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
struct VisualLine {
    logical: usize,
    start: usize,
    len: usize,
}

/// The `─── ↑ n more` rule shown when lines are scrolled out of the box.
fn scroll_border(direction: &str, hidden: usize, width: usize) -> String {
    let label = format!(" {direction} {hidden} more ");
    let lw = visible_width(&label);
    if lw + 2 <= width {
        let left = (width - lw) / 2;
        return format!("{}{label}{}", "─".repeat(left), "─".repeat(width - left - lw));
    }
    let indicator = format!("─── {direction} {hidden} more ");
    let iw = visible_width(&indicator);
    if iw <= width {
        return format!("{indicator}{}", "─".repeat(width - iw));
    }
    let ellipsis = &"..."[..width.min(3)];
    let keep = width - ellipsis.len();
    format!("{}{ellipsis}", slice_by_column(&indicator, 0, keep, true))
}

/// `[\r\n]` normalized and tabs widened (pi-tui `normalizeText`).
fn normalize_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ")
}

pub struct Editor {
    state: State,
    pub focused: bool,
    theme: EditorTheme,
    padding_x: usize,
    last_width: usize,
    rendered_visible_lines: usize,
    rendered_autocomplete_height: usize,
    scroll_offset: usize,
    /// The terminal's height, which sizes the box (30% of it, at least 5 lines).
    pub terminal_rows: usize,
    provider: Option<AutocompleteProvider>,
    ac_list: Option<SelectList>,
    ac_state: Option<AcState>,
    ac_prefix: String,
    ac_max_visible: usize,
    ac_generation: u64,
    pending_argument: Option<ArgumentRequest>,
    pastes: BTreeMap<u32, String>,
    paste_counter: u32,
    history: Vec<String>,
    history_index: i64,
    history_draft: Option<State>,
    kill_ring: KillRing,
    last_action: Option<LastAction>,
    jump: Option<Jump>,
    preferred_visual_col: Option<usize>,
    snapped_from_cursor_col: Option<usize>,
    undo: Vec<Snapshot>,
    pub disable_submit: bool,
    submitted: Option<String>,
}

impl Editor {
    pub fn new(theme: EditorTheme, padding_x: usize) -> Self {
        Self {
            state: State::default(),
            focused: false,
            theme,
            padding_x,
            last_width: 80,
            rendered_visible_lines: 1,
            rendered_autocomplete_height: 0,
            scroll_offset: 0,
            terminal_rows: 24,
            provider: None,
            ac_list: None,
            ac_state: None,
            ac_prefix: String::new(),
            ac_max_visible: 5,
            ac_generation: 0,
            pending_argument: None,
            pastes: BTreeMap::new(),
            paste_counter: 0,
            history: Vec::new(),
            history_index: -1,
            history_draft: None,
            kill_ring: KillRing::default(),
            last_action: None,
            jump: None,
            preferred_visual_col: None,
            snapped_from_cursor_col: None,
            undo: Vec::new(),
            disable_submit: false,
            submitted: None,
        }
    }

    fn valid_ids(&self) -> BTreeSet<u32> {
        self.pastes.keys().copied().collect()
    }

    fn segment<'a>(&self, text: &'a str, mode: SegMode) -> Vec<Seg<'a>> {
        segment_with_markers(text, mode, &self.valid_ids())
    }

    pub fn set_autocomplete_provider(&mut self, provider: AutocompleteProvider) {
        self.cancel_autocomplete();
        self.provider = Some(provider);
    }

    /// Remember a prompt for up/down (newest first, no repeats in a row, at most 100).
    pub fn add_to_history(&mut self, text: &str) {
        let trimmed = js_trim(text);
        if trimmed.is_empty() || self.history.first().is_some_and(|h| h == trimmed) {
            return;
        }
        self.history.insert(0, trimmed.to_string());
        self.history.truncate(100);
    }

    pub fn is_empty(&self) -> bool {
        self.state.lines.len() == 1 && self.state.lines[0].is_empty()
    }

    pub fn get_text(&self) -> String {
        self.state.lines.join("\n")
    }

    pub fn get_lines(&self) -> Vec<String> {
        self.state.lines.clone()
    }

    /// (line, column in bytes).
    pub fn get_cursor(&self) -> (usize, usize) {
        (self.state.cursor_line, self.state.cursor_col)
    }

    /// The text with live paste markers expanded to what was pasted.
    pub fn get_expanded_text(&self) -> String {
        self.expand_paste_markers(&self.get_text())
    }

    pub fn is_showing_autocomplete(&self) -> bool {
        self.ac_state.is_some()
    }

    /// What enter submitted, once.
    pub fn take_submitted(&mut self) -> Option<String> {
        self.submitted.take()
    }

    /// An argument completion waiting to be asked for, once.
    pub fn take_argument_request(&mut self) -> Option<ArgumentRequest> {
        self.pending_argument.take()
    }

    fn expand_paste_markers(&self, text: &str) -> String {
        if self.pastes.is_empty() {
            return text.to_string();
        }
        // Only a marker exactly as it was made expands: one the user typed or edited stays text.
        replace_markers(text, |whole, id, _| match self.pastes.get(&id) {
            Some(content) if whole == paste_marker(id, content) => content.clone(),
            _ => whole.to_string(),
        })
    }

    /// The cursor to the end of the text (sasacode patch).
    pub fn move_to_end(&mut self) {
        self.state.cursor_line = self.state.lines.len() - 1;
        let len = self.state.lines[self.state.cursor_line].len();
        self.set_cursor_col(len);
    }

    pub fn set_text(&mut self, text: &str) {
        self.cancel_autocomplete();
        self.last_action = None;
        self.exit_history();
        let normalized = normalize_text(text);
        if self.get_text() != normalized {
            self.push_undo();
        }
        self.pastes.clear();
        self.paste_counter = 0;
        self.set_text_internal(&normalized, false);
    }

    /// Insert text at the cursor as one undo step (image markers, a short paste).
    pub fn insert_text_at_cursor(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.cancel_autocomplete();
        self.push_undo();
        self.last_action = None;
        self.exit_history();
        self.insert_text_internal(text);
    }

    fn set_text_internal(&mut self, text: &str, at_start: bool) {
        self.state.lines = text.split('\n').map(str::to_string).collect();
        self.state.cursor_line = if at_start { 0 } else { self.state.lines.len() - 1 };
        let col = if at_start {
            0
        } else {
            self.state.lines[self.state.cursor_line].len()
        };
        self.set_cursor_col(col);
        self.scroll_offset = 0;
    }

    fn insert_text_internal(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let normalized = normalize_text(text);
        let inserted: Vec<&str> = normalized.split('\n').collect();
        let line = self.current_line().to_string();
        let col = self.state.cursor_col.min(line.len());
        let (before, after) = line.split_at(col);
        if inserted.len() == 1 {
            self.state.lines[self.state.cursor_line] = format!("{before}{normalized}{after}");
            self.set_cursor_col(col + normalized.len());
        } else {
            let cl = self.state.cursor_line;
            let mut new_lines: Vec<String> = self.state.lines[..cl].to_vec();
            new_lines.push(format!("{before}{}", inserted[0]));
            for l in &inserted[1..inserted.len() - 1] {
                new_lines.push((*l).to_string());
            }
            let last = inserted[inserted.len() - 1];
            new_lines.push(format!("{last}{after}"));
            new_lines.extend_from_slice(&self.state.lines[cl + 1..]);
            self.state.lines = new_lines;
            self.state.cursor_line += inserted.len() - 1;
            self.set_cursor_col(last.len());
        }
    }

    fn current_line(&self) -> &str {
        self.state
            .lines
            .get(self.state.cursor_line)
            .map(String::as_str)
            .unwrap_or("")
    }

    fn set_cursor_col(&mut self, col: usize) {
        self.state.cursor_col = col;
        self.preferred_visual_col = None;
        self.snapped_from_cursor_col = None;
    }

    fn push_undo(&mut self) {
        self.undo.push(Snapshot {
            state: self.state.clone(),
            pastes: self.pastes.clone(),
            paste_counter: self.paste_counter,
        });
    }

    fn undo(&mut self) {
        self.exit_history();
        let Some(s) = self.undo.pop() else { return };
        self.state = s.state;
        self.pastes = s.pastes;
        self.paste_counter = s.paste_counter;
        self.last_action = None;
        self.preferred_visual_col = None;
    }

    fn exit_history(&mut self) {
        self.history_index = -1;
        self.history_draft = None;
    }

    fn navigate_history(&mut self, direction: i64) {
        self.last_action = None;
        if self.history.is_empty() {
            return;
        }
        let next = self.history_index - direction;
        if next < -1 || next >= self.history.len() as i64 {
            return;
        }
        if self.history_index == -1 && next >= 0 {
            self.push_undo();
            self.history_draft = Some(self.state.clone());
        }
        self.history_index = next;
        if next == -1 {
            match self.history_draft.take() {
                Some(draft) => {
                    self.state = draft;
                    self.preferred_visual_col = None;
                    self.snapped_from_cursor_col = None;
                    self.scroll_offset = 0;
                }
                None => self.set_text_internal("", false),
            }
        } else {
            let text = self.history[next as usize].clone();
            self.set_text_internal(&text, direction == -1);
        }
    }

    // ── rendering ──────────────────────────────────────────────────────────

    fn layout_text(&self, width: usize) -> Vec<LayoutLine> {
        if self.is_empty() {
            return vec![LayoutLine {
                text: String::new(),
                cursor: Some(0),
            }];
        }
        let mut out = Vec::new();
        for (i, line) in self.state.lines.iter().enumerate() {
            let current = i == self.state.cursor_line;
            if visible_width(line) <= width {
                out.push(LayoutLine {
                    text: line.clone(),
                    cursor: current.then_some(self.state.cursor_col),
                });
                continue;
            }
            let chunks = word_wrap_line(line, width, Some(self.segment(line, SegMode::Grapheme)));
            let n = chunks.len();
            for (k, chunk) in chunks.into_iter().enumerate() {
                let pos = self.state.cursor_col;
                let mut cursor = None;
                if current {
                    if k == n - 1 {
                        if pos >= chunk.start {
                            cursor = Some(pos - chunk.start);
                        }
                    } else if pos >= chunk.start && pos < chunk.end {
                        cursor = Some((pos - chunk.start).min(chunk.text.len()));
                    }
                }
                out.push(LayoutLine {
                    text: chunk.text,
                    cursor,
                });
            }
        }
        out
    }

    fn max_visible_lines(&self) -> usize {
        5.max(self.terminal_rows * 3 / 10)
    }

    pub fn render(&mut self, width: usize) -> Vec<String> {
        let padding_x = self.padding_x.min(width.saturating_sub(1) / 2);
        let content_width = width.saturating_sub(padding_x * 2).max(1);
        let layout_width = content_width.saturating_sub(if padding_x > 0 { 0 } else { 1 }).max(1);
        self.last_width = layout_width;
        let layout = self.layout_text(layout_width);
        let max_visible = self.max_visible_lines();
        let cursor_index = layout.iter().position(|l| l.cursor.is_some()).unwrap_or(0);
        if cursor_index < self.scroll_offset {
            self.scroll_offset = cursor_index;
        } else if cursor_index >= self.scroll_offset + max_visible {
            self.scroll_offset = cursor_index + 1 - max_visible;
        }
        self.scroll_offset = self.scroll_offset.min(layout.len().saturating_sub(max_visible));
        let visible = &layout[self.scroll_offset..(self.scroll_offset + max_visible).min(layout.len())];
        self.rendered_visible_lines = visible.len();
        let pad = " ".repeat(padding_x);
        let mut out = Vec::new();
        let top = if self.scroll_offset > 0 {
            scroll_border("↑", self.scroll_offset, width)
        } else {
            "─".repeat(width)
        };
        out.push((self.theme.border)(&top));
        for l in visible {
            let mut display = l.text.clone();
            let mut line_width = visible_width(&l.text);
            let mut cursor_in_padding = false;
            if let Some(pos) = l.cursor {
                let pos = pos.min(display.len());
                let (before, after) = display.split_at(pos);
                let marker = if self.focused { CURSOR_MARKER } else { "" };
                if !after.is_empty() {
                    let first = self.segment(after, SegMode::Grapheme).first().map_or("", |s| s.text);
                    let rest = &after[first.len()..];
                    display = format!("{before}{marker}\x1b[7m{first}\x1b[0m{rest}");
                } else {
                    display = format!("{before}{marker}\x1b[7m \x1b[0m");
                    line_width += 1;
                    if line_width > content_width && padding_x > 0 {
                        cursor_in_padding = true;
                    }
                }
            }
            let fill = " ".repeat(content_width.saturating_sub(line_width));
            let right = if cursor_in_padding { &pad[1..] } else { pad.as_str() };
            out.push(format!("{pad}{display}{fill}{right}"));
        }
        let below = layout.len() - (self.scroll_offset + visible.len());
        let bottom = if below > 0 {
            scroll_border("↓", below, width)
        } else {
            "─".repeat(width)
        };
        out.push((self.theme.border)(&bottom));
        self.rendered_autocomplete_height = 0;
        if self.ac_state.is_some()
            && let Some(list) = &self.ac_list
        {
            let lines = list.render(content_width);
            self.rendered_autocomplete_height = lines.len();
            for line in lines {
                let fill = " ".repeat(content_width.saturating_sub(visible_width(&line)));
                out.push(format!("{pad}{line}{fill}{pad}"));
            }
        }
        out
    }

    // ── input ──────────────────────────────────────────────────────────────

    pub fn handle_key(&mut self, ev: &KeyEvent, kb: &Keybindings) {
        if let Some(dir) = self.jump {
            if kb.matches(ev, "tui.editor.jumpForward") || kb.matches(ev, "tui.editor.jumpBackward") {
                self.jump = None;
                return;
            }
            self.jump = None;
            if let Some(c) = printable(ev) {
                self.jump_to_char(c, dir);
                return;
            }
        }
        if kb.matches(ev, "tui.input.copy") {
            return;
        }
        if kb.matches(ev, "tui.editor.undo") {
            self.undo();
            return;
        }
        if self.ac_state.is_some() && self.ac_list.is_some() {
            if kb.matches(ev, "tui.select.cancel") {
                self.cancel_autocomplete();
                return;
            }
            if kb.matches(ev, "tui.select.up") || kb.matches(ev, "tui.select.down") {
                if let Some(list) = self.ac_list.as_mut() {
                    list.handle_key(ev, kb);
                }
                return;
            }
            if kb.matches(ev, "tui.input.tab") {
                if let Some(item) = self.ac_list.as_ref().and_then(|l| l.selected_item().cloned()) {
                    self.take_completion(&item);
                    self.cancel_autocomplete();
                }
                return;
            }
            if kb.matches(ev, "tui.select.confirm")
                && let Some(item) = self.ac_list.as_ref().and_then(|l| l.selected_item().cloned())
            {
                let slash = self.ac_prefix.starts_with('/');
                self.take_completion(&item);
                self.cancel_autocomplete();
                // A command name completes and runs; an argument only completes.
                if !slash {
                    return;
                }
            }
        }
        if kb.matches(ev, "tui.input.tab") && self.ac_state.is_none() {
            self.handle_tab_completion();
            return;
        }
        if kb.matches(ev, "tui.editor.deleteToLineEnd") {
            self.delete_to_end_of_line();
            return;
        }
        if kb.matches(ev, "tui.editor.deleteToLineStart") {
            self.delete_to_start_of_line();
            return;
        }
        if kb.matches(ev, "tui.editor.deleteWordBackward") {
            self.delete_word_backwards();
            return;
        }
        if kb.matches(ev, "tui.editor.deleteWordForward") {
            self.delete_word_forward();
            return;
        }
        if kb.matches(ev, "tui.editor.deleteCharBackward") || matches_key(ev, "shift+backspace") {
            self.handle_backspace();
            return;
        }
        if kb.matches(ev, "tui.editor.deleteCharForward") || matches_key(ev, "shift+delete") {
            self.handle_forward_delete();
            return;
        }
        if kb.matches(ev, "tui.editor.yank") {
            self.yank();
            return;
        }
        if kb.matches(ev, "tui.editor.yankPop") {
            self.yank_pop();
            return;
        }
        if kb.matches(ev, "tui.editor.historyPrevious") {
            self.cancel_autocomplete();
            self.navigate_history(-1);
            return;
        }
        if kb.matches(ev, "tui.editor.historyNext") {
            self.cancel_autocomplete();
            self.navigate_history(1);
            return;
        }
        if kb.matches(ev, "tui.editor.cursorLineStart") {
            self.last_action = None;
            self.set_cursor_col(0);
            return;
        }
        if kb.matches(ev, "tui.editor.cursorLineEnd") {
            self.move_to_line_end();
            return;
        }
        if kb.matches(ev, "tui.editor.cursorWordLeft") {
            self.move_word_backwards();
            return;
        }
        if kb.matches(ev, "tui.editor.cursorWordRight") {
            self.move_word_forwards();
            return;
        }
        // shift+enter, ctrl+j (a bare \n), alt+enter (ESC \r).
        if kb.matches(ev, "tui.input.newLine") || matches_key(ev, "alt+enter") {
            self.add_new_line();
            return;
        }
        if kb.matches(ev, "tui.input.submit") {
            if self.disable_submit {
                return;
            }
            let col = self.state.cursor_col;
            if col > 0 && self.current_line().as_bytes().get(col - 1) == Some(&b'\\') {
                self.handle_backspace();
                self.add_new_line();
                return;
            }
            self.submit_value();
            return;
        }
        if kb.matches(ev, "tui.editor.cursorUp") {
            let first = self.is_on_first_visual_line();
            if first && (self.is_empty() || self.history_index > -1 || self.state.cursor_col == 0) {
                self.navigate_history(-1);
            } else if first {
                self.last_action = None;
                self.set_cursor_col(0);
            } else {
                self.move_cursor(-1, 0);
            }
            return;
        }
        if kb.matches(ev, "tui.editor.cursorDown") {
            let last = self.is_on_last_visual_line();
            if self.history_index > -1 && last {
                self.navigate_history(1);
            } else if last {
                self.move_to_line_end();
            } else {
                self.move_cursor(1, 0);
            }
            return;
        }
        if kb.matches(ev, "tui.editor.cursorRight") {
            self.move_cursor(0, 1);
            return;
        }
        if kb.matches(ev, "tui.editor.cursorLeft") {
            self.move_cursor(0, -1);
            return;
        }
        if kb.matches(ev, "tui.editor.pageUp") {
            self.page_scroll(-1);
            return;
        }
        if kb.matches(ev, "tui.editor.pageDown") {
            self.page_scroll(1);
            return;
        }
        if kb.matches(ev, "tui.editor.jumpForward") {
            self.jump = Some(Jump::Forward);
            return;
        }
        if kb.matches(ev, "tui.editor.jumpBackward") {
            self.jump = Some(Jump::Backward);
            return;
        }
        if matches_key(ev, "shift+space") {
            self.insert_character(" ");
            return;
        }
        if let Some(c) = printable(ev) {
            let mut buf = [0u8; 4];
            self.insert_character(c.encode_utf8(&mut buf));
        }
    }

    /// A bracketed paste: folded into a marker past 10 lines or 1000 characters.
    pub fn handle_paste(&mut self, pasted: &str) {
        if pasted.is_empty() {
            return;
        }
        self.cancel_autocomplete();
        self.exit_history();
        self.last_action = None;
        self.push_undo();
        // Some terminals send ctrl+letter inside a paste as CSI u.
        let decoded = decode_csi_u_ctrl(pasted);
        let clean = normalize_text(&decoded);
        let mut filtered: String = clean.chars().filter(|&c| c == '\n' || c as u32 >= 32).collect();
        if filtered.starts_with(['/', '~', '.']) {
            let col = self.state.cursor_col;
            let before = self.current_line()[..col].chars().last();
            if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                filtered = format!(" {filtered}");
            }
        }
        let lines = filtered.split('\n').count();
        if lines > 10 || utf16_len(&filtered) > 1000 {
            self.paste_counter += 1;
            let id = self.paste_counter;
            let marker = paste_marker(id, &filtered);
            self.pastes.insert(id, filtered);
            self.insert_text_internal(&marker);
            return;
        }
        self.insert_text_internal(&filtered);
    }

    fn insert_character(&mut self, ch: &str) {
        self.exit_history();
        if ch.chars().any(is_js_whitespace) || self.last_action != Some(LastAction::TypeWord) {
            self.push_undo();
        }
        self.last_action = Some(LastAction::TypeWord);
        let col = self.state.cursor_col;
        self.state.lines[self.state.cursor_line].insert_str(col, ch);
        self.set_cursor_col(col + ch.len());
        if self.ac_state.is_some() {
            self.update_autocomplete();
            return;
        }
        let before = self.current_line()[..self.state.cursor_col].to_string();
        if ch == "/" && self.is_at_start_of_message() {
            self.request_autocomplete(false, false);
        } else if ch.chars().count() == 1 && ch.chars().all(|c| TRIGGER_CHARACTERS.contains(&c)) {
            if trigger_pattern(&before) {
                self.request_autocomplete(false, false);
            }
        } else if ch
            .chars()
            .any(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') || is_cjk_break(c))
            && (self.is_in_slash_context(&before) || trigger_pattern(&before))
        {
            self.request_autocomplete(false, false);
        }
    }

    fn add_new_line(&mut self) {
        self.cancel_autocomplete();
        self.exit_history();
        self.last_action = None;
        self.push_undo();
        let col = self.state.cursor_col;
        let line = std::mem::take(&mut self.state.lines[self.state.cursor_line]);
        let (before, after) = line.split_at(col.min(line.len()));
        self.state.lines[self.state.cursor_line] = before.to_string();
        self.state.lines.insert(self.state.cursor_line + 1, after.to_string());
        self.state.cursor_line += 1;
        self.set_cursor_col(0);
    }

    fn submit_value(&mut self) {
        self.cancel_autocomplete();
        let result = js_trim(&self.expand_paste_markers(&self.get_text())).to_string();
        self.state = State::default();
        self.pastes.clear();
        self.paste_counter = 0;
        self.exit_history();
        self.scroll_offset = 0;
        self.undo.clear();
        self.last_action = None;
        self.submitted = Some(result);
    }

    fn handle_backspace(&mut self) {
        self.exit_history();
        self.last_action = None;
        if self.state.cursor_col > 0 {
            self.push_undo();
            let col = self.state.cursor_col;
            let line = self.current_line().to_string();
            let segs = self.segment(&line[..col], SegMode::Grapheme);
            let last = segs.last().map_or("", |s| s.text).to_string();
            let glen = if last.is_empty() { 1 } else { last.len() };
            if let Some(target) = paste_marker_id(&last) {
                self.remove_paste(target);
            }
            let line = self.current_line().to_string();
            let start = col.saturating_sub(glen).min(line.len());
            let end = col.min(line.len());
            self.state.lines[self.state.cursor_line] = format!("{}{}", &line[..start], &line[end..]);
            self.set_cursor_col(start);
        } else if self.state.cursor_line > 0 {
            self.push_undo();
            let cl = self.state.cursor_line;
            let current = self.state.lines.remove(cl);
            let prev_len = self.state.lines[cl - 1].len();
            self.state.lines[cl - 1].push_str(&current);
            self.state.cursor_line -= 1;
            self.set_cursor_col(prev_len);
        }
        self.after_deletion();
    }

    /// A deleted marker drops its paste; markers of later pastes are renumbered down.
    fn remove_paste(&mut self, target: u32) {
        self.pastes.remove(&target);
        self.paste_counter = self.paste_counter.saturating_sub(1);
        let higher: Vec<u32> = self.pastes.keys().copied().filter(|&id| id > target).collect();
        for id in higher {
            if let Some(content) = self.pastes.remove(&id) {
                self.pastes.insert(id - 1, content);
            }
        }
        for line in &mut self.state.lines {
            *line = replace_markers(line, |whole, x, suffix| {
                if x <= target {
                    whole.to_string()
                } else {
                    format!("[paste #{}{suffix}]", x - 1)
                }
            });
        }
    }

    fn after_deletion(&mut self) {
        if self.ac_state.is_some() {
            self.update_autocomplete();
        } else {
            let before = self.current_line()[..self.state.cursor_col.min(self.current_line().len())].to_string();
            if self.is_in_slash_context(&before) || trigger_pattern(&before) {
                self.request_autocomplete(false, false);
            }
        }
    }

    fn handle_forward_delete(&mut self) {
        self.exit_history();
        self.last_action = None;
        let col = self.state.cursor_col;
        let line = self.current_line().to_string();
        if col < line.len() {
            self.push_undo();
            let segs = self.segment(&line[col..], SegMode::Grapheme);
            let glen = segs.first().map_or(1, |s| s.text.len());
            self.state.lines[self.state.cursor_line] = format!("{}{}", &line[..col], &line[col + glen..]);
        } else if self.state.cursor_line + 1 < self.state.lines.len() {
            self.push_undo();
            let next = self.state.lines.remove(self.state.cursor_line + 1);
            self.state.lines[self.state.cursor_line].push_str(&next);
        }
        self.after_deletion();
    }

    fn move_to_line_end(&mut self) {
        self.last_action = None;
        let len = self.current_line().len();
        self.set_cursor_col(len);
    }

    fn delete_to_start_of_line(&mut self) {
        self.exit_history();
        let col = self.state.cursor_col;
        let accumulate = self.last_action == Some(LastAction::Kill);
        if col > 0 {
            self.push_undo();
            let line = self.current_line().to_string();
            self.kill_ring.push(&line[..col], true, accumulate);
            self.last_action = Some(LastAction::Kill);
            self.state.lines[self.state.cursor_line] = line[col..].to_string();
            self.set_cursor_col(0);
        } else if self.state.cursor_line > 0 {
            self.push_undo();
            self.kill_ring.push("\n", true, accumulate);
            self.last_action = Some(LastAction::Kill);
            self.join_with_previous();
        }
    }

    fn join_with_previous(&mut self) {
        let cl = self.state.cursor_line;
        let current = self.state.lines.remove(cl);
        let prev_len = self.state.lines[cl - 1].len();
        self.state.lines[cl - 1].push_str(&current);
        self.state.cursor_line -= 1;
        self.set_cursor_col(prev_len);
    }

    fn delete_to_end_of_line(&mut self) {
        self.exit_history();
        let col = self.state.cursor_col;
        let line = self.current_line().to_string();
        let accumulate = self.last_action == Some(LastAction::Kill);
        if col < line.len() {
            self.push_undo();
            self.kill_ring.push(&line[col..], false, accumulate);
            self.last_action = Some(LastAction::Kill);
            self.state.lines[self.state.cursor_line] = line[..col].to_string();
        } else if self.state.cursor_line + 1 < self.state.lines.len() {
            self.push_undo();
            self.kill_ring.push("\n", false, accumulate);
            self.last_action = Some(LastAction::Kill);
            let next = self.state.lines.remove(self.state.cursor_line + 1);
            self.state.lines[self.state.cursor_line].push_str(&next);
        }
    }

    fn delete_word_backwards(&mut self) {
        self.exit_history();
        let accumulate = self.last_action == Some(LastAction::Kill);
        if self.state.cursor_col == 0 {
            if self.state.cursor_line > 0 {
                self.push_undo();
                self.kill_ring.push("\n", true, accumulate);
                self.last_action = Some(LastAction::Kill);
                self.join_with_previous();
            }
            return;
        }
        self.push_undo();
        let line = self.current_line().to_string();
        let old = self.state.cursor_col;
        self.move_word_backwards();
        let from = self.state.cursor_col;
        self.kill_ring.push(&line[from..old], true, accumulate);
        self.last_action = Some(LastAction::Kill);
        self.state.lines[self.state.cursor_line] = format!("{}{}", &line[..from], &line[old..]);
        self.set_cursor_col(from);
    }

    fn delete_word_forward(&mut self) {
        self.exit_history();
        let accumulate = self.last_action == Some(LastAction::Kill);
        let line = self.current_line().to_string();
        if self.state.cursor_col >= line.len() {
            if self.state.cursor_line + 1 < self.state.lines.len() {
                self.push_undo();
                self.kill_ring.push("\n", false, accumulate);
                self.last_action = Some(LastAction::Kill);
                let next = self.state.lines.remove(self.state.cursor_line + 1);
                self.state.lines[self.state.cursor_line].push_str(&next);
            }
            return;
        }
        self.push_undo();
        let old = self.state.cursor_col;
        self.move_word_forwards();
        let to = self.state.cursor_col;
        self.set_cursor_col(old);
        self.kill_ring.push(&line[old..to], false, accumulate);
        self.last_action = Some(LastAction::Kill);
        self.state.lines[self.state.cursor_line] = format!("{}{}", &line[..old], &line[to..]);
    }

    fn move_word_backwards(&mut self) {
        self.last_action = None;
        if self.state.cursor_col == 0 {
            if self.state.cursor_line > 0 {
                self.state.cursor_line -= 1;
                let len = self.current_line().len();
                self.set_cursor_col(len);
            }
            return;
        }
        let line = self.current_line().to_string();
        let valid = self.valid_ids();
        let segs: &dyn Fn(&str) -> Vec<Seg<'_>> = &|t| segment_with_markers(t, SegMode::Word, &valid);
        let col = find_word_backward(&line, self.state.cursor_col, segs, &is_paste_marker);
        self.set_cursor_col(col);
    }

    fn move_word_forwards(&mut self) {
        self.last_action = None;
        let line = self.current_line().to_string();
        if self.state.cursor_col >= line.len() {
            if self.state.cursor_line + 1 < self.state.lines.len() {
                self.state.cursor_line += 1;
                self.set_cursor_col(0);
            }
            return;
        }
        let valid = self.valid_ids();
        let segs: &dyn Fn(&str) -> Vec<Seg<'_>> = &|t| segment_with_markers(t, SegMode::Word, &valid);
        let col = find_word_forward(&line, self.state.cursor_col, segs, &is_paste_marker);
        self.set_cursor_col(col);
    }

    fn yank(&mut self) {
        let Some(text) = self.kill_ring.peek().map(str::to_string) else {
            return;
        };
        self.push_undo();
        self.insert_yanked(&text);
        self.last_action = Some(LastAction::Yank);
    }

    fn yank_pop(&mut self) {
        if self.last_action != Some(LastAction::Yank) || self.kill_ring.len() <= 1 {
            return;
        }
        self.push_undo();
        self.delete_yanked();
        self.kill_ring.rotate();
        let text = self.kill_ring.peek().unwrap_or("").to_string();
        self.insert_yanked(&text);
        self.last_action = Some(LastAction::Yank);
    }

    fn insert_yanked(&mut self, text: &str) {
        self.exit_history();
        self.insert_text_internal_raw(text);
    }

    /// Insert without normalizing (yanked text is already editor text).
    fn insert_text_internal_raw(&mut self, text: &str) {
        let parts: Vec<&str> = text.split('\n').collect();
        let line = self.current_line().to_string();
        let col = self.state.cursor_col.min(line.len());
        let (before, after) = line.split_at(col);
        if parts.len() == 1 {
            self.state.lines[self.state.cursor_line] = format!("{before}{text}{after}");
            self.set_cursor_col(col + text.len());
            return;
        }
        let cl = self.state.cursor_line;
        self.state.lines[cl] = format!("{before}{}", parts[0]);
        for (i, p) in parts[1..parts.len() - 1].iter().enumerate() {
            self.state.lines.insert(cl + 1 + i, (*p).to_string());
        }
        let last_index = cl + parts.len() - 1;
        let last = parts[parts.len() - 1];
        self.state.lines.insert(last_index, format!("{last}{after}"));
        self.state.cursor_line = last_index;
        self.set_cursor_col(last.len());
    }

    fn delete_yanked(&mut self) {
        let Some(yanked) = self.kill_ring.peek().map(str::to_string) else {
            return;
        };
        let parts: Vec<&str> = yanked.split('\n').collect();
        if parts.len() == 1 {
            let col = self.state.cursor_col;
            let line = self.current_line().to_string();
            let start = col.saturating_sub(yanked.len());
            self.state.lines[self.state.cursor_line] = format!("{}{}", &line[..start], &line[col..]);
            self.set_cursor_col(start);
        } else {
            let start_line = self.state.cursor_line + 1 - parts.len();
            let start_col = self.state.lines[start_line].len().saturating_sub(parts[0].len());
            let after = self.current_line()[self.state.cursor_col..].to_string();
            let before = self.state.lines[start_line][..start_col].to_string();
            self.state
                .lines
                .splice(start_line..start_line + parts.len(), [format!("{before}{after}")]);
            self.state.cursor_line = start_line;
            self.set_cursor_col(start_col);
        }
    }

    fn jump_to_char(&mut self, c: char, dir: Jump) {
        self.last_action = None;
        let n = self.state.lines.len() as i64;
        let (end, step) = match dir {
            Jump::Forward => (n, 1),
            Jump::Backward => (-1, -1),
        };
        let mut i = self.state.cursor_line as i64;
        while i != end {
            let line = &self.state.lines[i as usize];
            let current = i as usize == self.state.cursor_line;
            let found = match (dir, current) {
                (Jump::Forward, true) => {
                    let from =
                        self.state.cursor_col + line[self.state.cursor_col..].chars().next().map_or(1, char::len_utf8);
                    line.get(from..).and_then(|s| s.find(c)).map(|k| k + from)
                }
                (Jump::Backward, true) => {
                    let col = self.state.cursor_col;
                    if col == 0 {
                        line.starts_with(c).then_some(0)
                    } else {
                        line[..col].rfind(c)
                    }
                }
                (Jump::Forward, false) => line.find(c),
                (Jump::Backward, false) => line.rfind(c),
            };
            if let Some(k) = found {
                self.state.cursor_line = i as usize;
                self.set_cursor_col(k);
                return;
            }
            i += step;
        }
    }

    // ── vertical movement ────────────────────────────────────────────────

    fn visual_lines(&self, width: usize) -> Vec<VisualLine> {
        let mut out = Vec::new();
        for (i, line) in self.state.lines.iter().enumerate() {
            if line.is_empty() {
                out.push(VisualLine {
                    logical: i,
                    start: 0,
                    len: 0,
                });
            } else if visible_width(line) <= width {
                out.push(VisualLine {
                    logical: i,
                    start: 0,
                    len: line.len(),
                });
            } else {
                for c in word_wrap_line(line, width, Some(self.segment(line, SegMode::Grapheme))) {
                    out.push(VisualLine {
                        logical: i,
                        start: c.start,
                        len: c.end - c.start,
                    });
                }
            }
        }
        out
    }

    fn find_visual_line_at(vls: &[VisualLine], line: usize, col: usize) -> usize {
        for (i, vl) in vls.iter().enumerate() {
            if vl.logical != line {
                continue;
            }
            let last_of_line = i + 1 == vls.len() || vls[i + 1].logical != vl.logical;
            if col >= vl.start {
                let offset = col - vl.start;
                if offset < vl.len || (last_of_line && offset == vl.len) {
                    return i;
                }
            }
        }
        vls.len().saturating_sub(1)
    }

    fn current_visual_line(&self, vls: &[VisualLine]) -> usize {
        Self::find_visual_line_at(vls, self.state.cursor_line, self.state.cursor_col)
    }

    fn is_on_first_visual_line(&self) -> bool {
        let vls = self.visual_lines(self.last_width);
        self.current_visual_line(&vls) == 0
    }

    fn is_on_last_visual_line(&self) -> bool {
        let vls = self.visual_lines(self.last_width);
        self.current_visual_line(&vls) + 1 == vls.len()
    }

    fn compute_vertical_move_column(&mut self, current: usize, source_max: usize, target_max: usize) -> usize {
        let has_preferred = self.preferred_visual_col.is_some();
        let in_middle = current < source_max;
        let too_short = target_max < current;
        if !has_preferred || in_middle {
            if too_short {
                self.preferred_visual_col = Some(current);
                return target_max;
            }
            self.preferred_visual_col = None;
            return current;
        }
        let preferred = self.preferred_visual_col.unwrap_or(current);
        if too_short || target_max < preferred {
            return target_max;
        }
        self.preferred_visual_col = None;
        preferred
    }

    fn move_to_visual_line(&mut self, vls: &[VisualLine], current: usize, target: usize) {
        let (Some(&cur), Some(&tgt)) = (vls.get(current), vls.get(target)) else {
            return;
        };
        let current_col = match self.snapped_from_cursor_col {
            Some(snapped) => {
                let k = Self::find_visual_line_at(vls, cur.logical, snapped);
                snapped.saturating_sub(vls[k].start)
            }
            None => self.state.cursor_col.saturating_sub(cur.start),
        };
        let last_source = current + 1 == vls.len() || vls[current + 1].logical != cur.logical;
        let source_max = if last_source {
            cur.len
        } else {
            cur.len.saturating_sub(1)
        };
        let last_target = target + 1 == vls.len() || vls[target + 1].logical != tgt.logical;
        let target_max = if last_target {
            tgt.len
        } else {
            tgt.len.saturating_sub(1)
        };
        let col = self.compute_vertical_move_column(current_col, source_max, target_max);
        self.state.cursor_line = tgt.logical;
        let line = self.state.lines[tgt.logical].clone();
        let mut cursor = (tgt.start + col).min(line.len());
        // Never inside a cluster: snap to its start (or step past a cluster split over lines).
        for seg in self.segment(&line, SegMode::Grapheme) {
            if seg.index > cursor {
                break;
            }
            if seg.text.chars().count() <= 1 && seg.text.len() <= 1 {
                continue;
            }
            if cursor < seg.index + seg.text.len() {
                if seg.index == cursor {
                    break;
                }
                let continuation = seg.index < tgt.start;
                let moving_down = target > current;
                if continuation && moving_down {
                    let seg_end = seg.index + seg.text.len();
                    let mut next = target + 1;
                    while next < vls.len() && vls[next].logical == tgt.logical && vls[next].start < seg_end {
                        next += 1;
                    }
                    if next < vls.len() {
                        self.move_to_visual_line(vls, current, next);
                        return;
                    }
                }
                self.snapped_from_cursor_col = Some(cursor);
                cursor = seg.index;
                self.state.cursor_col = cursor;
                return;
            }
        }
        self.state.cursor_col = cursor;
        self.snapped_from_cursor_col = None;
    }

    fn move_cursor(&mut self, dl: i64, dc: i64) {
        self.last_action = None;
        let vls = self.visual_lines(self.last_width);
        let current = self.current_visual_line(&vls);
        if dl != 0 {
            let target = current as i64 + dl;
            if target >= 0 && (target as usize) < vls.len() {
                self.move_to_visual_line(&vls, current, target as usize);
            }
        }
        if dc != 0 {
            let line = self.current_line().to_string();
            let col = self.state.cursor_col;
            if dc > 0 {
                if col < line.len() {
                    let step = self
                        .segment(&line[col..], SegMode::Grapheme)
                        .first()
                        .map_or(1, |s| s.text.len());
                    self.set_cursor_col(col + step);
                } else if self.state.cursor_line + 1 < self.state.lines.len() {
                    self.state.cursor_line += 1;
                    self.set_cursor_col(0);
                } else if let Some(vl) = vls.get(current) {
                    self.preferred_visual_col = Some(col.saturating_sub(vl.start));
                }
            } else if col > 0 {
                let step = self
                    .segment(&line[..col], SegMode::Grapheme)
                    .last()
                    .map_or(1, |s| s.text.len());
                self.set_cursor_col(col - step);
            } else if self.state.cursor_line > 0 {
                self.state.cursor_line -= 1;
                let len = self.current_line().len();
                self.set_cursor_col(len);
            }
        }
        if self.ac_state.is_some() {
            self.update_autocomplete();
        }
    }

    fn page_scroll(&mut self, direction: i64) {
        self.last_action = None;
        let page = self.max_visible_lines() as i64;
        let vls = self.visual_lines(self.last_width);
        let current = self.current_visual_line(&vls);
        let target = (current as i64 + direction * page).clamp(0, vls.len() as i64 - 1);
        self.move_to_visual_line(&vls, current, target as usize);
    }

    /// A click on row `y` (0 is the top rule) at column `x` of the rendered box, `width` wide: the
    /// cursor goes there, or the completion list takes it. Returns the item a click chose.
    pub fn click(&mut self, x: usize, y: usize, width: usize) -> Option<SelectItem> {
        let padding_x = self.padding_x.min(width.saturating_sub(1) / 2);
        let ac_start = self.rendered_visible_lines + 2;
        if self.ac_state.is_some() && y >= ac_start && y < ac_start + self.rendered_autocomplete_height {
            let item = self.ac_list.as_mut().and_then(|l| l.click(y - ac_start))?;
            self.take_completion(&item);
            self.cancel_autocomplete();
            return Some(item);
        }
        if y == 0 || y > self.rendered_visible_lines {
            return None;
        }
        let vls = self.visual_lines(self.last_width);
        let k = self.scroll_offset + y - 1;
        let vl = *vls.get(k)?;
        let line = self.state.lines[vl.logical].clone();
        let chunk = &line[vl.start..vl.start + vl.len];
        let target = x.saturating_sub(padding_x);
        let mut col = 0;
        let mut index = chunk.len();
        let mut last_index = 0;
        for seg in self.segment(chunk, SegMode::Grapheme) {
            let next = col + visible_width(seg.text);
            last_index = seg.index;
            if target < next {
                index = seg.index;
                break;
            }
            col = next;
        }
        let last_segment = k + 1 == vls.len() || vls[k + 1].logical != vl.logical;
        if !last_segment && index == chunk.len() && !chunk.is_empty() {
            index = last_index;
        }
        self.state.cursor_line = vl.logical;
        self.set_cursor_col(vl.start + index);
        self.last_action = None;
        self.exit_history();
        if self.ac_state.is_some() {
            self.update_autocomplete();
        }
        None
    }

    // ── completion ──────────────────────────────────────────────────────────

    fn is_at_start_of_message(&self) -> bool {
        if self.state.cursor_line != 0 {
            return false;
        }
        let before = js_trim(&self.current_line()[..self.state.cursor_col]);
        before.is_empty() || before == "/"
    }

    fn is_in_slash_context(&self, before: &str) -> bool {
        self.state.cursor_line == 0 && before.trim_start_matches(is_js_whitespace).starts_with('/')
    }

    fn take_completion(&mut self, item: &SelectItem) {
        self.push_undo();
        self.last_action = None;
        let applied = AutocompleteProvider::apply_completion(
            &self.state.lines,
            self.state.cursor_line,
            self.state.cursor_col,
            item,
            &self.ac_prefix,
        );
        self.apply(applied);
    }

    fn apply(&mut self, a: Applied) {
        self.state.lines = a.lines;
        self.state.cursor_line = a.cursor_line;
        self.set_cursor_col(a.cursor_col);
    }

    fn handle_tab_completion(&mut self) {
        if self.provider.is_none() {
            return;
        }
        let before = self.current_line()[..self.state.cursor_col].to_string();
        if self.is_in_slash_context(&before) && !before.trim_start_matches(is_js_whitespace).contains(' ') {
            self.request_autocomplete(false, true);
        } else {
            self.request_autocomplete(true, true);
        }
    }

    fn update_autocomplete(&mut self) {
        let Some(state) = self.ac_state else { return };
        if self.provider.is_none() {
            return;
        }
        self.request_autocomplete(state == AcState::Force, false);
    }

    fn cancel_request(&mut self) {
        self.ac_generation += 1;
        self.pending_argument = None;
    }

    fn cancel_autocomplete(&mut self) {
        self.cancel_request();
        self.ac_state = None;
        self.ac_list = None;
        self.ac_prefix.clear();
    }

    fn snapshot(&self) -> (String, usize, usize) {
        (self.get_text(), self.state.cursor_line, self.state.cursor_col)
    }

    fn request_autocomplete(&mut self, force: bool, explicit_tab: bool) {
        let Some(provider) = &self.provider else { return };
        if force
            && !AutocompleteProvider::should_trigger_file_completion(
                &self.state.lines,
                self.state.cursor_line,
                self.state.cursor_col,
            )
        {
            return;
        }
        let suggest = provider.suggestions(&self.state.lines, self.state.cursor_line, self.state.cursor_col, force);
        self.cancel_request();
        match suggest {
            Suggest::None => self.finish(None, force, explicit_tab),
            Suggest::Items { items, prefix } => self.finish(Some((items, prefix)), force, explicit_tab),
            Suggest::Argument { command, argument } => {
                self.pending_argument = Some(ArgumentRequest {
                    command,
                    argument,
                    generation: self.ac_generation,
                    snapshot: self.snapshot(),
                    force,
                    explicit_tab,
                });
            }
        }
    }

    /// The candidates a command gave for its argument (an empty list offers nothing).
    pub fn resolve_argument(&mut self, req: ArgumentRequest, items: Vec<SelectItem>) {
        if req.generation != self.ac_generation || req.snapshot != self.snapshot() {
            return;
        }
        let prefix = req.argument.clone();
        self.finish(
            (!items.is_empty()).then_some((items, prefix)),
            req.force,
            req.explicit_tab,
        );
    }

    fn finish(&mut self, result: Option<(Vec<SelectItem>, String)>, force: bool, explicit_tab: bool) {
        let Some((items, prefix)) = result.filter(|(items, _)| !items.is_empty()) else {
            self.cancel_autocomplete();
            return;
        };
        if force && explicit_tab && items.len() == 1 {
            self.push_undo();
            self.last_action = None;
            let applied = AutocompleteProvider::apply_completion(
                &self.state.lines,
                self.state.cursor_line,
                self.state.cursor_col,
                &items[0],
                &prefix,
            );
            self.apply(applied);
            return;
        }
        let layout = if prefix.starts_with('/') {
            SLASH_LAYOUT
        } else {
            ARGUMENT_LAYOUT
        };
        let mut list = SelectList::new(items.clone(), self.ac_max_visible, self.theme.select, layout);
        if let Some(best) = best_match_index(&items, &prefix) {
            list.set_selected_index(best);
        }
        self.ac_prefix = prefix;
        self.ac_list = Some(list);
        self.ac_state = Some(if force { AcState::Force } else { AcState::Regular });
    }
}

/// Exact value first, else the first value starting with the prefix.
fn best_match_index(items: &[SelectItem], prefix: &str) -> Option<usize> {
    if prefix.is_empty() {
        return None;
    }
    let mut first = None;
    for (i, item) in items.iter().enumerate() {
        if item.value == prefix {
            return Some(i);
        }
        if first.is_none() && item.value.starts_with(prefix) {
            first = Some(i);
        }
    }
    first
}

/// pi-tui's trigger pattern for `@` and `#`: at a token start, `@"…` or a trigger character
/// followed by anything up to the cursor that is not a separator.
fn trigger_pattern(before: &str) -> bool {
    let sep = |c: char| is_js_whitespace(c) || crate::text::width::is_cjk_punctuation(c);
    // The token: from after the last separator.
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| sep(*c))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let token = &before[start..];
    if token.starts_with(TRIGGER_CHARACTERS) {
        return true;
    }
    // `@"…` may hold separators inside the quotes.
    if let Some(at) = before.rfind("@\"") {
        let pre = &before[..at];
        let tail = &before[at + 2..];
        return !tail.contains('"') && (pre.is_empty() || pre.chars().last().is_some_and(sep));
    }
    false
}

/// `\x1b[<code>;5u` (ctrl+letter in CSI u) inside a paste, as the control character it is.
fn decode_csi_u_ctrl(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("\x1b[") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && after[digits..].starts_with(";5u") {
            let cp: u32 = after[..digits].parse().unwrap_or(0);
            let decoded = match cp {
                97..=122 => char::from_u32(cp - 96),
                65..=90 => char::from_u32(cp - 64),
                _ => None,
            };
            match decoded {
                Some(c) => out.push(c),
                None => out.push_str(&rest[i..i + 2 + digits + 3]),
            }
            rest = &after[digits + 3..];
        } else {
            out.push_str("\x1b[");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Whether any char of `s` is whitespace (for callers outside the module).
pub fn contains_whitespace(s: &str) -> bool {
    has_whitespace(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::key_event;
    use crate::theme::EDITOR_THEME;

    fn editor() -> Editor {
        Editor::new(EDITOR_THEME, 1)
    }

    fn typ(e: &mut Editor, s: &str) {
        let kb = Keybindings::new();
        for c in s.chars() {
            e.handle_key(
                &KeyEvent::new(crossterm::event::KeyCode::Char(c), crossterm::event::KeyModifiers::NONE),
                &kb,
            );
        }
    }

    fn key(e: &mut Editor, id: &str) {
        e.handle_key(&key_event(id), &Keybindings::new());
    }

    #[test]
    fn a_long_paste_folds_into_a_marker_that_expands_on_submit() {
        let mut e = editor();
        let text = (1..=12).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        e.handle_paste(&text);
        assert_eq!(e.get_text(), "[paste #1 +12 lines]");
        key(&mut e, "enter");
        assert_eq!(e.take_submitted(), Some(text));
    }

    #[test]
    fn a_paste_over_a_thousand_chars_folds_and_a_short_one_does_not() {
        let mut e = editor();
        e.handle_paste(&"x".repeat(1001));
        assert_eq!(e.get_text(), "[paste #1 1001 chars]");
        let mut e = editor();
        e.handle_paste("a\nb");
        assert_eq!(e.get_lines(), vec!["a", "b"]);
    }

    #[test]
    fn a_typed_marker_is_text_and_an_edited_marker_does_not_expand() {
        let mut e = editor();
        typ(&mut e, "[paste #1 3 chars]");
        key(&mut e, "enter");
        assert_eq!(e.take_submitted().as_deref(), Some("[paste #1 3 chars]"));
    }

    #[test]
    fn backspace_deletes_a_marker_whole_and_renumbers_later_ones() {
        let mut e = editor();
        e.handle_paste(&"a".repeat(1001));
        e.handle_paste(&"b".repeat(1002));
        assert_eq!(e.get_text(), "[paste #1 1001 chars][paste #2 1002 chars]");
        key(&mut e, "left");
        key(&mut e, "backspace");
        assert_eq!(e.get_text(), "[paste #1 1002 chars]");
        key(&mut e, "enter");
        assert_eq!(e.take_submitted(), Some("b".repeat(1002)));
    }

    #[test]
    fn a_path_pasted_after_a_word_gets_a_space() {
        let mut e = editor();
        typ(&mut e, "see");
        e.handle_paste("/tmp/x");
        assert_eq!(e.get_text(), "see /tmp/x");
    }

    #[test]
    fn history_walks_newest_first_and_keeps_the_draft() {
        let mut e = editor();
        e.add_to_history("one");
        e.add_to_history("two");
        e.add_to_history("two");
        typ(&mut e, "dr");
        key(&mut e, "home");
        key(&mut e, "up");
        assert_eq!(e.get_text(), "two");
        key(&mut e, "up");
        assert_eq!(e.get_text(), "one");
        key(&mut e, "down");
        key(&mut e, "down");
        assert_eq!(e.get_text(), "dr");
    }

    #[test]
    fn trailing_backslash_makes_a_newline_and_kill_yank_round_trip() {
        let mut e = editor();
        typ(&mut e, "a\\");
        key(&mut e, "enter");
        assert_eq!(e.get_lines(), vec!["a", ""]);
        let mut e = editor();
        typ(&mut e, "hello world");
        key(&mut e, "ctrl+w");
        assert_eq!(e.get_text(), "hello ");
        key(&mut e, "ctrl+y");
        assert_eq!(e.get_text(), "hello world");
        key(&mut e, "ctrl+-");
        assert_eq!(e.get_text(), "hello ");
    }

    #[test]
    fn the_box_renders_rules_and_a_reversed_cursor() {
        let mut e = editor();
        e.focused = true;
        typ(&mut e, "hi");
        let lines = e.render(20);
        assert_eq!(lines.len(), 3);
        assert!(lines[1].contains(CURSOR_MARKER));
        assert!(lines[1].contains("\x1b[7m \x1b[0m"));
        assert_eq!(visible_width(&lines[1]), 20);
    }

    #[test]
    fn slash_opens_the_command_list_and_enter_completes_and_submits() {
        let mut e = editor();
        e.set_autocomplete_provider(AutocompleteProvider::new(
            vec![crate::ui::autocomplete::CommandEntry {
                name: "model".into(),
                description: "change".into(),
                argument_hint: None,
                completes: false,
            }],
            "/",
        ));
        typ(&mut e, "/mo");
        assert!(e.is_showing_autocomplete());
        key(&mut e, "enter");
        assert_eq!(e.take_submitted().as_deref(), Some("/model"));
    }

    #[test]
    fn csi_u_ctrl_letters_in_a_paste_decode() {
        assert_eq!(decode_csi_u_ctrl("a\x1b[99;5ub"), "a\x03b");
        assert_eq!(decode_csi_u_ctrl("a\x1b[1;5ub"), "a\x1b[1;5ub");
    }
}
