//! What the transcript is made of (`views.ts`): the user's prompt, the assistant's thinking, text and
//! tool calls, notes and notices, and the margins around them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sasacode_ai::{AssistantContent, AssistantMessage, ToolCall, UserContent};
use sasacode_host::t;
use sasacode_plugin_api::{RenderOptions, ToolRenderer, ToolResult};

use crate::jsfmt::group_thousands;
use crate::links::local_links;
use crate::text::{js_trim, truncate_to_width, utf16_len};
use crate::theme::{self, MARKDOWN_THEME};
use crate::ui::markdown::Markdown;
use crate::ui::{Component, Text};

static EXPANDED: AtomicBool = AtomicBool::new(false);

/// ctrl+o: full tool output and thinking.
pub fn expanded() -> bool {
    EXPANDED.load(Ordering::Relaxed)
}

pub fn set_expanded(on: bool) {
    EXPANDED.store(on, Ordering::Relaxed);
}

const COLLAPSED_LINES: usize = 6;

/// Text from the model, tools, MCP servers, config or files, made safe to print (shared with the CLI).
pub use sasacode_host::plain;

/// A container: its children's lines one after another.
#[derive(Default)]
pub struct Container {
    pub children: Vec<Box<dyn Component + Send>>,
}

impl Container {
    pub fn add(&mut self, c: Box<dyn Component + Send>) {
        self.children.push(c);
    }
}

impl Component for Container {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.children.iter_mut().flat_map(|c| c.render(width)).collect()
    }
    fn invalidate(&mut self) {
        for c in &mut self.children {
            c.invalidate();
        }
    }
}

/// The user's prompt, on its own background, marked as a prompt (OSC 133) so terminals and the
/// full-screen view can jump between prompts.
pub struct UserView(Text);

impl UserView {
    pub fn new(content: &[UserContent]) -> Self {
        let text: Vec<String> = content
            .iter()
            .map(|b| b.as_text().map_or_else(|| "[image]".to_string(), str::to_string))
            .filter(|t| !t.starts_with("[The user interrupted"))
            .collect();
        Self(Text::new(
            format!(
                "\x1b]133;A\x07{}",
                theme::bg_user(&format!("> {}", plain(&text.join("\n"))))
            ),
            0,
            0,
        ))
    }
}

impl Component for UserView {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.render(width)
    }
    fn invalidate(&mut self) {
        self.0.invalidate();
    }
}

/// Lines kept while what they are made from stays the same: the whole transcript is rendered each
/// frame (the spinner's, every 80 ms), and wrapping every line of a long expanded output each time
/// is too slow.
struct LineCache<K> {
    key: Option<K>,
    lines: Vec<String>,
}

impl<K: PartialEq> LineCache<K> {
    fn new() -> Self {
        Self {
            key: None,
            lines: Vec::new(),
        }
    }
    fn get(&mut self, key: K, make: impl FnOnce() -> Vec<String>) -> Vec<String> {
        if self.key.as_ref() != Some(&key) {
            self.lines = make();
            self.key = Some(key);
        }
        self.lines.clone()
    }
    fn clear(&mut self) {
        self.key = None;
    }
}

pub struct ThinkingView {
    pub text: String,
    cache: LineCache<(usize, bool, String)>,
}

impl Default for ThinkingView {
    fn default() -> Self {
        Self {
            text: String::new(),
            cache: LineCache::new(),
        }
    }
}

