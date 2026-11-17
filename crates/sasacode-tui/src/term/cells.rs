//! ANSI lines into ratatui cells, for the full-screen renderer. The components keep rendering ANSI
//! strings (pi-tui's model); only the finished screen becomes a [`Buffer`], so ratatui's diff and
//! its crossterm backend decide what to rewrite.
//!
//! Every painted cell carries `CellDiffOption::ForcedWidth` with the width this crate computed
//! (`grapheme_width`, the ambiguous width included), so ratatui moves the cursor by the same widths
//! the layout used; the columns a wide cell covers are marked `Skip`. A link is kept per cell: the
//! cell's symbol is wrapped in the OSC 8 open and close, which `ForcedWidth` keeps from counting.

use std::num::NonZeroU16;

use ratatui::buffer::{Buffer, Cell, CellDiffOption};
use ratatui::style::{Color, Modifier};

use crate::text::ansi::parse_osc8;
use crate::text::{Piece, grapheme_width, graphemes, pieces};

/// The style and link in effect while a line is read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pen {
    pub fg: Color,
    pub bg: Color,
    pub underline: Color,
    pub modifier: Modifier,
    pub link: Option<String>,
}

fn basic(n: u16) -> Color {
    match n {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        _ => Color::Gray,
    }
}

fn bright(n: u16) -> Color {
    match n {
        0 => Color::DarkGray,
        1 => Color::LightRed,
        2 => Color::LightGreen,
        3 => Color::LightYellow,
        4 => Color::LightBlue,
        5 => Color::LightMagenta,
        6 => Color::LightCyan,
        _ => Color::White,
    }
}

/// `38;5;n` / `38;2;r;g;b` (and the colon forms): the colour and how many parameters it used.
fn extended(params: &[Vec<u16>], i: usize) -> (Option<Color>, usize) {
    let head = &params[i];
    // Colon form: everything in one parameter.
    if head.len() > 1 {
        return match head.get(1) {
            Some(5) => (head.get(2).map(|&n| Color::Indexed(n as u8)), 1),
            Some(2) => {
                // 38:2::r:g:b or 38:2:r:g:b
                let rgb: Vec<u16> = head[2..].to_vec();
                let rgb = if rgb.len() >= 4 { rgb[1..].to_vec() } else { rgb };
                match rgb.as_slice() {
                    [r, g, b, ..] => (Some(Color::Rgb(*r as u8, *g as u8, *b as u8)), 1),
                    _ => (None, 1),
                }
            }
            _ => (None, 1),
        };
    }
    let at = |k: usize| params.get(i + k).and_then(|p| p.first().copied());
    match at(1) {
        Some(5) => (at(2).map(|n| Color::Indexed(n as u8)), 3),
        Some(2) => match (at(2), at(3), at(4)) {
            (Some(r), Some(g), Some(b)) => (Some(Color::Rgb(r as u8, g as u8, b as u8)), 5),
            _ => (None, params.len() - i),
        },
        _ => (None, 1),
    }
}

impl Pen {
    /// Apply one SGR sequence (`ESC [ … m`).
    pub fn apply_sgr(&mut self, code: &str) {
        let body = code.trim_start_matches("\x1b[").trim_end_matches('m');
        let params: Vec<Vec<u16>> = if body.is_empty() {
            vec![vec![0]]
        } else {
            body.split(';')
                .map(|p| p.split(':').map(|n| n.parse().unwrap_or(0)).collect())
                .collect()
        };
        let mut i = 0;
        while i < params.len() {
            let n = params[i].first().copied().unwrap_or(0);
            let mut used = 1;
            match n {
                0 => {
                    let link = self.link.take();
                    *self = Pen { link, ..Pen::default() };
                }
                1 => self.modifier.insert(Modifier::BOLD),
                2 => self.modifier.insert(Modifier::DIM),
                3 => self.modifier.insert(Modifier::ITALIC),
                4 => {
                    // 4:0 is "no underline"
                    if params[i].get(1) == Some(&0) {
                        self.modifier.remove(Modifier::UNDERLINED);
                    } else {
                        self.modifier.insert(Modifier::UNDERLINED);
                    }
                }
                5 => self.modifier.insert(Modifier::SLOW_BLINK),
                6 => self.modifier.insert(Modifier::RAPID_BLINK),
                7 => self.modifier.insert(Modifier::REVERSED),
                8 => self.modifier.insert(Modifier::HIDDEN),
                9 => self.modifier.insert(Modifier::CROSSED_OUT),
                21 => self.modifier.insert(Modifier::UNDERLINED),
                22 => self.modifier.remove(Modifier::BOLD | Modifier::DIM),
                23 => self.modifier.remove(Modifier::ITALIC),
                24 => self.modifier.remove(Modifier::UNDERLINED),
                25 => self.modifier.remove(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK),
                27 => self.modifier.remove(Modifier::REVERSED),
                28 => self.modifier.remove(Modifier::HIDDEN),
                29 => self.modifier.remove(Modifier::CROSSED_OUT),
                30..=37 => self.fg = basic(n - 30),
                39 => self.fg = Color::Reset,
                40..=47 => self.bg = basic(n - 40),
                49 => self.bg = Color::Reset,
                90..=97 => self.fg = bright(n - 90),
                100..=107 => self.bg = bright(n - 100),
                38 | 48 | 58 => {
                    let (color, k) = extended(&params, i);
                    used = k;
                    if let Some(c) = color {
                        match n {
                            38 => self.fg = c,
                            48 => self.bg = c,
                            _ => self.underline = c,
                        }
                    }
                }
                59 => self.underline = Color::Reset,
                _ => {}
            }
            i += used.max(1);
        }
    }

