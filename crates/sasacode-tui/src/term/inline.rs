//! The inline renderer: the UI drawn in the normal screen below the shell prompt, redrawing only
//! the lines that changed since the last frame (pi-tui `TuiMainScreen.doRender`). Lines that
//! scroll off the top stay in the terminal's scrollback; a change up there redraws everything.

use crate::text::{CURSOR_MARKER, SEGMENT_RESET, extract_ansi_code, slice_by_column, visible_width};

/// Where the hardware cursor goes (IME candidates follow it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPos {
    pub row: usize,
    pub col: usize,
}

/// Find the cursor marker in the last `height` lines, remove it and say where it was.
pub fn extract_cursor_position(lines: &mut [String], height: usize) -> Option<CursorPos> {
    let top = lines.len().saturating_sub(height);
    for row in (top..lines.len()).rev() {
        if let Some(i) = lines[row].find(CURSOR_MARKER) {
            let col = visible_width(&lines[row][..i]);
            lines[row].replace_range(i..i + CURSOR_MARKER.len(), "");
            return Some(CursorPos { row, col });
        }
    }
    None
}

/// Text as written to the terminal: Thai/Lao AM decomposed (some terminals repaint the precomposed
/// form badly) and visible tabs expanded to the three cells layout counted.
pub fn normalize_terminal_output(s: &str) -> String {
    let mut s = if s.contains(['\u{0E33}', '\u{0EB3}']) {
        s.replace('\u{0E33}', "\u{0E4D}\u{0E32}")
            .replace('\u{0EB3}', "\u{0ECD}\u{0EB2}")
    } else {
        s.to_string()
    };
    if s.contains('\t') {
        let mut out = String::with_capacity(s.len() + 8);
        let mut i = 0;
        let b = s.as_bytes();
        while i < b.len() {
            if let Some(len) = extract_ansi_code(&s, i) {
                out.push_str(&s[i..i + len]);
                i += len;
                continue;
            }
            let Some(c) = s[i..].chars().next() else { break };
            if c == '\t' {
                out.push_str("   ");
            } else {
                out.push(c);
            }
            i += c.len_utf8();
        }
        s = out;
    }
    s
}

/// The finished lines of a frame: each line normalized, cut to the width (pi-tui stops with an
/// error on an overlong line; a cut line is the safer failure here) and ended with a full reset.
pub fn finish_lines(lines: &mut [String], width: usize) {
    for line in lines.iter_mut() {
        let mut l = normalize_terminal_output(line);
        if visible_width(&l) > width {
            l = slice_by_column(&l, 0, width, true);
        }
        l.push_str(SEGMENT_RESET);
        *line = l;
    }
}

#[derive(Debug, Default)]
pub struct InlineRenderer {
    previous_lines: Vec<String>,
    previous_width: i64,
    previous_height: i64,
    cursor_row: i64,
    hardware_cursor_row: i64,
    max_lines_rendered: i64,
    previous_viewport_top: i64,
    /// Full redraws so far (tests check that a frame was not redrawn whole).
    pub full_redraws: usize,
}

fn move_rows(out: &mut String, diff: i64) {
    if diff > 0 {
        out.push_str(&format!("\x1b[{diff}B"));
    } else if diff < 0 {
        out.push_str(&format!("\x1b[{}A", -diff));
    }
}