impl Component for ThinkingView {
    fn render(&mut self, width: usize) -> Vec<String> {
        let exp = expanded();
        let text = self.text.clone();
        self.cache.get((width, exp, text.clone()), || {
            if js_trim(&text).is_empty() {
                return Vec::new();
            }
            let cleaned = plain(&text);
            let lines: Vec<&str> = js_trim(&cleaned)
                .split('\n')
                .filter(|l| !js_trim(l).is_empty())
                .collect();
            let shown: &[&str] = if exp {
                &lines
            } else {
                &lines[lines.len().saturating_sub(2)..]
            };
            let mut out =
                Text::new(theme::dim(&theme::italic(&format!("∴ {}", shown.join("\n  ")))), 0, 0).render(width);
            if !exp && lines.len() > 2 {
                out.insert(
                    0,
                    truncate_to_width(
                        &theme::gray(&format!("∴ thinking… ({} lines, ctrl+o)", lines.len())),
                        width,
                        "...",
                        false,
                    ),
                );
            }
            out
        })
    }
    fn invalidate(&mut self) {
        self.cache.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    Running,
    Done,
    Error,
}

type ToolKey = (
    usize,
    bool,
    ToolStatus,
    String,
    Option<String>,
    usize,
    ToolCall,
    Option<ToolResult>,
);

/// One tool call: a header with its status and summary, then the tool's own rendering, a diff, or
/// its output (folded to a few lines unless expanded).
pub struct ToolView {
    pub call: ToolCall,
    pub status: ToolStatus,
    pub summary: String,
    pub live: String,
    pub output: String,
    pub diff: Option<String>,
    pub result: Option<ToolResult>,
    pub renderer: Option<ToolRenderer>,
    /// Set when a tool_call_raw hook fixed this call.
    pub repair_note: Option<String>,
    /// Characters of arguments streamed so far; the input is parsed only when the call is complete.
    pub received: usize,
    cache: LineCache<ToolKey>,
}

/// A tool view shared between the transcript and the events that update it.
pub type ToolHandle = Arc<Mutex<ToolView>>;

impl ToolView {
    pub fn new(call: ToolCall) -> Self {
        Self {
            call,
            status: ToolStatus::Pending,
            summary: String::new(),
            live: String::new(),
            output: String::new(),
            diff: None,
            result: None,
            renderer: None,
            repair_note: None,
            received: 0,
            cache: LineCache::new(),
        }
    }

    fn body(&self, width: usize, body: &str) -> Vec<String> {
        let exp = expanded();
        let mut lines = Vec::new();
        let custom = match (&self.renderer, self.status) {
            (Some(r), s) if s != ToolStatus::Running => r(
                &self.call,
                self.result.as_ref(),
                RenderOptions {
                    expanded: exp,
                    width: width.min(u16::MAX as usize) as u16,
                },
            )
            .map(|ls| {
                ls.iter()
                    .map(|l| truncate_to_width(l, width, "...", false))
                    .collect::<Vec<_>>()
            }),
            _ => None,
        };
        if let Some(custom) = custom {
            lines.extend(custom);
        } else if let (Some(diff), ToolStatus::Done) = (&self.diff, self.status) {
            lines.extend(render_diff(&plain(diff), width));
        } else if !js_trim(body).is_empty() {
            let all: Vec<&str> = body
                .trim_end_matches(crate::text::is_js_whitespace)
                .split('\n')
                .collect();
            let shown: &[&str] = if exp {
                &all
            } else if self.status == ToolStatus::Running {
                &all[all.len().saturating_sub(COLLAPSED_LINES)..]
            } else {
                &all[..all.len().min(COLLAPSED_LINES)]
            };
            let color: fn(&str) -> String = if self.status == ToolStatus::Error {
                theme::red
            } else {
                theme::gray
            };
            // Wrapped under its own indent: a long line (ask_user's question and answer) stays readable as a block.
            for (i, l) in shown.iter().enumerate() {
                let wrapped = if l.is_empty() {
                    vec![String::new()]
                } else {
                    Text::new(*l, 0, 0).render(width.saturating_sub(4).max(1))
                };
                for (j, w) in wrapped.iter().enumerate() {
                    let lead = if i == 0 && j == 0 { "  ⎿ " } else { "    " };
                    lines.push(color(&format!("{lead}{w}")));
                }
            }
            if !exp && all.len() > shown.len() {
                lines.push(truncate_to_width(
                    &theme::gray(&format!("    … +{} lines (ctrl+o)", all.len() - shown.len())),
                    width,
                    "...",
                    false,
                ));
            }
        }
        lines
    }
}

impl Component for ToolView {
    fn render(&mut self, width: usize) -> Vec<String> {
        let dot = match self.status {
            ToolStatus::Pending => theme::gray("○"),
            ToolStatus::Running => theme::yellow("●"),
            ToolStatus::Done => theme::green("●"),
            ToolStatus::Error => theme::red("●"),
        };
        let streaming = self.status == ToolStatus::Pending && self.received > 0 && self.call.input.is_empty();
        let summary = if streaming {
            t("受信中… {n} 字", &[("n", &group_thousands(self.received as u64))])
        } else {
            let s = if self.summary.is_empty() {
                arg_preview(&self.call.input)
            } else {
                self.summary.clone()
            };
            one_line(&plain(&s))
        };
        let mut lines = vec![truncate_to_width(
            &format!(
                "{dot} {}{}",
                theme::bold(&self.call.name),
                theme::gray(&format!("({summary})"))
            ),
            width,
            "...",
            false,
        )];
        if let Some(note) = &self.repair_note {
            lines.push(truncate_to_width(
                &theme::gray(&format!("  ↻ {}", t("修復: {note}", &[("note", &plain(note))]))),
                width,
                "...",
                false,
            ));
        }
        let text = if self.status == ToolStatus::Running {
            &self.live
        } else {
            &self.output
        };
        let key: ToolKey = (
            width,
            expanded(),
            self.status,
            text.clone(),
            self.diff.clone(),
            self.renderer
                .as_ref()
                .map_or(0, |r| Arc::as_ptr(r) as *const () as usize),
            self.call.clone(),
            self.result.clone(),
        );
        let body_text = plain(text);
        let mut cache = std::mem::replace(&mut self.cache, LineCache::new());
        let body = cache.get(key, || self.body(width, &body_text));
        self.cache = cache;
        lines.extend(body);
        lines
    }

