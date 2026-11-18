//! The full-screen renderer (pi-tui `TuiAltScreen`): the transcript in a scroll view that follows
//! the end, the dock (status, input, footer) under it, a conversation search box, a scrollbar while
//! scrolling, transient flashes and a mouse selection that is copied on release.
//!
//! The frame is composed from ANSI lines as pi-tui does (`renderLayoutFrame`: a vertical stack, a
//! scroll view, the dock's parts shrinking to fit), then painted into a ratatui [`Buffer`]
//! ([`super::cells`]); ratatui's diff and crossterm backend write only the cells that changed,
//! inside one synchronized-output block. pi-tui diffed whole rows instead; the screen is the same.

use std::time::{Duration, Instant};

use crossterm::event::{KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use regex::RegexBuilder;

use super::cells::{self, paint_line, restyle};
use crate::keys::{Keybindings, is_key_release};
use crate::text::{
    CURSOR_MARKER, grapheme_width, graphemes, slice_by_column, strip_terminal_sequences, truncate_to_width,
    visible_width,
};
use crate::ui::input::{Input, InputAction};

/// Mouse reporting off: SGR coordinates, focus, any motion, drag, buttons.
pub const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1004l\x1b[?1003l\x1b[?1002l\x1b[?1000l";
const BUTTON_MOTION_MOUSE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1004h\x1b[?1006h";
const ALL_MOTION_MOUSE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1004h\x1b[?1006h";
const BEGIN_SYNC: &str = "\x1b[?2026h";
const END_SYNC: &str = "\x1b[?2026l";
const PAGE_SCROLL_OVERLAP: usize = 4;
const ALT_WHEEL_MULTIPLIER: usize = 5;
const SCROLLBAR_HIDE: Duration = Duration::from_millis(1000);
const FLASH_DURATION: Duration = Duration::from_millis(1000);
const COPY_ERROR_FLASH: Duration = Duration::from_millis(5000);

/// Multiplexers lag when every pointer movement is forwarded: button-motion tracking there.
pub fn mouse_on_sequence() -> &'static str {
    let term = std::env::var("TERM").unwrap_or_default().to_lowercase();
    let mux = ["TMUX", "ZELLIJ", "STY"].iter().any(|v| std::env::var_os(v).is_some())
        || term.starts_with("tmux")
        || term.starts_with("screen");
    if mux { BUTTON_MOTION_MOUSE } else { ALL_MOTION_MOUSE }
}

/// What entering writes, before the terminal's own start (the kitty keyboard flags are pushed
/// inside the alternate screen, which keeps its own flag stack).
pub fn enter_sequence() -> String {
    format!("\x1b[?1049h\x1b[?7l{}\x1b[2J\x1b[H\x1b[?25l", mouse_on_sequence())
}

/// What leaving writes before the terminal's own stop.
pub fn before_stop_sequence() -> String {
    format!("{BEGIN_SYNC}{MOUSE_OFF}\x1b[?7h{END_SYNC}")
}

/// What leaving writes after the terminal's own stop: back to the normal screen as it was (the
/// transcript is not copied onto it).
pub fn after_stop_sequence() -> String {
    format!("{BEGIN_SYNC}\x1b[?1049l\x1b[?25h{END_SYNC}")
}

// ── stack layout (pi-tui stack.js) ───────────────────────────────────

/// One child of a vertical stack.
#[derive(Debug, Clone, Copy)]
pub struct StackEntry {
    /// `None`: its rendered height.
    pub basis: Option<usize>,
    pub grow: usize,
    pub shrink: usize,
    pub min: usize,
    pub max: usize,
}

impl Default for StackEntry {
    fn default() -> Self {
        Self {
            basis: None,
            grow: 0,
            shrink: 1,
            min: 0,
            max: usize::MAX,
        }
    }
}

fn distribute(sizes: &mut [usize], entries: &[StackEntry], amount: usize, grow: bool) {
    let mut remaining = amount;
    while remaining > 0 {
        let candidates: Vec<usize> = (0..entries.len())
            .filter(|&i| {
                if grow {
                    entries[i].grow > 0 && sizes[i] < entries[i].max
                } else {
                    entries[i].shrink > 0 && sizes[i] > entries[i].min
                }
            })
            .collect();
        if candidates.is_empty() {
            return;
        }
        let weight = |i: usize, sizes: &[usize]| -> usize {
            if grow {
                entries[i].grow
            } else {
                entries[i].shrink * sizes[i].max(1)
            }
        };
        let total: usize = candidates.iter().map(|&i| weight(i, sizes)).sum();
        let mut distributed = 0;
        for &i in &candidates {
            if remaining == 0 {
                break;
            }
            let proposed = ((remaining * weight(i, sizes)) / total.max(1)).max(1);
            let capacity = if grow {
                entries[i].max - sizes[i]
            } else {
                sizes[i] - entries[i].min
            };
            let delta = remaining.min(proposed).min(capacity);
            if delta == 0 {
                continue;
            }
            if grow {
                sizes[i] += delta;
            } else {
                sizes[i] -= delta;
            }
            remaining -= delta;
            distributed += delta;
        }
        if distributed == 0 {
            return;
        }
    }
}

/// The rows each child gets out of `available` (`None`: as many as they ask for).
pub fn allocate_stack_sizes(entries: &[StackEntry], intrinsic: &[usize], available: Option<usize>) -> Vec<usize> {
    let mut sizes: Vec<usize> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let size = e.basis.unwrap_or(intrinsic.get(i).copied().unwrap_or(0));
            size.clamp(e.min, e.max.max(e.min))
        })
        .collect();
    let Some(available) = available else { return sizes };
    let total: usize = sizes.iter().sum();
    if total < available {
        distribute(&mut sizes, entries, available - total, true);
    } else if total > available {
        distribute(&mut sizes, entries, total - available, false);
    }
    sizes
}

// ── scroll view (pi-tui scroll-view.js, vertical, follow "end", scrollbar "auto") ──

#[derive(Debug, Clone)]
pub struct ScrollState {
    top: usize,
    content: usize,
    viewport: usize,
    following_end: bool,
    suppressed_at_end: bool,
    scrollbar_until: Option<Instant>,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            top: 0,
            content: 0,
            viewport: 0,
            following_end: true,
            suppressed_at_end: false,
            scrollbar_until: None,
        }
    }
}

impl ScrollState {
    pub fn top(&self) -> usize {
        self.top
    }
    pub fn viewport(&self) -> usize {
        self.viewport
    }
    pub fn is_following_end(&self) -> bool {
        self.following_end
    }
    fn max_top(&self) -> usize {
        self.content.saturating_sub(self.viewport)
    }
    fn mark_activity(&mut self) {
        if self.content > self.viewport {
            self.scrollbar_until = Some(Instant::now() + SCROLLBAR_HIDE);
        }
    }
    pub fn scrollbar_visible(&self, now: Instant) -> bool {
        self.content > self.viewport && self.scrollbar_until.is_some_and(|t| now < t)
    }

    pub fn scroll_to(&mut self, top: usize, disable_follow: bool) {
        let max = self.max_top();
        let next = top.min(max);
        let suppressed = disable_follow && next == max;
        let following = !suppressed && next == max;
        if next != self.top {
            self.mark_activity();
        }
        self.top = next;
        self.following_end = following;
        self.suppressed_at_end = suppressed;
    }

    /// Scroll by `lines`; what could not be scrolled is returned.
    pub fn scroll_by(&mut self, lines: i64) -> i64 {
        if lines == 0 {
            return 0;
        }
        let max = self.max_top() as i64;
        let start = if self.following_end { max } else { self.top as i64 };
        let next = (start + lines).clamp(0, max);
        let moved = next - start;
        self.top = next as usize;
        self.following_end = next == max;
        self.suppressed_at_end = false;
        if moved != 0 {
            self.mark_activity();
        }
        lines - moved
    }

    pub fn scroll_to_start(&mut self) {
        let moved = self.top != 0;
        self.top = 0;
        self.following_end = self.content <= self.viewport;
        self.suppressed_at_end = false;
        if moved {
            self.mark_activity();
        }
    }

    pub fn scroll_to_end(&mut self) {
        let next = self.max_top();
        let moved = self.top != next;
        self.top = next;
        self.following_end = true;
        self.suppressed_at_end = false;
        if moved {
            self.mark_activity();
        }
    }

    pub fn update_layout(&mut self, content: usize, viewport: usize) {
        self.content = content;
        self.viewport = viewport;
        let max = self.max_top();
        if self.following_end {
            self.top = max;
        } else {
            self.top = self.top.min(max);
        }
        if self.top < max {
            self.suppressed_at_end = false;
        }
        if self.top == max && !self.suppressed_at_end {
            self.following_end = true;
        }
        if self.content <= self.viewport {
            self.scrollbar_until = None;
        }
    }

    /// (thumb top, thumb height) within the track, which is the viewport.
    pub fn scrollbar_geometry(&self) -> (usize, usize) {
        let track = self.viewport;
        let content = self.content.max(1);
        let thumb = ((track * track) as f64 / content as f64 + 0.5).floor() as usize;
        let thumb = thumb.min(track).max(2.min(track));
        let max_top = self.max_top();
        let max_thumb_top = track - thumb;
        let offset = if max_top == 0 {
            0
        } else {
            ((self.top as f64 / max_top as f64) * max_thumb_top as f64 + 0.5).floor() as usize
        };
        (offset, thumb)
    }
}

// ── conversation search (pi-tui alt-screen-search.js) ────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub row: usize,
    pub start_col: usize,
    pub end_col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    pub segments: Vec<Segment>,
}