impl InlineRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the screen: the next frame is drawn whole after clearing it.
    pub fn reset(&mut self) {
        self.previous_lines.clear();
        self.previous_width = -1;
        self.previous_height = -1;
        self.cursor_row = 0;
        self.hardware_cursor_row = 0;
        self.max_lines_rendered = 0;
        self.previous_viewport_top = 0;
    }

    /// What leaving writes: the cursor below the last line, so the shell prompt starts under the UI.
    pub fn stop_sequence(&self, preserve_screen: bool) -> String {
        if preserve_screen || self.previous_lines.is_empty() {
            return String::new();
        }
        let mut out = String::from(" ");
        move_rows(&mut out, self.previous_lines.len() as i64 - self.hardware_cursor_row);
        out.push_str("\r\n");
        out
    }

    /// The bytes that turn the last frame into `lines` on a `width` × `height` terminal. The lines
    /// are raw component output (cursor marker included).
    pub fn render(&mut self, mut new_lines: Vec<String>, width: usize, height: usize) -> String {
        let w = width as i64;
        let h = height as i64;
        let width_changed = self.previous_width != 0 && self.previous_width != w;
        let height_changed = self.previous_height != 0 && self.previous_height != h;
        let previous_buffer_length = if self.previous_height > 0 {
            self.previous_viewport_top + self.previous_height
        } else {
            h
        };
        let mut prev_viewport_top = if height_changed {
            (previous_buffer_length - h).max(0)
        } else {
            self.previous_viewport_top
        };
        let mut viewport_top = prev_viewport_top;
        let mut hardware_cursor_row = self.hardware_cursor_row;

        let cursor = extract_cursor_position(&mut new_lines, height);
        finish_lines(&mut new_lines, width);
        let new_len = new_lines.len() as i64;
        let mut out = String::new();

        if (self.previous_lines.is_empty() && !width_changed && !height_changed) || width_changed || height_changed {
            let clear = width_changed || height_changed;
            self.full_render(&mut out, new_lines, clear, (width, height), cursor);
            return out;
        }

        let prev_len = self.previous_lines.len() as i64;
        let mut first_changed: i64 = -1;
        let mut last_changed: i64 = -1;
        let max_lines = new_lines.len().max(self.previous_lines.len());
        for i in 0..max_lines {
            let old = self.previous_lines.get(i).map(String::as_str).unwrap_or("");
            let new = new_lines.get(i).map(String::as_str).unwrap_or("");
            if old != new {
                if first_changed == -1 {
                    first_changed = i as i64;
                }
                last_changed = i as i64;
            }
        }
        let appended = new_len > prev_len;
        if appended {
            if first_changed == -1 {
                first_changed = prev_len;
            }
            last_changed = new_len - 1;
        }
        let append_start = appended && first_changed == prev_len && first_changed > 0;

        if first_changed == -1 {
            self.position_hardware_cursor(&mut out, cursor, new_len);
            self.previous_viewport_top = prev_viewport_top;
            self.previous_height = h;
            return out;
        }

        if first_changed >= new_len {
            if prev_len > new_len {
                let mut frame = String::from("\x1b[?2026h");
                let target_row = (new_len - 1).max(0);
                if target_row < prev_viewport_top {
                    self.full_render(&mut out, new_lines, true, (width, height), cursor);
                    return out;
                }
                let current_screen_row = hardware_cursor_row - prev_viewport_top;
                move_rows(&mut frame, (target_row - viewport_top) - current_screen_row);
                frame.push('\r');
                let extra = prev_len - new_len;
                if extra > h {
                    self.full_render(&mut out, new_lines, true, (width, height), cursor);
                    return out;
                }
                let clear_start_offset = if new_len == 0 { 0 } else { 1 };
                if extra > 0 && clear_start_offset > 0 {
                    frame.push_str(&format!("\x1b[{clear_start_offset}B"));
                }
                for i in 0..extra {
                    frame.push_str("\r\x1b[2K");
                    if i < extra - 1 {
                        frame.push_str("\x1b[1B");
                    }
                }
                let move_back = (extra - 1 + clear_start_offset).max(0);
                if move_back > 0 {
                    frame.push_str(&format!("\x1b[{move_back}A"));
                }
                frame.push_str("\x1b[?2026l");
                out.push_str(&frame);
                self.cursor_row = target_row;
                self.hardware_cursor_row = target_row;
            }
            self.position_hardware_cursor(&mut out, cursor, new_len);
            self.previous_lines = new_lines;
            self.previous_width = w;
            self.previous_height = h;
            self.previous_viewport_top = prev_viewport_top;
            return out;
        }

        if first_changed < prev_viewport_top {
            self.full_render(&mut out, new_lines, true, (width, height), cursor);
            return out;
        }

        out.push_str("\x1b[?2026h");
        let prev_viewport_bottom = prev_viewport_top + h - 1;
        let move_target_row = if append_start { first_changed - 1 } else { first_changed };
        if move_target_row > prev_viewport_bottom {
            let current_screen_row = (hardware_cursor_row - prev_viewport_top).clamp(0, h - 1);
            let move_to_bottom = h - 1 - current_screen_row;
            if move_to_bottom > 0 {
                out.push_str(&format!("\x1b[{move_to_bottom}B"));
            }
            let scroll = move_target_row - prev_viewport_bottom;
            out.push_str(&"\r\n".repeat(scroll as usize));
            prev_viewport_top += scroll;
            viewport_top += scroll;
            hardware_cursor_row = move_target_row;
        }
        let current_screen_row = hardware_cursor_row - prev_viewport_top;
        move_rows(&mut out, (move_target_row - viewport_top) - current_screen_row);
        out.push_str(if append_start { "\r\n" } else { "\r" });
        let render_end = last_changed.min(new_len - 1);
        for i in first_changed..=render_end {
            if i > first_changed {
                out.push_str("\r\n");
            }
            out.push_str("\x1b[2K");
            out.push_str(&new_lines[i as usize]);
        }
        let mut final_cursor_row = render_end;
        if prev_len > new_len {
            if render_end < new_len - 1 {
                let move_down = new_len - 1 - render_end;
                out.push_str(&format!("\x1b[{move_down}B"));
                final_cursor_row = new_len - 1;
            }
            let extra = prev_len - new_len;
            for _ in new_len..prev_len {
                out.push_str("\r\n\x1b[2K");
            }
            out.push_str(&format!("\x1b[{extra}A"));
        }
        out.push_str("\x1b[?2026l");
        self.cursor_row = (new_len - 1).max(0);
        self.hardware_cursor_row = final_cursor_row;
        self.max_lines_rendered = self.max_lines_rendered.max(new_len);
        self.previous_viewport_top = prev_viewport_top.max(final_cursor_row - h + 1);
        self.position_hardware_cursor(&mut out, cursor, new_len);
        self.previous_lines = new_lines;
        self.previous_width = w;
        self.previous_height = h;
        out
    }

    fn full_render(
        &mut self,
        out: &mut String,
        new_lines: Vec<String>,
        clear: bool,
        (width, height): (usize, usize),
        cursor: Option<CursorPos>,
    ) {
        self.full_redraws += 1;
        out.push_str("\x1b[?2026h");
        if clear {
            out.push_str("\x1b[2J\x1b[H\x1b[3J");
        }
        for (i, line) in new_lines.iter().enumerate() {
            if i > 0 {
                out.push_str("\r\n");
            }
            out.push_str(line);
        }
        out.push_str("\x1b[?2026l");
        let n = new_lines.len() as i64;
        self.cursor_row = (n - 1).max(0);
        self.hardware_cursor_row = self.cursor_row;
        self.max_lines_rendered = if clear { n } else { self.max_lines_rendered.max(n) };
        let buffer_length = (height as i64).max(n);
        self.previous_viewport_top = (buffer_length - height as i64).max(0);
        self.position_hardware_cursor(out, cursor, n);
        self.previous_width = width as i64;
        self.previous_lines = new_lines;
        self.previous_height = height as i64;
    }

    fn position_hardware_cursor(&mut self, out: &mut String, cursor: Option<CursorPos>, total: i64) {
        let Some(pos) = cursor.filter(|_| total > 0) else {
            out.push_str("\x1b[?25l");
            return;
        };
        let target_row = (pos.row as i64).clamp(0, total - 1);
        move_rows(out, target_row - self.hardware_cursor_row);
        out.push_str(&format!("\x1b[{}G", pos.col + 1));
        self.hardware_cursor_row = target_row;
        out.push_str("\x1b[?25l");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Just enough of a terminal to replay what the renderer writes: a screen of `h` rows over a
    /// scrollback, cursor moves, line feeds that scroll at the bottom, and line/screen erases.
    struct Vt {
        lines: Vec<Vec<char>>,
        top: usize,
        row: usize,
        col: usize,
        h: usize,
    }

    impl Vt {
        fn new(h: usize) -> Vt {
            Vt {
                lines: vec![Vec::new(); h],
                top: 0,
                row: 0,
                col: 0,
                h,
            }
        }

        fn line(&mut self) -> &mut Vec<char> {
            let i = self.top + self.row;
            while self.lines.len() <= i {
                self.lines.push(Vec::new());
            }
            &mut self.lines[i]
        }

        fn feed(&mut self, s: &str) {
            let c: Vec<char> = s.chars().collect();
            let mut i = 0;
            while i < c.len() {
                match c[i] {
                    '\x1b' if c.get(i + 1) == Some(&'[') => {
                        let mut j = i + 2;
                        while j < c.len() && !c[j].is_ascii_alphabetic() {
                            j += 1;
                        }
                        let params: String = c[i + 2..j].iter().collect();
                        let n: usize = params.trim_start_matches('?').parse().unwrap_or(1);
                        match (c.get(j), params.starts_with('?')) {
                            (_, true) | (Some('m'), _) => {}
                            (Some('A'), _) => self.row = self.row.saturating_sub(n),
                            (Some('B'), _) => self.row = (self.row + n).min(self.h - 1),
                            (Some('G'), _) => self.col = n - 1,
                            (Some('H'), _) => (self.row, self.col) = (0, 0),
                            (Some('K'), _) => self.line().clear(),
                            (Some('J'), _) if n == 2 => {
                                for r in self.top..self.top + self.h {
                                    if let Some(l) = self.lines.get_mut(r) {
                                        l.clear();
                                    }
                                }
                            }
                            (Some('J'), _) if n == 3 => {
                                self.lines.drain(..self.top);
                                self.top = 0;
                            }
                            other => panic!("unexpected CSI {params}{other:?}"),
                        }
                        i = j + 1;
                    }
                    // OSC and APC run to BEL.
                    '\x1b' if matches!(c.get(i + 1), Some(']' | '_')) => {
                        while i < c.len() && c[i] != '\x07' {
                            i += 1;
                        }
                        i += 1;
                    }
                    '\r' => {
                        self.col = 0;
                        i += 1;
                    }
                    '\n' => {
                        if self.row == self.h - 1 {
                            self.top += 1;
                        } else {
                            self.row += 1;
                        }
                        self.line();
                        i += 1;
                    }
                    ch => {
                        let col = self.col;
                        let l = self.line();
                        while l.len() <= col {
                            l.push(' ');
                        }
                        l[col] = ch;
                        self.col += 1;
                        i += 1;
                    }
                }
            }
        }

        /// The visible rows, trailing blanks trimmed.
        fn screen(&self) -> Vec<String> {
            (self.top..self.top + self.h)
                .map(|r| {
                    let l: String = self.lines.get(r).map(|l| l.iter().collect()).unwrap_or_default();
                    l.trim_end().to_string()
                })
                .collect()
        }

        /// Everything scrolled off the top.
        fn scrollback(&self) -> Vec<String> {
            self.lines[..self.top].iter().map(|l| l.iter().collect()).collect()
        }
    }

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// What a `h`-row screen should show: the frame's last rows, blank rows under a short one.
    fn shown(frame: &[&str], h: usize) -> Vec<String> {
        let from = frame.len().saturating_sub(h);
        let mut out = lines(&frame[from..]);
        out.resize(h, String::new());
        out
    }

    struct Rig {
        r: InlineRenderer,
        vt: Vt,
        w: usize,
        h: usize,
    }

    impl Rig {
        fn new(w: usize, h: usize) -> Rig {
            Rig {
                r: InlineRenderer::new(),
                vt: Vt::new(h),
                w,
                h,
            }
        }
        fn frame(&mut self, frame: &[&str]) -> String {
            let out = self.r.render(lines(frame), self.w, self.h);
            self.vt.feed(&out);
            assert_eq!(self.vt.screen(), shown(frame, self.h), "after {out:?}");
            out
        }
    }

    #[test]
    fn only_changed_rows_are_written_and_a_shorter_frame_clears_what_it_left() {
        let mut rig = Rig::new(20, 6);
        rig.frame(&["header", "one", "two", "input"]);
        let out = rig.frame(&["header", "one", "TWO", "input"]);
        assert!(!out.contains("header") && !out.contains("one"), "{out:?}");
        rig.frame(&["header", "one", "TWO", "three", "input"]);
        rig.frame(&["header", "one"]);
        let out = rig.frame(&["header", "one"]);
        assert!(
            !out.contains("header") && !out.contains("one"),
            "an unchanged frame rewrote rows: {out:?}"
        );
        assert_eq!(rig.r.full_redraws, 1);
    }

    #[test]
    fn a_frame_taller_than_the_screen_scrolls_and_keeps_the_rest_in_the_scrollback() {
        let mut rig = Rig::new(20, 4);
        let mut frame: Vec<String> = (0..7).map(|i| format!("line {i}")).collect();
        let as_refs = |f: &Vec<String>| f.iter().map(String::as_str).collect::<Vec<_>>().join("\n");
        rig.frame(&as_refs(&frame).split('\n').collect::<Vec<_>>());
        assert_eq!(rig.vt.scrollback(), lines(&["line 0", "line 1", "line 2"]));
        // The bottom changes in place, and appended lines scroll the screen.
        frame[6] = "line 6 edited".into();
        rig.frame(&as_refs(&frame).split('\n').collect::<Vec<_>>());
        frame.push("line 7".into());
        frame.push("line 8".into());
        rig.frame(&as_refs(&frame).split('\n').collect::<Vec<_>>());
        assert_eq!(rig.vt.scrollback().last().map(String::as_str), Some("line 4"));
        assert_eq!(rig.r.full_redraws, 1);
        // A change above the screen cannot be reached by moving the cursor: everything is drawn again.
        frame[0] = "line 0 edited".into();
        rig.frame(&as_refs(&frame).split('\n').collect::<Vec<_>>());
        assert_eq!(rig.r.full_redraws, 2);
    }

    #[test]
    fn a_new_width_or_height_redraws_the_whole_frame_on_a_cleared_screen() {
        let mut rig = Rig::new(20, 5);
        rig.frame(&["a long line of text", "b"]);
        rig.w = 8;
        let out = rig.frame(&["a long", "line of", "text", "b"]);
        assert!(out.contains("\x1b[2J"), "{out:?}");
        assert_eq!(rig.r.full_redraws, 2);
        rig.h = 3;
        rig.vt.h = 3;
        rig.frame(&["a long", "line of", "text", "b"]);
        assert_eq!(rig.r.full_redraws, 3);
        // A line too wide for the screen is cut, not wrapped by the terminal.
        let out = rig.r.render(lines(&["0123456789abc", "b"]), rig.w, rig.h);
        rig.vt.feed(&out);
        assert_eq!(rig.vt.screen(), lines(&["01234567", "b", ""]));
    }

    #[test]
    fn leaving_puts_the_cursor_under_the_last_line() {
        let mut rig = Rig::new(20, 6);
        rig.frame(&["one", "two", "three"]);
        let tail = rig.r.stop_sequence(false);
        rig.vt.feed(&tail);
        // As pi-tui: down to the row after the content, then a line feed (on a full screen the move
        // stops at the bottom and the feed scrolls, so the prompt comes right under the UI).
        assert_eq!(rig.vt.row, 4);
        assert_eq!(rig.vt.col, 0);
        assert_eq!(rig.vt.screen()[..3], lines(&["one", "two", "three"]));
        assert!(rig.r.stop_sequence(true).is_empty());
    }
}