    fn invalidate(&mut self) {
        self.cache.clear();
    }
}

impl Component for ToolHandle {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.lock().unwrap_or_else(|e| e.into_inner()).render(width)
    }
    fn invalidate(&mut self) {
        self.lock().unwrap_or_else(|e| e.into_inner()).invalidate();
    }
}

/// A tool's summary on its one header line: a multi-line command (a heredoc) would otherwise break
/// the screen apart.
fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            // `\s*\n\s*` → " ↵ "
            let start = i;
            let mut j = i;
            let mut newline = false;
            while j < chars.len() && chars[j].is_whitespace() {
                newline |= chars[j] == '\n';
                j += 1;
            }
            if newline {
                out.push_str(" ↵ ");
                i = j;
                continue;
            }
            for &c in &chars[start..j] {
                out.push(if c == '\t' { ' ' } else { c });
            }
            i = j;
            continue;
        }
        let c = chars[i];
        if !matches!(c, '\x00'..='\x08' | '\x0b'..='\x1f' | '\x7f') {
            out.push(c);
        }
        i += 1;
    }
    out
}

fn arg_preview(input: &sasacode_ai::JsonObject) -> String {
    let s = serde_json::to_string(input).unwrap_or_default();
    if utf16_len(&s) <= 80 {
        return s;
    }
    let mut units = 0;
    let mut end = 0;
    for (i, c) in s.char_indices() {
        if units + c.len_utf16() > 77 {
            break;
        }
        units += c.len_utf16();
        end = i + c.len_utf8();
    }
    format!("{}…", &s[..end])
}

fn render_diff(diff: &str, width: usize) -> Vec<String> {
    let exp = expanded();
    let all: Vec<&str> = diff.split('\n').collect();
    let shown = if exp { &all[..] } else { &all[..all.len().min(16)] };
    let mut out = Vec::new();
    for l in shown {
        let color: fn(&str) -> String = if l.starts_with('+') {
            theme::green
        } else if l.starts_with('-') {
            theme::red
        } else if l.starts_with("@@") {
            theme::cyan
        } else {
            theme::gray
        };
        // Expanded, a long line wraps so its end can be read; folded, each stays on one row.
        if exp {
            out.extend(Text::new(color(&format!("    {l}")), 0, 0).render(width));
        } else {
            out.push(color(&truncate_to_width(&format!("    {l}"), width, "...", false)));
        }
    }
    if all.len() > shown.len() {
        out.push(truncate_to_width(
            &theme::gray(&format!("    … +{} lines (ctrl+o)", all.len() - shown.len())),
            width,
            "...",
            false,
        ));
    }
    out
}

enum BlockView {
    Text(Markdown),
    Thinking(ThinkingView),
    Tool(ToolHandle),
}