impl SearchMatch {
    fn key(&self) -> String {
        match (self.segments.first(), self.segments.last()) {
            (Some(f), Some(l)) => format!("{}:{}:{}:{}", f.row, f.start_col, l.row, l.end_col),
            _ => String::new(),
        }
    }
}

struct Span {
    text_start: usize,
    text_end: usize,
    row: usize,
    start_col: usize,
    end_col: usize,
    linear: bool,
}

struct Corpus {
    text: String,
    spans: Vec<Span>,
}

/// The transcript as one string: words of every line, joined by single spaces, each mapped back to
/// its row and columns.
fn build_corpus(lines: &[String]) -> Corpus {
    let mut text = String::new();
    let mut spans = Vec::new();
    let mut pending = false;
    for (row, raw) in lines.iter().enumerate() {
        let line = strip_terminal_sequences(raw);
        let mut column = 0;
        if line.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
            for (i, word) in line.split(' ').enumerate() {
                if i > 0 {
                    if !text.is_empty() {
                        pending = true;
                    }
                    column += 1;
                }
                if word.is_empty() {
                    continue;
                }
                if pending {
                    text.push(' ');
                    pending = false;
                }
                spans.push(Span {
                    text_start: text.len(),
                    text_end: text.len() + word.len(),
                    row,
                    start_col: column,
                    end_col: column + word.len(),
                    linear: true,
                });
                text.push_str(word);
                column += word.len();
            }
        } else {
            for g in graphemes(&line) {
                let w = grapheme_width(g);
                if g.chars().all(char::is_whitespace) {
                    if !text.is_empty() {
                        pending = true;
                    }
                    column += w;
                    continue;
                }
                if pending {
                    text.push(' ');
                    pending = false;
                }
                spans.push(Span {
                    text_start: text.len(),
                    text_end: text.len() + g.len(),
                    row,
                    start_col: column,
                    end_col: column + w,
                    linear: false,
                });
                text.push_str(g);
                column += w;
            }
        }
        if !text.is_empty() {
            pending = true;
        }
    }
    Corpus { text, spans }
}