    fn style(&self, cell: &mut Cell) {
        cell.fg = self.fg;
        cell.bg = self.bg;
        cell.underline_color = self.underline;
        cell.modifier = self.modifier;
    }
}

fn forced(width: usize) -> CellDiffOption {
    CellDiffOption::ForcedWidth(NonZeroU16::new(width.clamp(1, u16::MAX as usize) as u16).unwrap_or(NonZeroU16::MIN))
}

/// The width a painted cell covers (1 for an unpainted one).
pub fn cell_width(cell: &Cell) -> usize {
    match cell.diff_option {
        CellDiffOption::ForcedWidth(w) => w.get() as usize,
        _ => 1,
    }
}

/// True for the columns a wide cell to their left covers.
pub fn is_covered(cell: &Cell) -> bool {
    cell.diff_option == CellDiffOption::Skip
}

/// A blank at `(x, y)`, keeping nothing of what was there.
fn blank(buf: &mut Buffer, x: u16, y: u16) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.reset();
    }
}

/// Before a write over `[x, end)`: a wide cell cut by either edge becomes blanks, so no half of a
/// wide glyph stays on screen.
fn split_wide_edges(buf: &mut Buffer, x: u16, end: u16, y: u16) {
    let width = buf.area.width;
    // A wide cell starting left of x and reaching into it.
    if x < width && buf.cell((x, y)).is_some_and(is_covered) {
        let mut lead = x;
        while lead > 0 && buf.cell((lead, y)).is_some_and(is_covered) {
            lead -= 1;
        }
        for c in lead..x {
            blank(buf, c, y);
        }
    }
    // A wide cell inside the range reaching past its end.
    if end < width && buf.cell((end, y)).is_some_and(is_covered) {
        let mut c = end;
        while c < width && buf.cell((c, y)).is_some_and(is_covered) {
            blank(buf, c, y);
            c += 1;
        }
    }
}

fn wrap_link(symbol: &str, link: &Option<String>) -> String {
    match link {
        Some(url) => format!("\x1b]8;;{url}\x1b\\{symbol}\x1b]8;;\x1b\\"),
        None => symbol.to_string(),
    }
}

/// Paint `line` into row `y` from column `x`, at most `max` columns (and never past the buffer's
/// edge). A grapheme that does not fit whole is left out. Returns the columns painted.
pub fn paint_line(buf: &mut Buffer, x: u16, y: u16, max: usize, line: &str) -> usize {
    if y >= buf.area.height || x >= buf.area.width {
        return 0;
    }
    let limit = (buf.area.width - x) as usize;
    let max = max.min(limit);
    let mut pen = Pen::default();
    let mut col = 0usize;
    let mut last: Option<u16> = None;
    let mut painted_any = false;
    for piece in pieces(line) {
        match piece {
            Piece::Ansi(code) => {
                if code.starts_with("\x1b[") && code.ends_with('m') {
                    pen.apply_sgr(code);
                } else if let Some(link) = parse_osc8(code) {
                    pen.link = link.map(|h| h.url);
                }
            }
            Piece::Text(text) => {
                for g in graphemes(text) {
                    let w = if g == "\t" { 3 } else { grapheme_width(g) };
                    if w == 0 {
                        // Joins the cell before it, unless it is a control (never written).
                        if g.chars()
                            .all(|c| (c as u32) < 0x20 || c == '\x7f' || ('\u{80}'..='\u{9f}').contains(&c))
                        {
                            continue;
                        }
                        if let Some(lx) = last
                            && let Some(cell) = buf.cell_mut((lx, y))
                        {
                            let joined = format!("{}{g}", cell.symbol());
                            cell.set_symbol(&joined);
                        }
                        continue;
                    }
                    if col + w > max {
                        return col;
                    }
                    if !painted_any {
                        painted_any = true;
                    }
                    let cx = x + col as u16;
                    split_wide_edges(buf, cx, cx + w as u16, y);
                    if g == "\t" {
                        for k in 0..3u16 {
                            if let Some(cell) = buf.cell_mut((cx + k, y)) {
                                cell.reset();
                                cell.set_symbol(&wrap_link(" ", &pen.link));
                                pen.style(cell);
                                cell.set_diff_option(forced(1));
                            }
                        }
                        last = Some(cx + 2);
                    } else {
                        if let Some(cell) = buf.cell_mut((cx, y)) {
                            cell.reset();
                            cell.set_symbol(&wrap_link(g, &pen.link));
                            pen.style(cell);
                            cell.set_diff_option(forced(w));
                        }
                        for k in 1..w as u16 {
                            if let Some(cell) = buf.cell_mut((cx + k, y)) {
                                cell.reset();
                                cell.bg = pen.bg;
                                cell.set_diff_option(CellDiffOption::Skip);
                            }
                        }
                        last = Some(cx);
                    }
                    col += w;
                }
            }
        }
    }
    col
}