impl BlockView {
    fn render(&mut self, width: usize) -> Vec<String> {
        match self {
            BlockView::Text(m) => m.render(width),
            BlockView::Thinking(v) => v.render(width),
            BlockView::Tool(v) => v.render(width),
        }
    }
    fn invalidate(&mut self) {
        match self {
            BlockView::Text(m) => m.invalidate(),
            BlockView::Thinking(v) => v.invalidate(),
            BlockView::Tool(v) => v.invalidate(),
        }
    }
    /// Whether this is the kind of view `block` is shown with (and, for a tool call, that very call).
    fn fits(&self, block: &AssistantContent) -> bool {
        match (self, block) {
            (BlockView::Text(_), AssistantContent::Text(_)) => true,
            (BlockView::Thinking(_), AssistantContent::Thinking(_)) => true,
            (BlockView::Tool(v), AssistantContent::ToolCall(c)) => {
                v.lock().unwrap_or_else(|e| e.into_inner()).call.id == c.id
            }
            _ => false,
        }
    }
}

/// An assistant turn: thinking, markdown text, and tool calls in stream order.
pub struct AssistantView {
    blocks: Vec<(usize, BlockView)>,
    /// Links to local files are resolved against it, so the terminal can open them.
    cwd: Option<String>,
}

impl AssistantView {
    pub fn new(cwd: Option<String>) -> Self {
        Self {
            blocks: Vec::new(),
            cwd,
        }
    }

    /// How many views it shows (one per block).
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The message as it is now; `on_tool` hears of each new tool call's view.
    pub fn update(&mut self, message: &AssistantMessage, on_tool: &mut dyn FnMut(&ToolCall, &ToolHandle)) {
        for (i, b) in message.content.iter().enumerate() {
            let pos = self.blocks.iter().position(|(k, _)| *k == i);
            // The core may drop blocks from a reply it settles (unsigned thinking after an
            // interrupt), so the block at an index can be another kind now: a new view, not the wrong one.
            let pos = match pos {
                Some(p) if !self.blocks[p].1.fits(b) => {
                    self.blocks.remove(p);
                    None
                }
                p => p,
            };
            let pos = match pos {
                Some(p) => p,
                None => {
                    let view = match b {
                        AssistantContent::Text(_) => {
                            let mut md = Markdown::new("", 0, 0, MARKDOWN_THEME);
                            if let Some(cwd) = self.cwd.clone() {
                                md = md.with_transform(Box::new(move |m, _| local_links(m, &cwd)));
                            }
                            BlockView::Text(md)
                        }
                        AssistantContent::Thinking(_) => BlockView::Thinking(ThinkingView::default()),
                        AssistantContent::ToolCall(c) => {
                            let handle: ToolHandle = Arc::new(Mutex::new(ToolView::new(c.clone())));
                            on_tool(c, &handle);
                            BlockView::Tool(handle)
                        }
                    };
                    let at = self
                        .blocks
                        .iter()
                        .position(|(k, _)| *k > i)
                        .unwrap_or(self.blocks.len());
                    self.blocks.insert(at, (i, view));
                    at
                }
            };
            match (&mut self.blocks[pos].1, b) {
                (BlockView::Text(md), AssistantContent::Text(tc)) => {
                    let text = plain(&tc.text);
                    if md.text() != text {
                        md.set_text(text);
                    }
                }
                (BlockView::Thinking(v), AssistantContent::Thinking(tc)) => v.text = tc.thinking.clone(),
                (BlockView::Tool(v), AssistantContent::ToolCall(c)) => {
                    v.lock().unwrap_or_else(|e| e.into_inner()).call = c.clone()
                }
                _ => {}
            }
        }
        // Views past the end show blocks that were dropped (a half-received tool call, for one).
        let n = message.content.len();
        self.blocks.retain(|(k, _)| *k < n);
    }

    /// Arguments arriving for the tool call at `index`: a long file to write can take many seconds.
    pub fn receiving(&mut self, index: usize, chars: usize) {
        if let Some((_, BlockView::Tool(v))) = self.blocks.iter_mut().find(|(k, _)| *k == index) {
            v.lock().unwrap_or_else(|e| e.into_inner()).received += chars;
        }
    }
}