fn normalize_query(q: &str) -> String {
    q.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn find_matches(corpus: &Corpus, query: &str) -> Vec<SearchMatch> {
    if query.is_empty() {
        return Vec::new();
    }
    let Ok(re) = RegexBuilder::new(&regex::escape(query)).case_insensitive(true).build() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut si = 0;
    for m in re.find_iter(&corpus.text) {
        let (start, end) = (m.start(), m.end());
        while si < corpus.spans.len() && corpus.spans[si].text_end <= start {
            si += 1;
        }
        let mut segments: Vec<Segment> = Vec::new();
        for span in &corpus.spans[si..] {
            if span.text_start >= end {
                break;
            }
            if span.text_end <= start {
                continue;
            }
            let start_col = if span.linear {
                span.start_col + start.max(span.text_start) - span.text_start
            } else {
                span.start_col
            };
            let end_col = if span.linear {
                span.start_col + end.min(span.text_end) - span.text_start
            } else {
                span.end_col
            };
            match segments.last_mut() {
                Some(prev) if prev.row == span.row && start_col <= prev.end_col => {
                    prev.end_col = prev.end_col.max(end_col);
                }
                _ => segments.push(Segment {
                    row: span.row,
                    start_col,
                    end_col,
                }),
            }
        }
        while si < corpus.spans.len() && corpus.spans[si].text_end <= end {
            si += 1;
        }
        if !segments.is_empty() {
            out.push(SearchMatch { segments });
        }
    }
    out
}

/// Every match of `query` in `lines` (case-insensitive, whitespace-insensitive across lines).
pub fn find_search_matches(lines: &[String], query: &str) -> Vec<SearchMatch> {
    let q = normalize_query(query);
    if q.is_empty() {
        return Vec::new();
    }
    find_matches(&build_corpus(lines), &q)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionMode {
    Query,
    Next,
    Previous,
    Retain,
}

struct Search {
    input: Input,
    query: String,
    matches: Vec<SearchMatch>,
    selected: Option<usize>,
    selected_key: Option<String>,
    anchor_row: usize,
    mode: SelectionMode,
    source: Option<Vec<String>>,
    corpus: Option<Corpus>,
    normalized: Option<String>,
    no_matches: String,
}

fn format_key(key: Option<&String>) -> String {
    let Some(key) = key else { return "Unbound".into() };
    key.split('+')
        .map(|part| {
            if cfg!(target_os = "macos") && part.eq_ignore_ascii_case("alt") {
                return "Option".to_string();
            }
            let mut cs = part.chars();
            match cs.next() {
                Some(f) => f.to_uppercase().chain(cs).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

impl Search {
    fn new(anchor_row: usize, placeholder: &str, no_matches: &str) -> Self {
        let mut input = Input::new(" ", placeholder, crate::theme::dim);
        input.focused = true;
        Self {
            input,
            query: String::new(),
            matches: Vec::new(),
            selected: None,
            selected_key: None,
            anchor_row,
            mode: SelectionMode::Query,
            source: None,
            corpus: None,
            normalized: None,
            no_matches: no_matches.to_string(),
        }
    }

    /// (matches changed?) after looking at `lines` again.
    fn search(&mut self, lines: &[String]) -> bool {
        let source_changed = self.source.as_deref() != Some(lines);
        if source_changed || self.corpus.is_none() {
            self.source = Some(lines.to_vec());
            self.corpus = Some(build_corpus(lines));
        }
        let q = normalize_query(&self.query);
        let changed = source_changed || self.normalized.as_deref() != Some(q.as_str());
        if changed {
            self.matches = self.corpus.as_ref().map(|c| find_matches(c, &q)).unwrap_or_default();
            self.normalized = Some(q);
        }
        changed
    }

    fn set_query(&mut self, query: String, scroll_top: usize) {
        if query == self.query {
            return;
        }
        let selected_row = self
            .selected
            .and_then(|i| self.matches.get(i))
            .and_then(|m| m.segments.first())
            .map(|s| s.row);
        self.anchor_row = selected_row.unwrap_or(scroll_top);
        self.query = query;
        self.mode = SelectionMode::Query;
        self.selected = None;
    }

    fn render(&mut self, width: usize, kb: &Keybindings) -> Vec<String> {
        let width = width.max(1);
        if width == 1 {
            return vec!["┌".into(), "│".into(), "└".into()];
        }
        let inner = width.saturating_sub(2);
        let previous_key = format_key(kb.keys("tui.altScreen.searchPrevious").first());
        let next_key = format_key(kb.keys("tui.altScreen.searchNext").first());
        let result = if self.input.value().is_empty() {
            String::new()
        } else if self.matches.is_empty() {
            self.no_matches.clone()
        } else {
            format!("{}/{}", self.selected.map(|i| i + 1).unwrap_or(0), self.matches.len())
        };
        let visible_result = truncate_to_width(&result, inner.saturating_sub(3), "", false);
        let result_text = if visible_result.is_empty() {
            String::new()
        } else {
            format!("\x1b[2m {visible_result} \x1b[22m")
        };
        let input_width = inner.saturating_sub(visible_width(&result_text));
        let input_line = truncate_to_width(
            self.input
                .render(input_width.max(1))
                .first()
                .map(String::as_str)
                .unwrap_or(""),
            input_width,
            "",
            false,
        );
        let padding = " ".repeat(input_width.saturating_sub(visible_width(&input_line)));
        let content = format!("{input_line}{padding}{result_text}");
        let mut previous = format!("↑ {previous_key}");
        let mut next = format!("↓ {next_key}");
        let mut separator = " · ";
        let available = inner.saturating_sub(3);
        let mut controls = visible_width(&previous) + visible_width(separator) + visible_width(&next);
        if controls > available {
            previous = "↑".into();
            next = "↓".into();
            separator = " ";
            controls = 3;
        }
        let show = controls <= available;
        let buttons = if show {
            format!("{previous}{separator}{next}")
        } else {
            String::new()
        };
        let gaps = if show { 2 } else { 0 };
        let right_rule = usize::from(!buttons.is_empty() && inner > controls + gaps);
        let left_rule = inner.saturating_sub(if show { controls } else { 0 } + gaps + right_rule);
        let gap = if buttons.is_empty() { "" } else { " " };
        vec![
            format!("┌{}┐", "─".repeat(inner)),
            format!("│{content}│"),
            format!(
                "└{}{gap}{buttons}{gap}{}┘",
                "─".repeat(left_rule),
                "─".repeat(right_rule)
            ),
        ]
    }
}

// ── the screen ───────────────────────────────────────────────────────

/// A part of the dock under the transcript, with the fewest rows it keeps when space runs out.
#[derive(Debug, Clone, Default)]
pub struct DockPart {
    pub lines: Vec<String>,
    pub min: usize,
}

/// What a mouse event came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseOutcome {
    None,
    /// Something changed on screen.
    Render,
    /// A selection was made: copy this text (then [`AltScreen::copied`]).
    Copy(String),
    /// A click on a dock part: its index, the row within its rendered lines, the column.
    DockClick {
        part: usize,
        row: usize,
        col: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Point {
    row: usize,
    col: usize,
}

#[derive(Debug, Clone)]
struct Selection {
    anchor: Point,
    focus: Point,
    pressing: bool,
    dragged: bool,
}

struct Flash {
    message: String,
    until: Instant,
}

/// Where each dock part was drawn in the last frame.
#[derive(Debug, Clone, Copy, Default)]
struct PartBox {
    y: usize,
    height: usize,
    offset: usize,
}

pub struct AltScreen {
    prev: Option<Buffer>,
    full: bool,
    /// Frames drawn whole (after a clear) so far.
    pub full_redraws: usize,
    scroll: ScrollState,
    lines: Vec<String>,
    search: Option<Search>,
    flashes: Vec<Flash>,
    selection: Option<Selection>,
    indicator: Option<(usize, usize, usize)>,
    parts: Vec<PartBox>,
    width: usize,
    /// Rows the wheel scrolls per notch.
    pub wheel_lines: usize,
    /// The scroll-to-end label (shown inverted while scrolled up).
    pub indicator_label: String,
    pub search_placeholder: String,
    pub search_no_matches: String,
}

impl Default for AltScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn strip_osc133_prefix(line: &str) -> &str {
    let mut rest = line;
    loop {
        let Some(r) = rest.strip_prefix("\x1b]133;") else {
            return rest;
        };
        let Some(kind) = r.chars().next() else { return rest };
        if !matches!(kind, 'A' | 'B' | 'C') {
            return rest;
        }
        let r = &r[1..];
        if let Some(r) = r.strip_prefix('\x07') {
            rest = r;
        } else if let Some(r) = r.strip_prefix("\x1b\\") {
            rest = r;
        } else {
            return rest;
        }
    }
}

fn is_prompt_start(line: &str) -> bool {
    line.starts_with("\x1b]133;A\x07") || line.starts_with("\x1b]133;A\x1b\\")
}

impl AltScreen {
    pub fn new() -> Self {
        Self {
            prev: None,
            full: true,
            full_redraws: 0,
            scroll: ScrollState::default(),
            lines: Vec::new(),
            search: None,
            flashes: Vec::new(),
            selection: None,
            indicator: None,
            parts: Vec::new(),
            width: 0,
            wheel_lines: 3,
            indicator_label: " ↓ ctrl+end ".into(),
            search_placeholder: "Find in transcript".into(),
            search_no_matches: "No matches".into(),
        }
    }

    pub fn scroll(&self) -> &ScrollState {
        &self.scroll
    }

    /// The next frame is drawn whole, after clearing the screen.
    pub fn force_full(&mut self) {
        self.full = true;
    }

    pub fn search_open(&self) -> bool {
        self.search.is_some()
    }

    pub fn scroll_by(&mut self, lines: i64) {
        self.scroll.scroll_by(lines);
    }
    pub fn scroll_to_top(&mut self) {
        self.scroll.scroll_to_start();
    }
    pub fn scroll_to_bottom(&mut self) {
        self.scroll.scroll_to_end();
    }

    /// Jump to the previous (-1) or next (1) user prompt (an OSC 133 prompt mark).
    pub fn scroll_to_prompt(&mut self, direction: i64) {
        let mut row = self.scroll.top as i64 + direction;
        while row >= 0 && (row as usize) < self.lines.len() {
            if is_prompt_start(&self.lines[row as usize]) {
                self.scroll.scroll_to(row as usize, false);
                return;
            }
            row += direction;
        }
    }

    pub fn toggle_search(&mut self) {
        if self.search.is_some() {
            self.search = None;
            return;
        }
        self.search = Some(Search::new(
            self.scroll.top,
            &self.search_placeholder,
            &self.search_no_matches,
        ));
    }

    pub fn close_search(&mut self) {
        self.search = None;
    }

    fn navigate_search(&mut self, direction: i64) {
        if let Some(s) = self.search.as_mut()
            && !s.query.is_empty()
        {
            s.mode = if direction < 0 {
                SelectionMode::Previous
            } else {
                SelectionMode::Next
            };
        }
    }

    /// A transient message at the top right.
    pub fn flash(&mut self, message: &str, duration: Option<Duration>) {
        self.flashes.push(Flash {
            message: message.to_string(),
            until: Instant::now() + duration.unwrap_or(FLASH_DURATION),
        });
    }

    /// How a copy of the selection went (`None`: it failed, with this message if any).
    pub fn copied(&mut self, ok: bool, message: Option<&str>) {
        if ok {
            self.flash("Copied!", None);
        } else {
            self.flash(message.unwrap_or("Copy failed"), Some(COPY_ERROR_FLASH));
        }
    }

    /// When a timer (scrollbar hide, flash end) needs a new frame.
    pub fn next_wakeup(&self) -> Option<Instant> {
        let flash = self.flashes.iter().map(|f| f.until).min();
        let bar = self.scroll.scrollbar_until;
        match (flash, bar) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The viewport's own keys (they run before the app's). True when the key was used.
    pub fn handle_key(&mut self, ev: &KeyEvent, kb: &Keybindings) -> bool {
        if is_key_release(ev) {
            return false;
        }
        if kb.matches(ev, "tui.altScreen.search") {
            self.toggle_search();
            return true;
        }
        if self.search.is_some() {
            if kb.matches(ev, "tui.altScreen.searchNext") {
                self.navigate_search(1);
                return true;
            }
            if kb.matches(ev, "tui.altScreen.searchPrevious") {
                self.navigate_search(-1);
                return true;
            }
            if kb.matches(ev, "tui.altScreen.searchClose") {
                self.close_search();
                return true;
            }
        }
        let page = self.scroll.viewport.saturating_sub(PAGE_SCROLL_OVERLAP).max(1) as i64;
        let half = (self.scroll.viewport / 2).max(1) as i64;
        if kb.matches(ev, "tui.altScreen.pageUp") {
            self.scroll.scroll_by(-page);
        } else if kb.matches(ev, "tui.altScreen.pageDown") {
            self.scroll.scroll_by(page);
        } else if kb.matches(ev, "tui.altScreen.halfPageUp") {
            self.scroll.scroll_by(-half);
        } else if kb.matches(ev, "tui.altScreen.halfPageDown") {
            self.scroll.scroll_by(half);
        } else if kb.matches(ev, "tui.altScreen.lineUp") {
            self.scroll.scroll_by(-1);
        } else if kb.matches(ev, "tui.altScreen.lineDown") {
            self.scroll.scroll_by(1);
        } else if kb.matches(ev, "tui.altScreen.previousPrompt") {
            self.scroll_to_prompt(-1);
        } else if kb.matches(ev, "tui.altScreen.nextPrompt") {
            self.scroll_to_prompt(1);
        } else if kb.matches(ev, "tui.altScreen.top") {
            self.scroll.scroll_to_start();
        } else if kb.matches(ev, "tui.altScreen.bottom") {
            self.scroll.scroll_to_end();
        } else {
            return false;
        }
        true
    }

    /// A key for the open search box (after the app had its turn). True when it was taken.
    pub fn search_key(&mut self, ev: &KeyEvent, kb: &Keybindings) -> bool {
        let top = self.scroll.top;
        let Some(s) = self.search.as_mut() else { return false };
        let before = s.input.value().to_string();
        if let InputAction::Escape = s.input.handle_key(ev, kb) {
            self.search = None;
            return true;
        }
        let after = s.input.value().to_string();
        if after != before {
            s.set_query(after, top);
        }
        true
    }

    /// A paste while the search box is open goes to it. True when it was taken.
    pub fn search_paste(&mut self, text: &str) -> bool {
        let top = self.scroll.top;
        let Some(s) = self.search.as_mut() else { return false };
        s.input.handle_paste(text);
        let after = s.input.value().to_string();
        s.set_query(after, top);
        true
    }

    /// The search box's query (tests).
    pub fn search_query(&self) -> Option<&str> {
        self.search.as_ref().map(|s| s.input.value())
    }

    fn selection_bounds(&self) -> Option<(Point, Point)> {
        let s = self.selection.as_ref()?;
        if s.anchor == s.focus {
            return None;
        }
        let before = (s.anchor.row, s.anchor.col) < (s.focus.row, s.focus.col);
        Some(if before {
            (s.anchor, s.focus)
        } else {
            (s.focus, s.anchor)
        })
    }

    fn selection_columns(line: &str, row: usize, start: Point, end: Point) -> (usize, usize) {
        let width = visible_width(line);
        let mut from = 0;
        let mut to = width;
        if row == start.row {
            from = crate::text::get_grapheme_cell_range(line, start.col)
                .map(|(s, _)| s)
                .unwrap_or(start.col.min(width));
        }
        if row == end.row {
            to = crate::text::get_grapheme_cell_range(line, end.col)
                .map(|(_, e)| e)
                .unwrap_or((end.col + 1).min(width));
        }
        (from, to.max(from))
    }

    /// The selected text, styles left out, each line's trailing blanks trimmed.
    pub fn selection_text(&self) -> Option<String> {
        let (start, end) = self.selection_bounds()?;
        let mut out = Vec::new();
        for row in start.row..=end.row {
            let line = strip_osc133_prefix(self.lines.get(row).map(String::as_str).unwrap_or(""));
            let (from, to) = Self::selection_columns(line, row, start, end);
            let text = strip_terminal_sequences(&slice_by_column(line, from, to - from, true));
            out.push(crate::text::js_trim_end(&text).to_string());
        }
        let text = out.join("\n");
        (!text.is_empty()).then_some(text)
    }

    /// Mouse input: the wheel scrolls, a drag over the transcript selects (copied on release), a
    /// click on the scroll-to-end label follows the end again, a click in the dock is reported.
    pub fn handle_mouse(&mut self, ev: &MouseEvent) -> MouseOutcome {
        let (x, y) = (ev.column as usize, ev.row as usize);
        let viewport = self.scroll.viewport;
        match ev.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let lines = self.wheel_lines
                    * if ev.modifiers.contains(KeyModifiers::ALT) {
                        ALT_WHEEL_MULTIPLIER
                    } else {
                        1
                    };
                let dir = if ev.kind == MouseEventKind::ScrollUp { -1 } else { 1 };
                self.scroll.scroll_by(dir * lines as i64);
                MouseOutcome::Render
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((row, col, w)) = self.indicator
                    && y == row
                    && x >= col
                    && x < col + w
                {
                    self.scroll.scroll_to_end();
                    self.selection = None;
                    return MouseOutcome::Render;
                }
                let had = self.selection.take().is_some();
                if y < viewport && self.search.is_none() {
                    let p = Point {
                        row: self.scroll.top + y,
                        col: x,
                    };
                    self.selection = Some(Selection {
                        anchor: p,
                        focus: p,
                        pressing: true,
                        dragged: false,
                    });
                    return MouseOutcome::Render;
                }
                if let Some((part, b)) = self
                    .parts
                    .iter()
                    .enumerate()
                    .find(|(_, b)| y >= b.y && y < b.y + b.height)
                {
                    return MouseOutcome::DockClick {
                        part,
                        row: y - b.y + b.offset,
                        col: x,
                    };
                }
                if had { MouseOutcome::Render } else { MouseOutcome::None }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if !self.selection.as_ref().is_some_and(|s| s.pressing) {
                    return MouseOutcome::None;
                }
                // Past the top or bottom edge: scroll one line that way.
                if y >= viewport && viewport > 0 {
                    self.scroll.scroll_by(1);
                } else if y == 0 && self.scroll.top > 0 {
                    self.scroll.scroll_by(-1);
                }
                let top = self.scroll.top;
                if let Some(s) = self.selection.as_mut() {
                    s.dragged = true;
                    s.focus = Point {
                        row: top + y.min(viewport.saturating_sub(1)),
                        col: x,
                    };
                }
                MouseOutcome::Render
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let Some(s) = self.selection.as_mut().filter(|s| s.pressing) else {
                    return MouseOutcome::None;
                };
                s.pressing = false;
                if !s.dragged {
                    self.selection = None;
                    return MouseOutcome::Render;
                }
                match self.selection_text() {
                    Some(text) => MouseOutcome::Copy(text),
                    None => MouseOutcome::Render,
                }
            }
            _ => MouseOutcome::None,
        }
    }

    /// Forget the selection (any key does).
    pub fn clear_selection(&mut self) -> bool {
        self.selection.take().is_some()
    }

    fn refresh_search(&mut self) {
        let top = self.scroll.top;
        let viewport = self.scroll.viewport;
        let lines = std::mem::take(&mut self.lines);
        let reveal = {
            let Some(s) = self.search.as_mut() else {
                self.lines = lines;
                return;
            };
            if s.query.trim().is_empty() {
                s.matches.clear();
                s.selected = None;
                s.selected_key = None;
                s.mode = SelectionMode::Retain;
                s.normalized = None;
                self.lines = lines;
                return;
            }
            let reveal = s.mode != SelectionMode::Retain;
            let changed = s.search(&lines);
            if !changed && s.mode == SelectionMode::Retain {
                self.lines = lines;
                return;
            }
            let n = s.matches.len();
            let exact = if changed {
                s.selected_key
                    .as_ref()
                    .and_then(|k| s.matches.iter().position(|m| &m.key() == k))
            } else {
                s.selected
            };
            let selected = if n == 0 {
                None
            } else {
                Some(match s.mode {
                    SelectionMode::Query => {
                        let low = s
                            .matches
                            .partition_point(|m| m.segments.first().map(|g| g.row).unwrap_or(0) < s.anchor_row);
                        if low < n { low } else { 0 }
                    }
                    SelectionMode::Next => match exact.or(s.selected.map(|i| i.min(n - 1))) {
                        Some(b) => (b + 1) % n,
                        None => 0,
                    },
                    SelectionMode::Previous => match exact.or(s.selected.map(|i| i.min(n - 1))) {
                        Some(b) => (b + n - 1) % n,
                        None => n - 1,
                    },
                    SelectionMode::Retain => exact.unwrap_or(s.selected.unwrap_or(0).min(n - 1)),
                })
            };
            s.selected = selected;
            s.selected_key = selected.map(|i| s.matches[i].key());
            s.mode = SelectionMode::Retain;
            if reveal {
                selected.and_then(|i| {
                    let m = &s.matches[i];
                    Some((m.segments.first()?.row, m.segments.last()?.row))
                })
            } else {
                None
            }
        };
        self.lines = lines;
        if let Some((first, last)) = reveal
            && viewport > 0
        {
            let bottom = top + viewport - 1;
            let target = if first < top || last > bottom {
                first.saturating_sub(viewport / 3)
            } else {
                top
            };
            self.scroll.scroll_to(target, true);
        }
    }

    /// The bytes that put this frame on screen: `transcript` (rendered at `width`) in the scroll
    /// view, `dock` under it.
    pub fn render(
        &mut self,
        transcript: Vec<String>,
        dock: &[DockPart],
        width: usize,
        height: usize,
        kb: &Keybindings,
    ) -> String {
        let w = width.clamp(1, u16::MAX as usize);
        let h = height.clamp(1, u16::MAX as usize);
        let now = Instant::now();
        self.flashes.retain(|f| f.until > now);
        if self.width != 0 && self.width != w {
            // Rows wrap differently now: the selection's rows mean something else.
            self.selection = None;
        }
        self.width = w;

        // Layout: the scroll view takes what the dock leaves, keeping a row.
        let dock_entries: Vec<StackEntry> = dock
            .iter()
            .map(|p| StackEntry {
                min: p.min,
                ..StackEntry::default()
            })
            .collect();
        let lens: Vec<usize> = dock.iter().map(|p| p.lines.len()).collect();
        let dock_natural: usize = allocate_stack_sizes(&dock_entries, &lens, None).iter().sum();
        let root = [
            StackEntry {
                basis: Some(0),
                grow: 1,
                shrink: 1,
                min: 1,
                max: usize::MAX,
            },
            StackEntry {
                basis: None,
                grow: 0,
                shrink: 1,
                min: 1,
                max: usize::MAX,
            },
        ];
        let sizes = allocate_stack_sizes(&root, &[0, dock_natural], Some(h));
        let (scroll_h, dock_h) = (sizes[0], sizes[1]);
        let part_sizes = allocate_stack_sizes(&dock_entries, &lens, Some(dock_h));

        self.lines = transcript;
        self.scroll.update_layout(self.lines.len(), scroll_h);
        self.refresh_search();

        let area = Rect::new(0, 0, w as u16, h as u16);
        let mut buf = Buffer::empty(area);
        let top = self.scroll.top;
        for r in 0..scroll_h.min(h) {
            if let Some(line) = self.lines.get(top + r) {
                paint_line(&mut buf, 0, r as u16, w, strip_osc133_prefix(line));
            }
        }

        let mut cursor: Option<(usize, usize)> = None;
        let mut y = scroll_h;
        self.parts.clear();
        for (part, &size) in dock.iter().zip(&part_sizes) {
            let mut offset = 0;
            if part.lines.len() > size
                && size > 0
                && let Some(ci) = part.lines.iter().position(|l| l.contains(CURSOR_MARKER))
                && ci >= size
            {
                offset = ci - size + 1;
            }
            self.parts.push(PartBox {
                y,
                height: size,
                offset,
            });
            for r in 0..size {
                if y + r >= h {
                    break;
                }
                let Some(line) = part.lines.get(offset + r) else {
                    continue;
                };
                let line = strip_osc133_prefix(line);
                if let Some(i) = line.find(CURSOR_MARKER) {
                    cursor = Some((y + r, visible_width(&line[..i])));
                }
                paint_line(&mut buf, 0, (y + r) as u16, w, line);
            }
            y += size;
        }

        // The scrollbar, while scrolling.
        let bar_column = (self.scroll.scrollbar_visible(now) && scroll_h > 0 && w > 0).then_some(w - 1);
        if let Some(col) = bar_column {
            let (thumb_top, thumb_h) = self.scroll.scrollbar_geometry();
            // Under NO_COLOR the track is dim instead of dark gray.
            let plain = crate::theme::no_color();
            for r in 0..scroll_h.min(h) {
                let thumb = r >= thumb_top && r < thumb_top + thumb_h;
                let bg = buf.cell((col as u16, r as u16)).map(|c| c.bg).unwrap_or_default();
                let (symbol, fg) = match (thumb, plain) {
                    (true, false) => ("┃", Color::Gray),
                    (false, false) => ("│", Color::DarkGray),
                    (true, true) => ("┃", Color::Reset),
                    (false, true) => ("│", Color::Reset),
                };
                paint_line(&mut buf, col as u16, r as u16, 1, symbol);
                if let Some(cell) = buf.cell_mut((col as u16, r as u16)) {
                    cell.fg = fg;
                    cell.bg = bg;
                    if plain && !thumb {
                        cell.modifier.insert(Modifier::DIM);
                    }
                }
            }
        }

        // Search matches in view: the current one bold and inverted, the others underlined.
        if let Some(s) = self.search.as_ref()
            && let Some(sel) = s.selected
        {
            let max_col = bar_column.unwrap_or(w).min(w);
            for (i, m) in s.matches.iter().enumerate() {
                for seg in &m.segments {
                    if seg.row < top || seg.row >= top + scroll_h {
                        continue;
                    }
                    let row = (seg.row - top) as u16;
                    let from = seg.start_col.min(max_col) as u16;
                    let to = seg.end_col.min(max_col) as u16;
                    restyle(&mut buf, row, from, to, |c| {
                        if i == sel {
                            c.modifier.insert(Modifier::BOLD | Modifier::REVERSED);
                        } else {
                            c.modifier.insert(Modifier::UNDERLINED);
                        }
                    });
                }
            }
        }

        // The way back to the end, while scrolled up.
        self.indicator = None;
        if !self.scroll.following_end && scroll_h > 0 {
            let row = scroll_h - 1;
            let label = truncate_to_width(&self.indicator_label, w, "", false);
            let label_w = visible_width(&label);
            let col = (w - label_w) / 2;
            let right = bar_column.unwrap_or(w);
            let text = truncate_to_width(&label, right.saturating_sub(col), "", false);
            let tw = visible_width(&text);
            if tw > 0 {
                paint_line(&mut buf, col as u16, row as u16, tw, &crate::theme::inverse(&text));
                self.indicator = Some((row, col, tw));
            }
        }

        // The search box, top right.
        if let Some(s) = self.search.as_mut() {
            let ow = ((w as f64 * 0.4).floor() as usize)
                .max(32)
                .min(w.saturating_sub(2))
                .max(1);
            let col = w.saturating_sub(1 + ow);
            for (i, line) in s.render(ow, kb).iter().enumerate() {
                let row = 1 + i;
                if row >= h {
                    break;
                }
                if let Some(at) = line.find(CURSOR_MARKER) {
                    cursor = Some((row, col + visible_width(&line[..at])));
                }
                paint_line(&mut buf, col as u16, row as u16, ow, line);
            }
        }

        // The mouse selection.
        if let Some((start, end)) = self.selection_bounds() {
            for row in start.row.max(top)..=end.row.min(top + scroll_h.saturating_sub(1)) {
                if scroll_h == 0 {
                    break;
                }
                let line = strip_osc133_prefix(self.lines.get(row).map(String::as_str).unwrap_or(""));
                let (from, to) = Self::selection_columns(line, row, start, end);
                restyle(&mut buf, (row - top) as u16, from as u16, to as u16, |c| {
                    c.modifier.insert(Modifier::REVERSED)
                });
            }
        }

        // Flashes, newest last, at the right edge from the top.
        let flashes: Vec<String> = self
            .flashes
            .iter()
            .map(|f| truncate_to_width(&format!(" {} ", f.message), w, "", false))
            .collect();
        for (row, text) in flashes.iter().rev().take(h).rev().enumerate() {
            let fw = visible_width(text);
            if fw > 0 {
                paint_line(&mut buf, (w - fw) as u16, row as u16, fw, &crate::theme::inverse(text));
            }
        }

        // Out: the changed cells (all of them after a clear), the cursor where IME text goes.
        let mut out = String::from(BEGIN_SYNC);
        let size_changed = self.prev.as_ref().is_none_or(|p| p.area != area);
        let base = if self.full || size_changed {
            self.full = false;
            self.full_redraws += 1;
            out.push_str("\x1b[2J\x1b[H");
            Buffer::empty(area)
        } else {
            self.prev.take().unwrap_or_else(|| Buffer::empty(area))
        };
        let mut bytes: Vec<u8> = Vec::new();
        {
            let mut backend = CrosstermBackend::new(&mut bytes);
            let _ = backend.draw(base.diff(&buf).into_iter());
        }
        out.push_str(&String::from_utf8_lossy(&bytes));
        if let Some((row, col)) = cursor.filter(|&(r, c)| r < h && c < w) {
            out.push_str(&format!("\x1b[{};{}H", row + 1, col + 1));
        }
        out.push_str("\x1b[?25l");
        out.push_str(END_SYNC);
        self.prev = Some(buf);
        out
    }

    /// The screen as plain text, one string per row (tests and diagnostics).
    pub fn screen_text(&self) -> Vec<String> {
        let Some(buf) = self.prev.as_ref() else {
            return Vec::new();
        };
        (0..buf.area.height).map(|y| cells::row_text(buf, y)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{global_lock, key_event};

    fn kb() -> Keybindings {
        let mut kb = Keybindings::new();
        kb.set("tui.altScreen.top", &["ctrl+home"]);
        kb.set("tui.altScreen.bottom", &["ctrl+end"]);
        kb
    }

    fn lines(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("line {i}")).collect()
    }

    fn dock(input: &[&str]) -> Vec<DockPart> {
        vec![
            DockPart { lines: vec![], min: 0 },
            DockPart {
                lines: input.iter().map(|s| s.to_string()).collect(),
                min: 3,
            },
            DockPart {
                lines: vec!["footer".into()],
                min: 0,
            },
        ]
    }

    #[test]
    fn stack_sizes_grow_and_shrink_as_pi_tui_does() {
        let scroll = StackEntry {
            basis: Some(0),
            grow: 1,
            shrink: 1,
            min: 1,
            max: usize::MAX,
        };
        let dock = StackEntry {
            min: 1,
            ..StackEntry::default()
        };
        assert_eq!(allocate_stack_sizes(&[scroll, dock], &[0, 5], Some(20)), vec![15, 5]);
        assert_eq!(allocate_stack_sizes(&[scroll, dock], &[0, 30], Some(20)), vec![1, 19]);
        let parts = [
            StackEntry::default(),
            StackEntry {
                min: 3,
                ..StackEntry::default()
            },
            StackEntry::default(),
        ];
        // 4 + 12 + 2 = 18 rows into 9: shrunk in proportion to their sizes, the input keeps 3.
        let s = allocate_stack_sizes(&parts, &[4, 12, 2], Some(9));
        assert_eq!(s.iter().sum::<usize>(), 9);
        assert!(s[1] >= 3);
        assert_eq!(allocate_stack_sizes(&parts, &[0, 1, 1], None), vec![0, 3, 1]);
    }

    #[test]
    fn the_transcript_follows_the_end_until_scrolled_up() {
        let _g = global_lock();
        let mut s = AltScreen::new();
        s.render(lines(50), &dock(&["╭", "> hi", "╰"]), 40, 10, &kb());
        let screen = s.screen_text();
        assert!(screen[5].starts_with("line 49"), "{screen:?}");
        assert!(screen[6].starts_with('╭'));
        assert!(screen[9].starts_with("footer"));
        assert!(s.handle_key(&key_event("pageup"), &kb()));
        s.render(lines(50), &dock(&["╭", "> hi", "╰"]), 40, 10, &kb());
        let screen = s.screen_text();
        // A page is the viewport less 4 rows of overlap (at least 1): 6 rows here, so 2.
        assert!(screen[0].starts_with("line 42"), "{screen:?}");
        assert!(!s.scroll().is_following_end());
        // New lines arrive: the view stays where it was, the way back is offered.
        s.render(lines(60), &dock(&["╭", "> hi", "╰"]), 40, 10, &kb());
        assert!(s.screen_text()[0].starts_with("line 42"));
        assert!(s.screen_text()[5].contains(" ↓ ctrl+end "));
        assert!(s.handle_key(&key_event("ctrl+end"), &kb()));
        s.render(lines(60), &dock(&["╭", "> hi", "╰"]), 40, 10, &kb());
        assert!(s.screen_text()[5].starts_with("line 59"));
        // home stays with the input once the app rebinds top/bottom.
        assert!(!s.handle_key(&key_event("home"), &kb()));
        assert!(s.handle_key(&key_event("ctrl+home"), &kb()));
        s.render(lines(60), &dock(&["╭", "> hi", "╰"]), 40, 10, &kb());
        assert!(s.screen_text()[0].starts_with("line 0"));
    }

    #[test]
    fn the_scrollbar_shows_while_scrolling_with_a_thumb_in_proportion() {
        let _g = global_lock();
        let mut s = AltScreen::new();
        s.render(lines(100), &dock(&["a", "b", "c"]), 20, 14, &kb());
        assert!(!s.screen_text().iter().any(|l| l.ends_with('│') || l.ends_with('┃')));
        s.scroll_by(-50);
        s.render(lines(100), &dock(&["a", "b", "c"]), 20, 14, &kb());
        let screen = s.screen_text();
        let bar: String = screen[..10].iter().map(|l| l.chars().last().unwrap_or(' ')).collect();
        // 10 rows of track, 100 rows of content: a 1-row thumb is at least 2; halfway down.
        assert_eq!(bar.matches('┃').count(), 2, "{bar}");
        assert_eq!(s.scroll().scrollbar_geometry(), (4, 2));
        assert!(s.next_wakeup().is_some());
    }

    #[test]
    fn search_finds_words_across_lines_and_scrolls_to_the_match() {
        let _g = global_lock();
        let mut transcript = lines(80);
        transcript[10] = "the quick brown".into();
        transcript[11] = "fox jumps".into();
        transcript[70] = "日本語の テキスト".into();
        let m = find_search_matches(&transcript, "BROWN   fox");
        assert_eq!(
            m,
            vec![SearchMatch {
                segments: vec![
                    Segment {
                        row: 10,
                        start_col: 10,
                        end_col: 15
                    },
                    Segment {
                        row: 11,
                        start_col: 0,
                        end_col: 3
                    }
                ]
            }]
        );
        let m = find_search_matches(&transcript, "語の テ");
        assert_eq!(
            m[0].segments[0],
            Segment {
                row: 70,
                start_col: 4,
                end_col: 8
            }
        );
        assert_eq!(
            m[0].segments[1],
            Segment {
                row: 70,
                start_col: 9,
                end_col: 11
            }
        );

        let mut s = AltScreen::new();
        s.search_placeholder = "会話内を検索".into();
        s.search_no_matches = "一致するものがありません".into();
        s.render(transcript.clone(), &dock(&["a", "b", "c"]), 80, 20, &kb());
        assert!(s.handle_key(&key_event("ctrl+shift+f"), &kb()));
        s.render(transcript.clone(), &dock(&["a", "b", "c"]), 80, 20, &kb());
        assert!(s.screen_text().join("\n").contains("会話内を検索"));
        for c in "quick".chars() {
            assert!(s.search_key(&key_event(&c.to_string()), &kb()));
        }
        s.render(transcript.clone(), &dock(&["a", "b", "c"]), 80, 20, &kb());
        let screen = s.screen_text().join("\n");
        assert!(screen.contains("the quick brown"), "{screen}");
        assert!(screen.contains("1/1"));
        assert!(s.search_paste("zzz"));
        s.render(transcript.clone(), &dock(&["a", "b", "c"]), 80, 20, &kb());
        assert!(s.screen_text().join("\n").contains("一致するものがありません"));
        assert!(s.handle_key(&key_event("escape"), &kb()));
        assert!(!s.search_open());
    }

    #[test]
    fn a_resize_redraws_whole_and_unchanged_frames_write_nothing_but_the_cursor() {
        let _g = global_lock();
        let mut s = AltScreen::new();
        let first = s.render(lines(5), &dock(&["a", "> x\x1b_pi:c\x07", "c"]), 30, 10, &kb());
        assert!(first.contains("\x1b[2J"));
        let again = s.render(lines(5), &dock(&["a", "> x\x1b_pi:c\x07", "c"]), 30, 10, &kb());
        assert!(!again.contains("line"));
        // The cursor goes where the marker was (row 8, column 4), hidden.
        assert!(again.contains("\x1b[8;4H\x1b[?25l"), "{again:?}");
        s.render(lines(5), &dock(&["a", "b", "c"]), 25, 10, &kb());
        assert_eq!(s.full_redraws, 2);
    }

    #[test]
    fn a_drag_selects_and_release_hands_over_the_text() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let _g = global_lock();
        let mut s = AltScreen::new();
        s.render(lines(5), &dock(&["a", "b", "c"]), 30, 12, &kb());
        let ev = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Five lines in a 7-row view sit at its top (content shorter than the view).
        assert_eq!(
            s.handle_mouse(&ev(MouseEventKind::Down(MouseButton::Left), 2, 1)),
            MouseOutcome::Render
        );
        s.handle_mouse(&ev(MouseEventKind::Drag(MouseButton::Left), 3, 2));
        assert_eq!(
            s.handle_mouse(&ev(MouseEventKind::Up(MouseButton::Left), 3, 2)),
            MouseOutcome::Copy("ne 1\nline".into())
        );
        let wheel = s.handle_mouse(&ev(MouseEventKind::ScrollUp, 0, 0));
        assert_eq!(wheel, MouseOutcome::Render);
    }

    #[test]
    fn prompt_marks_are_jump_targets_and_not_drawn() {
        let _g = global_lock();
        let mut t = lines(40);
        t[5] = "\x1b]133;A\x07> first".into();
        t[25] = "\x1b]133;A\x07> second".into();
        let mut s = AltScreen::new();
        s.render(t.clone(), &dock(&["a", "b", "c"]), 30, 10, &kb());
        s.handle_key(&key_event("ctrl+up"), &kb());
        s.render(t.clone(), &dock(&["a", "b", "c"]), 30, 10, &kb());
        assert!(s.screen_text()[0].starts_with("> second"), "{:?}", s.screen_text());
        s.handle_key(&key_event("ctrl+up"), &kb());
        s.render(t.clone(), &dock(&["a", "b", "c"]), 30, 10, &kb());
        assert!(s.screen_text()[0].starts_with("> first"));
    }
}