/// The text of row `y`, links and styles left out (tests, selection, search).
pub fn row_text(buf: &Buffer, y: u16) -> String {
    let mut out = String::new();
    for x in 0..buf.area.width {
        let Some(cell) = buf.cell((x, y)) else { break };
        if is_covered(cell) {
            continue;
        }
        let s = cell.symbol();
        match s.strip_prefix("\x1b]8;;") {
            Some(rest) => {
                let inner = rest.split_once("\x1b\\").map(|(_, r)| r).unwrap_or(rest);
                out.push_str(inner.strip_suffix("\x1b]8;;\x1b\\").unwrap_or(inner));
            }
            None => out.push_str(s),
        }
    }
    out
}

/// Restyle the cells of row `y` in `[from, to)`: `f` gets each painted cell.
pub fn restyle(buf: &mut Buffer, y: u16, from: u16, to: u16, mut f: impl FnMut(&mut Cell)) {
    let to = to.min(buf.area.width);
    for x in from..to {
        if let Some(cell) = buf.cell_mut((x, y))
            && !is_covered(cell)
        {
            f(cell);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::{Backend, CrosstermBackend};
    use ratatui::layout::Rect;

    fn draw(prev: &Buffer, next: &Buffer) -> String {
        let mut out: Vec<u8> = Vec::new();
        {
            let mut backend = CrosstermBackend::new(&mut out);
            backend.draw(prev.diff(next).into_iter()).unwrap();
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn styles_and_widths_become_cells() {
        let _g = crate::testing::global_lock();
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        let n = paint_line(&mut buf, 0, 0, 10, "\x1b[31m日本\x1b[0mab");
        assert_eq!(n, 6);
        assert_eq!(row_text(&buf, 0), "日本ab    ");
        assert_eq!(buf[(0, 0)].fg, Color::Red);
        assert_eq!(buf[(4, 0)].fg, Color::Reset);
        assert!(is_covered(&buf[(1, 0)]));
    }

    #[test]
    fn a_wide_glyph_that_does_not_fit_is_left_out() {
        let _g = crate::testing::global_lock();
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        assert_eq!(paint_line(&mut buf, 0, 0, 3, "a日本"), 3);
        assert_eq!(row_text(&buf, 0), "a日");
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        assert_eq!(paint_line(&mut buf, 0, 0, 2, "a日"), 1);
    }

    #[test]
    fn overlays_split_wide_cells_cleanly() {
        let _g = crate::testing::global_lock();
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 1));
        paint_line(&mut buf, 0, 0, 6, "日本語");
        paint_line(&mut buf, 1, 0, 2, "xy");
        assert_eq!(row_text(&buf, 0), " xy 語");
    }

    #[test]
    fn links_wrap_each_cell_and_the_diff_moves_by_forced_widths() {
        let _g = crate::testing::global_lock();
        let empty = Buffer::empty(Rect::new(0, 0, 6, 1));
        let mut buf = empty.clone();
        paint_line(&mut buf, 0, 0, 6, "\x1b]8;;https://x.test\x07ab\x1b]8;;\x07①c");
        let out = draw(&empty, &buf);
        assert!(out.contains("\x1b]8;;https://x.test\x1b\\a\x1b]8;;\x1b\\"));
        // "①" is one column here (ambiguous width 1): no move between it and "c".
        assert!(out.contains("①c"));
        assert_eq!(row_text(&buf, 0), "ab①c  ");
    }

    #[test]
    fn sgr_parameters_map_to_ratatui_styles() {
        let mut pen = Pen::default();
        pen.apply_sgr("\x1b[1;4;38;5;208;48;2;1;2;3m");
        assert!(pen.modifier.contains(Modifier::BOLD | Modifier::UNDERLINED));
        assert_eq!(pen.fg, Color::Indexed(208));
        assert_eq!(pen.bg, Color::Rgb(1, 2, 3));
        pen.apply_sgr("\x1b[22;24;39m");
        assert_eq!(pen.modifier, Modifier::empty());
        assert_eq!(pen.fg, Color::Reset);
        pen.apply_sgr("\x1b[90;107m");
        assert_eq!((pen.fg, pen.bg), (Color::DarkGray, Color::White));
        pen.apply_sgr("\x1b[m");
        assert_eq!(pen, Pen::default());
    }
}