impl Component for AssistantView {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.blocks.iter_mut().flat_map(|(_, v)| v.render(width)).collect()
    }
    fn invalidate(&mut self) {
        for (_, v) in &mut self.blocks {
            v.invalidate();
        }
    }
}

/// A bundled plugin's note to the model (a todo or progress reminder): one dim line, not a prompt.
pub struct NoteView {
    text: String,
}

impl NoteView {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

impl Component for NoteView {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut one = String::with_capacity(self.text.len());
        for c in self.text.chars() {
            if c.is_whitespace() {
                if !one.ends_with(' ') || one.is_empty() {
                    one.push(' ');
                }
            } else {
                one.push(c);
            }
        }
        vec![truncate_to_width(
            &theme::gray(&format!(
                "  ↳ {}",
                t("sasacode からモデルへ: {text}", &[("text", &one)])
            )),
            width,
            "...",
            false,
        )]
    }
}

/// A line of information in the transcript (gray unless coloured otherwise).
pub struct Notice(Text);

impl Notice {
    pub fn new(text: &str) -> Self {
        Self::colored(text, theme::gray)
    }
    pub fn colored(text: &str, color: fn(&str) -> String) -> Self {
        Self(Text::new(color(text), 0, 0))
    }
}

impl Component for Notice {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.render(width)
    }
    fn invalidate(&mut self) {
        self.0.invalidate();
    }
}

/// A rendered line must stay on its row: a stray newline, carriage return or other control
/// character from model or tool text would move the cursor and leave text over the input and
/// footer. Escape sequences (ESC … and the BEL ending an OSC) are kept; a tab becomes the spaces
/// its width was counted as.
pub fn safe_line(l: &str) -> String {
    let control = |c: char| matches!(c, '\x00'..='\x06' | '\x08'..='\x1a' | '\x1c'..='\x1f' | '\x7f');
    if !l.chars().any(control) {
        return l.to_string();
    }
    l.replace('\t', "   ").chars().filter(|&c| !control(c)).collect()
}

fn prompt_mark(l: &str) -> Option<&'static str> {
    ["\x1b]133;A\x07", "\x1b]133;A\x1b\\"]
        .into_iter()
        .find(|m| l.starts_with(m))
}

/// Margins around a component, so text does not run into the terminal's edges or the scrollbar.
pub struct Padded<C> {
    pub child: C,
    left: usize,
    right: usize,
    top: usize,
    bottom: usize,
    /// The child's lines last frame and what they became: most of a long transcript is the same each frame.
    last: Option<(usize, Vec<String>, Vec<String>)>,
}

impl<C: Component> Padded<C> {
    pub fn new(child: C, left: usize, right: usize) -> Self {
        Self::with_rows(child, left, right, 0, 0)
    }

    pub fn with_rows(child: C, left: usize, right: usize, top: usize, bottom: usize) -> Self {
        Self {
            child,
            left,
            right,
            top,
            bottom,
            last: None,
        }
    }
}

impl<C: Component> Component for Padded<C> {
    fn render(&mut self, width: usize) -> Vec<String> {
        let inner = width.saturating_sub(self.left + self.right).max(10);
        let pad = " ".repeat(self.left.min(width.saturating_sub(inner)));
        let from = self.child.render(inner);
        let last = self.last.take().filter(|(w, _, _)| *w == width);
        // A prompt mark (OSC 133) must stay at the start of the line to be found.
        let lines: Vec<String> = from
            .iter()
            .enumerate()
            .map(|(i, l)| {
                if let Some((_, f, to)) = &last
                    && f.get(i) == Some(l)
                    && let Some(done) = to.get(i)
                {
                    return done.clone();
                }
                let l = safe_line(l);
                match prompt_mark(&l) {
                    Some(mark) => format!("{mark}{pad}{}", &l[mark.len()..]),
                    None => format!("{pad}{l}"),
                }
            })
            .collect();
        let mut out = vec![String::new(); self.top];
        out.extend(lines.iter().cloned());
        out.extend(std::iter::repeat_n(String::new(), self.bottom));
        self.last = Some((width, from, lines));
        out
    }

    fn invalidate(&mut self) {
        self.last = None;
        self.child.invalidate();
    }
}
