//! Markdown rendered as styled terminal lines (pi-tui `Markdown`). pulldown-cmark parses; its events
//! are folded into the token shapes pi-tui renders from marked (paragraph, text, space, list, …),
//! and the rendering follows pi-tui line for line: blank-line spacing, list markers, tables that
//! shrink and wrap their cells, quotes with a border, links as OSC 8 or `text (url)`.
//!
//! Math (`$…$`, `$$…$$`) is shown as its source: pi-tui's LaTeX renderer is not ported.

use std::ops::Range;
use std::sync::atomic::{AtomicU8, Ordering};

use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};

use crate::text::{apply_background_to_line, is_js_whitespace, js_trim, visible_width, wrap_text_with_ansi};
use crate::theme::{MarkdownTheme, Style};
use crate::ui::Component;

// ── hyperlink capability ────────────────────────────────────────────────────

const CAP_UNKNOWN: u8 = 0;
const CAP_OFF: u8 = 1;
const CAP_ON: u8 = 2;

static HYPERLINKS: AtomicU8 = AtomicU8::new(CAP_UNKNOWN);
static HYPERLINKS_OVERRIDE: AtomicU8 = AtomicU8::new(CAP_UNKNOWN);

/// Force links on or off (`None`: detect from the environment).
pub fn set_hyperlinks_override(on: Option<bool>) {
    HYPERLINKS_OVERRIDE.store(
        match on {
            None => CAP_UNKNOWN,
            Some(false) => CAP_OFF,
            Some(true) => CAP_ON,
        },
        Ordering::Relaxed,
    );
}

/// Forget the detected capability (tests).
pub fn reset_capabilities_cache() {
    HYPERLINKS.store(CAP_UNKNOWN, Ordering::Relaxed);
}

/// Whether links are written as OSC 8 hyperlinks: an override, `PI_HYPERLINKS=1|0`, or a terminal
/// known to show them (under tmux, only when the client forwards them).
pub fn hyperlinks() -> bool {
    match HYPERLINKS_OVERRIDE.load(Ordering::Relaxed) {
        CAP_ON => return true,
        CAP_OFF => return false,
        _ => {}
    }
    match HYPERLINKS.load(Ordering::Relaxed) {
        CAP_ON => true,
        CAP_OFF => false,
        _ => {
            let on = detect_hyperlinks();
            HYPERLINKS.store(if on { CAP_ON } else { CAP_OFF }, Ordering::Relaxed);
            on
        }
    }
}

fn env_lower(key: &str) -> String {
    std::env::var(key).unwrap_or_default().to_lowercase()
}

fn env_set(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|v| !v.is_empty())
}

fn detect_hyperlinks() -> bool {
    match std::env::var("PI_HYPERLINKS").as_deref() {
        Ok("1") => return true,
        Ok("0") => return false,
        _ => {}
    }
    let term_program = env_lower("TERM_PROGRAM");
    let term = env_lower("TERM");
    if env_set("TMUX") || term.starts_with("tmux") {
        return probe_tmux_hyperlinks();
    }
    if term.starts_with("screen") {
        return false;
    }
    if env_set("KITTY_WINDOW_ID") || term_program == "kitty" {
        return true;
    }
    if term_program == "ghostty" || term.contains("ghostty") || env_set("GHOSTTY_RESOURCES_DIR") {
        return true;
    }
    if env_set("WEZTERM_PANE") || term_program == "wezterm" {
        return true;
    }
    if term_program == "warpterminal" || env_set("WARP_SESSION_ID") || env_set("WARP_TERMINAL_SESSION_UUID") {
        return true;
    }
    if env_set("ITERM_SESSION_ID") || term_program == "iterm.app" {
        return true;
    }
    if env_set("WT_SESSION") {
        return true;
    }
    matches!(term_program.as_str(), "alacritty" | "vscode" | "zed")
}

/// tmux re-emits OSC 8 only when the client's features list `hyperlinks`. The probe's stdin and
/// stderr are closed: nothing may reach the terminal the UI owns.
fn probe_tmux_hyperlinks() -> bool {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let Ok(mut child) = Command::new("tmux")
        .args(["display-message", "-p", "#{client_termfeatures}"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        use std::io::Read;
        let _ = stdout.read_to_string(&mut out);
    }
    out.split(',').any(|f| f.trim() == "hyperlinks")
}

/// `text` as an OSC 8 link to `url`.
pub fn hyperlink(text: &str, url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

// ── tokens ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Inline {
    Text(String),
    Strong(Vec<Inline>),
    Em(Vec<Inline>),
    Del(Vec<Inline>),
    Code(String),
    Link {
        href: String,
        text: String,
        children: Vec<Inline>,
    },
    Br,
    Html(String),
}

#[derive(Debug, Clone)]
struct Item {
    task: Option<bool>,
    blocks: Vec<Block>,
}

#[derive(Debug, Clone)]
enum Block {
    Heading {
        depth: usize,
        inline: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    /// A tight list item's content.
    Text(Vec<Inline>),
    Code {
        lang: String,
        text: String,
        fence: Option<String>,
        last_line: String,
    },
    List {
        ordered: bool,
        start: u64,
        loose: bool,
        items: Vec<Item>,
    },
    Quote(Vec<Block>),
    Hr,
    Html(String),
    Table {
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
        raw: String,
    },
    Space,
}

impl Block {
    fn kind(&self) -> &'static str {
        match self {
            Block::Heading { .. } => "heading",
            Block::Paragraph(_) => "paragraph",
            Block::Text(_) => "text",
            Block::Code { .. } => "code",
            Block::List { .. } => "list",
            Block::Quote(_) => "blockquote",
            Block::Hr => "hr",
            Block::Html(_) => "html",
            Block::Table { .. } => "table",
            Block::Space => "space",
        }
    }

    /// Blocks whose marked tokenizer takes the blank lines after them (no trailing space token).
    fn takes_trailing_blank(&self) -> bool {
        matches!(
            self,
            Block::Heading { .. } | Block::Hr | Block::Code { .. } | Block::Table { .. }
        )
    }
}

struct Builder<'s> {
    src: &'s str,
    events: Vec<(Event<'s>, Range<usize>)>,
    i: usize,
    task: Option<bool>,
}

fn is_inline_tag(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Emphasis
            | Tag::Strong
            | Tag::Strikethrough
            | Tag::Link { .. }
            | Tag::Image { .. }
            | Tag::Superscript
            | Tag::Subscript
    )
}

fn is_inline_event(ev: &Event<'_>) -> bool {
    match ev {
        Event::Start(tag) => is_inline_tag(tag),
        Event::Text(_)
        | Event::Code(_)
        | Event::InlineMath(_)
        | Event::DisplayMath(_)
        | Event::InlineHtml(_)
        | Event::SoftBreak
        | Event::HardBreak
        | Event::FootnoteReference(_)
        | Event::TaskListMarker(_) => true,
        _ => false,
    }
}

impl<'s> Builder<'s> {
    fn new(src: &'s str) -> Self {
        let opts =
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_MATH;
        Self {
            src,
            events: Parser::new_ext(src, opts).into_offset_iter().collect(),
            i: 0,
            task: None,
        }
    }

    fn peek(&self) -> Option<&(Event<'s>, Range<usize>)> {
        self.events.get(self.i)
    }

    /// Blocks up to the end of the enclosing container (whose End is consumed), with their ranges.
    fn blocks(&mut self) -> Vec<(Block, Range<usize>)> {
        let mut out = Vec::new();
        while let Some((ev, range)) = self.peek().cloned() {
            if is_inline_event(&ev) {
                let start = range.start;
                let (inline, end) = self.inlines();
                out.push((Block::Text(inline), start..end.max(start)));
                continue;
            }
            self.i += 1;
            match ev {
                Event::End(_) => return out,
                Event::Rule => out.push((Block::Hr, range)),
                Event::Html(h) => out.push((Block::Html(js_trim(&h).to_string()), range)),
                Event::Start(tag) => {
                    let block = self.block(tag, &range);
                    out.push((block, range));
                }
                _ => {}
            }
        }
        out
    }

    fn block(&mut self, tag: Tag<'s>, range: &Range<usize>) -> Block {
        match tag {
            Tag::Paragraph => {
                let (inline, _) = self.inlines();
                self.i += 1;
                Block::Paragraph(inline)
            }
            Tag::Heading { level, .. } => {
                let (inline, _) = self.inlines();
                self.i += 1;
                Block::Heading {
                    depth: level as usize,
                    inline,
                }
            }
            Tag::BlockQuote(_) => {
                let inner = self.blocks();
                Block::Quote(with_spaces(inner, self.src, false))
            }
            Tag::CodeBlock(kind) => {
                let mut text = String::new();
                while let Some((ev, _)) = self.peek().cloned() {
                    self.i += 1;
                    match ev {
                        Event::Text(t) => text.push_str(&t),
                        Event::End(_) => break,
                        _ => {}
                    }
                }
                let raw = &self.src[range.clone()];
                let (lang, fence) = match kind {
                    CodeBlockKind::Fenced(info) => {
                        let fence: String = js_trim(raw).chars().take_while(|&c| c == '`' || c == '~').collect();
                        text = text.strip_suffix('\n').unwrap_or(&text).to_string();
                        (js_trim(&info).to_string(), Some(fence))
                    }
                    CodeBlockKind::Indented => {
                        text = text.trim_end_matches('\n').to_string();
                        (String::new(), None)
                    }
                };
                Block::Code {
                    lang,
                    text,
                    fence,
                    last_line: raw.rsplit('\n').next().unwrap_or("").to_string(),
                }
            }
            Tag::HtmlBlock => {
                while let Some((ev, _)) = self.peek().cloned() {
                    self.i += 1;
                    if matches!(ev, Event::End(_)) {
                        break;
                    }
                }
                Block::Html(js_trim(&self.src[range.clone()]).to_string())
            }
            Tag::List(start) => {
                let mut items = Vec::new();
                let mut loose = false;
                while let Some((ev, _)) = self.peek().cloned() {
                    self.i += 1;
                    match ev {
                        Event::Start(Tag::Item) => {
                            self.task = None;
                            let blocks = self.blocks();
                            loose |= blocks.iter().any(|(b, _)| matches!(b, Block::Paragraph(_)));
                            items.push(Item {
                                task: self.task.take(),
                                blocks: with_spaces(blocks, self.src, false),
                            });
                        }
                        Event::End(_) => break,
                        _ => {}
                    }
                }
                Block::List {
                    ordered: start.is_some(),
                    start: start.unwrap_or(1),
                    loose,
                    items,
                }
            }
            Tag::Table(_) => {
                let mut header = Vec::new();
                let mut rows = Vec::new();
                let mut row: Vec<Vec<Inline>> = Vec::new();
                while let Some((ev, _)) = self.peek().cloned() {
                    self.i += 1;
                    match ev {
                        Event::Start(Tag::TableCell) => {
                            let (inline, _) = self.inlines();
                            self.i += 1;
                            row.push(inline);
                        }
                        Event::End(TagEnd::TableHead) => header = std::mem::take(&mut row),
                        Event::End(TagEnd::TableRow) => rows.push(std::mem::take(&mut row)),
                        Event::End(TagEnd::Table) => break,
                        _ => {}
                    }
                }
                let n = header.len();
                for r in &mut rows {
                    r.resize(n, Vec::new());
                }
                Block::Table {
                    header,
                    rows,
                    raw: self.src[range.clone()].to_string(),
                }
            }
            _ => {
                // Anything else (footnote definitions, metadata) as its source.
                let mut depth = 1;
                while let Some((ev, _)) = self.peek().cloned() {
                    self.i += 1;
                    match ev {
                        Event::Start(_) => depth += 1,
                        Event::End(_) => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                Block::Paragraph(vec![Inline::Text(js_trim(&self.src[range.clone()]).to_string())])
            }
        }
    }

    /// Inline content up to (not including) the End of its container or the next block; and the
    /// source offset where it ends.
    fn inlines(&mut self) -> (Vec<Inline>, usize) {
        let mut out = Vec::new();
        let mut end = self.peek().map_or(self.src.len(), |(_, r)| r.start);
        while let Some((ev, range)) = self.peek().cloned() {
            if !is_inline_event(&ev) {
                break;
            }
            self.i += 1;
            end = range.end;
            match ev {
                Event::Text(t) => {
                    let raw = &self.src[range.clone()];
                    // Entities stay as written (marked leaves them to an HTML renderer).
                    if raw.starts_with('&') && raw.ends_with(';') && raw != &*t {
                        out.push(Inline::Text(raw.to_string()));
                    } else {
                        out.push(Inline::Text(t.to_string()));
                    }
                }
                Event::Code(t) => out.push(Inline::Code(t.to_string())),
                Event::InlineMath(_) | Event::DisplayMath(_) => {
                    out.push(Inline::Text(self.src[range.clone()].to_string()));
                }
                Event::InlineHtml(h) => out.push(Inline::Html(h.to_string())),
                Event::SoftBreak => out.push(Inline::Text("\n".into())),
                Event::HardBreak => out.push(Inline::Br),
                Event::FootnoteReference(name) => out.push(Inline::Text(format!("[^{name}]"))),
                Event::TaskListMarker(checked) => self.task = Some(checked),
                Event::Start(tag) => {
                    let (children, _) = self.inlines();
                    let close = self.peek().map_or(range.end, |(_, r)| r.end);
                    self.i += 1;
                    end = close.max(end);
                    match tag {
                        Tag::Strong => out.push(Inline::Strong(children)),
                        Tag::Emphasis => out.push(Inline::Em(children)),
                        Tag::Strikethrough => {
                            // Only `~~x~~` strikes through (pi-tui's strict rule); `~x~` is text.
                            if self.src[range.clone()].starts_with("~~") {
                                out.push(Inline::Del(children));
                            } else {
                                out.push(Inline::Text("~".into()));
                                out.extend(children);
                                out.push(Inline::Text("~".into()));
                            }
                        }
                        Tag::Link {
                            link_type, dest_url, ..
                        } => {
                            let (href, text) = match link_type {
                                LinkType::Email => {
                                    let href = if dest_url.starts_with("mailto:") {
                                        dest_url.to_string()
                                    } else {
                                        format!("mailto:{dest_url}")
                                    };
                                    (href, dest_url.trim_start_matches("mailto:").to_string())
                                }
                                LinkType::Autolink => (dest_url.to_string(), dest_url.to_string()),
                                _ => (dest_url.to_string(), self.link_text(&range)),
                            };
                            out.push(Inline::Link { href, text, children });
                        }
                        // An image shows its alt text.
                        Tag::Image { .. } => out.push(Inline::Text(plain_text(&children))),
                        _ => out.extend(children),
                    }
                }
                _ => {}
            }
        }
        (out, end)
    }

    /// The source between a link's brackets (marked's `token.text`).
    fn link_text(&self, range: &Range<usize>) -> String {
        let raw = &self.src[range.clone()];
        let Some(rest) = raw.strip_prefix('[') else {
            return String::new();
        };
        let mut depth = 0usize;
        let mut escaped = false;
        for (i, c) in rest.char_indices() {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '[' => depth += 1,
                ']' if depth == 0 => return rest[..i].to_string(),
                ']' => depth -= 1,
                _ => {}
            }
        }
        rest.to_string()
    }
}

fn plain_text(inline: &[Inline]) -> String {
    let mut out = String::new();
    for i in inline {
        match i {
            Inline::Text(t) | Inline::Code(t) | Inline::Html(t) => out.push_str(t),
            Inline::Strong(c) | Inline::Em(c) | Inline::Del(c) => out.push_str(&plain_text(c)),
            Inline::Link { children, .. } => out.push_str(&plain_text(children)),
            Inline::Br => out.push('\n'),
        }
    }
    out
}

/// Blank lines between blocks become space tokens, as marked lexes them; `trailing`: also after the
/// last block (at the top level).
fn with_spaces(blocks: Vec<(Block, Range<usize>)>, src: &str, trailing: bool) -> Vec<Block> {
    // Where a block's text ends: trailing blank lines left out, quote-only lines (`>`) among them.
    let content_end = |r: &Range<usize>| {
        let mut s = &src[r.start.min(src.len())..r.end.min(src.len())];
        loop {
            s = s.trim_end_matches([' ', '\t', '\n', '\r']);
            match s.rfind('\n') {
                Some(nl) if s[nl + 1..].chars().all(|c| c == '>' || c == ' ' || c == '\t') => s = &s[..nl],
                _ => break,
            }
        }
        r.start + s.len()
    };
    let mut out = Vec::new();
    let n = blocks.len();
    let mut iter = blocks.into_iter().peekable();
    let mut k = 0;
    while let Some((block, range)) = iter.next() {
        k += 1;
        let end = content_end(&range);
        let takes = block.takes_trailing_blank();
        out.push(block);
        if let Some((_, next)) = iter.peek() {
            let gap = &src[end.min(next.start)..next.start];
            if gap.matches('\n').count() >= 2 {
                out.push(Block::Space);
            }
        } else if trailing && k == n && !takes {
            let rest = &src[end.min(src.len())..];
            if rest.len() > 1 && rest.contains('\n') && rest.chars().all(is_js_whitespace) {
                out.push(Block::Space);
            }
        }
    }
    out
}

/// GFM's bare links (`http://…`, `www.…`, `name@host`) inside text, as marked finds them.
fn linkify(text: &str) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut plain_start = 0;
    let mut i = 0;
    let bytes = text.as_bytes();
    let email_char = |c: u8| c.is_ascii_alphanumeric() || b".!#$%&'*+/=?_`{|}~-".contains(&c);
    while i < text.len() {
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &text[i..];
        let found = if rest.starts_with("http://")
            || rest.starts_with("https://")
            || rest.starts_with("ftp://")
            || rest.starts_with("www.")
        {
            match_url(rest).map(|len| {
                let t = &rest[..len];
                let href = if t.starts_with("www.") {
                    format!("http://{t}")
                } else {
                    t.to_string()
                };
                (len, href)
            })
        } else if (i == 0 || !email_char(bytes[i - 1]))
            && (bytes[i].is_ascii_alphanumeric() || b"._+-".contains(&bytes[i]))
        {
            match_email(rest).map(|len| (len, format!("mailto:{}", &rest[..len])))
        } else {
            None
        };
        match found {
            Some((len, href)) => {
                if plain_start < i {
                    out.push(Inline::Text(text[plain_start..i].to_string()));
                }
                let t = text[i..i + len].to_string();
                out.push(Inline::Link {
                    href,
                    text: t.clone(),
                    children: vec![Inline::Text(t)],
                });
                i += len;
                plain_start = i;
            }
            None => i += 1,
        }
    }
    if plain_start < text.len() {
        out.push(Inline::Text(text[plain_start..].to_string()));
    }
    out
}

/// The length of a bare URL at the start of `s`, its trailing punctuation left out.
fn match_url(s: &str) -> Option<usize> {
    let scheme = ["https://", "http://", "ftp://", "www."]
        .into_iter()
        .find(|p| s.starts_with(p))?;
    let host = &s[scheme.len()..];
    let host_len = host
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'.')
        .count();
    if host_len == 0 || !host.as_bytes()[0].is_ascii_alphanumeric() && host.as_bytes()[0] != b'-' {
        return None;
    }
    let mut len = scheme.len()
        + s[scheme.len()..]
            .find(|c: char| c.is_whitespace() || c == '<')
            .unwrap_or(s.len() - scheme.len());
    // Back off trailing punctuation, and a `)` that closes nothing.
    loop {
        let t = &s[..len];
        let Some(last) = t.chars().last() else { break };
        if "?!.,:;*_'\"~".contains(last) {
            len -= last.len_utf8();
        } else if last == ')' && t.matches('(').count() < t.matches(')').count() {
            len -= 1;
        } else {
            break;
        }
    }
    (len > scheme.len()).then_some(len)
}

/// The length of a bare e-mail address at the start of `s`.
fn match_email(s: &str) -> Option<usize> {
    let local = s
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || b"._+-".contains(b))
        .count();
    if local == 0 || s.as_bytes().get(local) != Some(&b'@') {
        return None;
    }
    let domain = &s[local + 1..];
    let part = |d: &str| {
        d.bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
            .count()
    };
    let first = part(domain);
    if first == 0 {
        return None;
    }
    let mut len = first;
    let mut dots = 0;
    while domain.as_bytes().get(len) == Some(&b'.') {
        let next = part(&domain[len + 1..]);
        let seg = &domain[len + 1..len + 1 + next];
        // A part after a dot must end in a letter or digit.
        let trimmed = seg.trim_end_matches(['-', '_']);
        if trimmed.is_empty() {
            break;
        }
        len += 1 + trimmed.len();
        dots += 1;
        if trimmed.len() < seg.len() {
            break;
        }
    }
    if dots == 0 {
        return None;
    }
    let after = domain.as_bytes().get(len);
    if matches!(after, Some(b'-') | Some(b'_')) {
        return None;
    }
    Some(local + 1 + len)
}

/// Adjacent text joined, and bare links found in it (outside links).
fn normalize(inline: Vec<Inline>, in_link: bool) -> Vec<Inline> {
    let mut merged: Vec<Inline> = Vec::new();
    for node in inline {
        let node = match node {
            Inline::Strong(c) => Inline::Strong(normalize(c, in_link)),
            Inline::Em(c) => Inline::Em(normalize(c, in_link)),
            Inline::Del(c) => Inline::Del(normalize(c, in_link)),
            Inline::Link { href, text, children } => Inline::Link {
                href,
                text,
                children: normalize(children, true),
            },
            other => other,
        };
        match (merged.last_mut(), node) {
            (Some(Inline::Text(prev)), Inline::Text(t)) => prev.push_str(&t),
            (_, node) => merged.push(node),
        }
    }
    if in_link {
        return merged;
    }
    merged
        .into_iter()
        .flat_map(|n| match n {
            Inline::Text(t) => linkify(&t),
            other => vec![other],
        })
        .collect()
}

fn normalize_blocks(blocks: &mut [Block]) {
    for b in blocks {
        match b {
            Block::Heading { inline, .. } | Block::Paragraph(inline) | Block::Text(inline) => {
                *inline = normalize(std::mem::take(inline), false);
            }
            Block::List { items, .. } => {
                for item in items {
                    normalize_blocks(&mut item.blocks);
                }
            }
            Block::Quote(inner) => normalize_blocks(inner),
            Block::Table { header, rows, .. } => {
                for cell in header.iter_mut().chain(rows.iter_mut().flatten()) {
                    *cell = normalize(std::mem::take(cell), false);
                }
            }
            _ => {}
        }
    }
}

/// A streamed partial closing fence (`` `` `` of ```` ``` ````) is left out, so the block does not
/// shrink when the last backtick arrives.
fn trim_partial_closing_fence(blocks: &mut [Block]) {
    match blocks.last_mut() {
        Some(Block::List { items, .. }) => {
            if let Some(item) = items.last_mut() {
                trim_partial_closing_fence(&mut item.blocks);
            }
        }
        Some(Block::Quote(inner)) => trim_partial_closing_fence(inner),
        Some(Block::Code {
            text,
            fence: Some(fence),
            last_line,
            ..
        }) => {
            let Some(first) = fence.chars().next() else { return };
            if last_line.is_empty() || last_line.len() >= fence.len() || last_line.chars().any(|c| c != first) {
                return;
            }
            if let Some(t) = text.strip_suffix(last_line.as_str()) {
                *text = t.strip_suffix('\n').unwrap_or(t).to_string();
            }
        }
        _ => {}
    }
}

fn parse(src: &str) -> Vec<Block> {
    let mut b = Builder::new(src);
    let blocks = b.blocks();
    let mut blocks = with_spaces(blocks, src, true);
    normalize_blocks(&mut blocks);
    trim_partial_closing_fence(&mut blocks);
    blocks
}

// ── rendering ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Apply {
    Plain,
    Heading1,
    Heading,
}

#[derive(Clone)]
struct StyleCtx {
    apply: Apply,
    prefix: String,
}

impl StyleCtx {
    fn plain() -> Self {
        Self {
            apply: Apply::Plain,
            prefix: String::new(),
        }
    }
}

fn style_prefix(f: impl Fn(&str) -> String) -> String {
    let styled = f("\u{0}");
    styled.find('\u{0}').map_or(String::new(), |i| styled[..i].to_string())
}

/// Links resolved and shown by the UI: a transform of the source before it is parsed.
pub type Transform = Box<dyn Fn(&str, usize) -> String + Send>;

pub struct Markdown {
    text: String,
    padding_x: usize,
    padding_y: usize,
    theme: MarkdownTheme,
    bg: Option<Style>,
    transform: Option<Transform>,
    cache: Option<(String, usize, Vec<String>)>,
}

impl Markdown {
    pub fn new(text: impl Into<String>, padding_x: usize, padding_y: usize, theme: MarkdownTheme) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
            theme,
            bg: None,
            transform: None,
            cache: None,
        }
    }

    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transform = Some(transform);
        self
    }

    pub fn with_bg(mut self, bg: Style) -> Self {
        self.bg = Some(bg);
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cache = None;
    }

    fn heading_style(&self, apply: Apply, text: &str) -> String {
        let t = &self.theme;
        match apply {
            Apply::Plain => text.to_string(),
            Apply::Heading1 => (t.heading)(&(t.bold)(&(t.underline)(text))),
            Apply::Heading => (t.heading)(&(t.bold)(text)),
        }
    }

    fn apply_with_newlines(&self, ctx: &StyleCtx, text: &str) -> String {
        if ctx.apply == Apply::Plain {
            return text.to_string();
        }
        text.split('\n')
            .map(|s| self.heading_style(ctx.apply, s))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_inline(&self, tokens: &[Inline], ctx: &StyleCtx) -> String {
        let t = &self.theme;
        let mut out = String::new();
        for token in tokens {
            match token {
                Inline::Text(s) | Inline::Html(s) => out.push_str(&self.apply_with_newlines(ctx, s)),
                Inline::Strong(c) => {
                    out.push_str(&(t.bold)(&self.render_inline(c, ctx)));
                    out.push_str(&ctx.prefix);
                }
                Inline::Em(c) => {
                    out.push_str(&(t.italic)(&self.render_inline(c, ctx)));
                    out.push_str(&ctx.prefix);
                }
                Inline::Del(c) => {
                    out.push_str(&(t.strikethrough)(&self.render_inline(c, ctx)));
                    out.push_str(&ctx.prefix);
                }
                Inline::Code(s) => {
                    out.push_str(&(t.code)(s));
                    out.push_str(&ctx.prefix);
                }
                Inline::Br => out.push('\n'),
                Inline::Link { href, text, children } => {
                    let styled = (t.link)(&(t.underline)(&self.render_inline(children, ctx)));
                    if hyperlinks() {
                        out.push_str(&hyperlink(&styled, href));
                    } else {
                        let bare = href.strip_prefix("mailto:").unwrap_or(href);
                        out.push_str(&styled);
                        if text != href && text != bare {
                            out.push_str(&(t.link_url)(&format!(" ({href})")));
                        }
                    }
                    out.push_str(&ctx.prefix);
                }
            }
        }
        if !ctx.prefix.is_empty() {
            while out.ends_with(&ctx.prefix) {
                out.truncate(out.len() - ctx.prefix.len());
            }
        }
        out
    }

    fn render_block(&self, block: &Block, width: usize, next: Option<&str>, ctx: &StyleCtx) -> Vec<String> {
        let t = &self.theme;
        let spaced = |lines: &mut Vec<String>| {
            if next.is_some_and(|n| n != "space") {
                lines.push(String::new());
            }
        };
        let mut lines = Vec::new();
        match block {
            Block::Heading { depth, inline } => {
                let apply = if *depth == 1 { Apply::Heading1 } else { Apply::Heading };
                let hctx = StyleCtx {
                    apply,
                    prefix: style_prefix(|s| self.heading_style(apply, s)),
                };
                let text = self.render_inline(inline, &hctx);
                if *depth >= 3 {
                    lines.push(format!(
                        "{}{text}",
                        self.heading_style(apply, &format!("{} ", "#".repeat(*depth)))
                    ));
                } else {
                    lines.push(text);
                }
                spaced(&mut lines);
            }
            Block::Paragraph(inline) => {
                lines.push(self.render_inline(inline, ctx));
                if next.is_some_and(|n| n != "list" && n != "space") {
                    lines.push(String::new());
                }
            }
            Block::Text(inline) => lines.push(self.render_inline(inline, ctx)),
            Block::Code { lang, text, .. } => {
                lines.push((t.code_block_border)(&format!("```{lang}")));
                for line in text.split('\n') {
                    lines.push(format!("  {}", (t.code_block)(line)));
                }
                lines.push((t.code_block_border)("```"));
                spaced(&mut lines);
            }
            Block::List { .. } => lines.extend(self.render_list(block, 0, width, ctx)),
            Block::Table { .. } => lines.extend(self.render_table(block, width, next, ctx)),
            Block::Quote(inner) => {
                let quote = |s: &str| (t.quote)(&(t.italic)(s));
                let prefix = style_prefix(quote);
                let qctx = StyleCtx {
                    apply: Apply::Plain,
                    prefix: prefix.clone(),
                };
                let qwidth = width.saturating_sub(2).max(1);
                let mut rendered = Vec::new();
                for (k, b) in inner.iter().enumerate() {
                    rendered.extend(self.render_block(b, qwidth, inner.get(k + 1).map(Block::kind), &qctx));
                }
                while rendered.last().is_some_and(String::is_empty) {
                    rendered.pop();
                }
                for line in rendered {
                    let styled = if prefix.is_empty() {
                        quote(&line)
                    } else {
                        quote(&line.replace("\x1b[0m", &format!("\x1b[0m{prefix}")))
                    };
                    for w in wrap_text_with_ansi(&styled, qwidth) {
                        lines.push(format!("{}{w}", (t.quote_border)("│ ")));
                    }
                }
                spaced(&mut lines);
            }
            Block::Hr => {
                lines.push((t.hr)(&"─".repeat(width.min(80))));
                spaced(&mut lines);
            }
            Block::Html(raw) => lines.push(js_trim(raw).to_string()),
            Block::Space => lines.push(String::new()),
        }
        lines
    }

    fn render_list(&self, block: &Block, depth: usize, width: usize, ctx: &StyleCtx) -> Vec<String> {
        let Block::List {
            ordered,
            start,
            loose,
            items,
        } = block
        else {
            return Vec::new();
        };
        let t = &self.theme;
        let mut lines = Vec::new();
        let indent = "    ".repeat(depth);
        for (i, item) in items.iter().enumerate() {
            let bullet = if *ordered {
                format!("{}. ", start + i as u64)
            } else {
                "- ".to_string()
            };
            let task = match item.task {
                Some(true) => "[x] ",
                Some(false) => "[ ] ",
                None => "",
            };
            let marker = format!("{bullet}{task}");
            let first_prefix = format!("{indent}{}", (t.list_bullet)(&marker));
            let continuation = format!("{indent}{}", " ".repeat(visible_width(&marker)));
            let item_width = width.saturating_sub(visible_width(&first_prefix)).max(1);
            let mut any = false;
            for b in &item.blocks {
                if matches!(b, Block::List { .. }) {
                    lines.extend(self.render_list(b, depth + 1, width, ctx));
                    any = true;
                    continue;
                }
                for line in self.render_block(b, item_width, None, ctx) {
                    for w in wrap_text_with_ansi(&line, item_width) {
                        let prefix = if any { &continuation } else { &first_prefix };
                        lines.push(format!("{prefix}{w}"));
                        any = true;
                    }
                }
            }
            if !any {
                lines.push(first_prefix);
            }
            if *loose && i + 1 < items.len() {
                lines.push(String::new());
            }
        }
        lines
    }

    fn render_table(&self, block: &Block, available: usize, next: Option<&str>, ctx: &StyleCtx) -> Vec<String> {
        let Block::Table { header, rows, raw } = block else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        let n = header.len();
        if n == 0 {
            return lines;
        }
        let overhead = 3 * n + 1;
        if available < overhead + n {
            // Too narrow for a stable table: the source, wrapped.
            let mut out = wrap_text_with_ansi(raw, available);
            if next.is_some_and(|k| k != "space") {
                out.push(String::new());
            }
            return out;
        }
        let for_cells = available - overhead;
        const MAX_WORD: usize = 30;
        let longest_word = |s: &str| {
            s.split(is_js_whitespace)
                .filter(|w| !w.is_empty())
                .map(visible_width)
                .max()
                .unwrap_or(0)
                .min(MAX_WORD)
        };
        let mut natural = vec![0usize; n];
        let mut min_word = vec![1usize; n];
        for (i, cell) in header.iter().enumerate() {
            let text = self.render_inline(cell, ctx);
            natural[i] = visible_width(&text);
            min_word[i] = longest_word(&text).max(1);
        }
        for row in rows {
            for (i, cell) in row.iter().enumerate().take(n) {
                let text = self.render_inline(cell, ctx);
                natural[i] = natural[i].max(visible_width(&text));
                min_word[i] = min_word[i].max(longest_word(&text));
            }
        }
        let mut min_cols = min_word.clone();
        let mut min_total: usize = min_cols.iter().sum();
        if min_total > for_cells {
            min_cols = vec![1; n];
            let remaining = for_cells - n;
            if remaining > 0 {
                let total_weight: usize = min_word.iter().map(|w| w.saturating_sub(1)).sum();
                let growth: Vec<usize> = min_word
                    .iter()
                    .map(|w| {
                        if total_weight > 0 {
                            ((w.saturating_sub(1)) as f64 / total_weight as f64 * remaining as f64).floor() as usize
                        } else {
                            0
                        }
                    })
                    .collect();
                for i in 0..n {
                    min_cols[i] += growth[i];
                }
                let mut leftover = remaining - growth.iter().sum::<usize>();
                let mut i = 0;
                while leftover > 0 && i < n {
                    min_cols[i] += 1;
                    leftover -= 1;
                    i += 1;
                }
            }
            min_total = min_cols.iter().sum();
        }
        let natural_total = natural.iter().sum::<usize>() + overhead;
        let widths: Vec<usize> = if natural_total <= available {
            natural.iter().zip(&min_cols).map(|(a, b)| *a.max(b)).collect()
        } else {
            let potential: usize = natural.iter().zip(&min_cols).map(|(a, b)| a.saturating_sub(*b)).sum();
            let extra = for_cells.saturating_sub(min_total);
            let mut w: Vec<usize> = min_cols
                .iter()
                .zip(&natural)
                .map(|(min, nat)| {
                    let delta = nat.saturating_sub(*min);
                    let grow = if potential > 0 {
                        (delta as f64 / potential as f64 * extra as f64).floor() as usize
                    } else {
                        0
                    };
                    min + grow
                })
                .collect();
            let mut remaining = for_cells.saturating_sub(w.iter().sum());
            while remaining > 0 {
                let mut grew = false;
                for i in 0..n {
                    if remaining == 0 {
                        break;
                    }
                    if w[i] < natural[i] {
                        w[i] += 1;
                        remaining -= 1;
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }
            w
        };
        let rule = |l: &str, m: &str, r: &str| {
            let cells: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
            format!("{l}─{}─{r}", cells.join(&format!("─{m}─")))
        };
        let wrap_cell = |text: &str, width: usize| -> Vec<String> {
            let wrapped = wrap_text_with_ansi(text, width.max(1));
            let last = wrapped.len().saturating_sub(1);
            wrapped
                .into_iter()
                .enumerate()
                .map(|(i, l)| {
                    let reset = if i < last { "\x1b[22;23;24;25;27;28;29;39m" } else { "" };
                    format!("{l}{reset}{}", ctx.prefix)
                })
                .collect()
        };
        let row_lines = |cells: &[Vec<Inline>], bold: bool| -> Vec<String> {
            let wrapped: Vec<Vec<String>> = cells
                .iter()
                .enumerate()
                .map(|(i, c)| wrap_cell(&self.render_inline(c, ctx), widths[i]))
                .collect();
            let count = wrapped.iter().map(Vec::len).max().unwrap_or(0);
            (0..count)
                .map(|k| {
                    let parts: Vec<String> = wrapped
                        .iter()
                        .enumerate()
                        .map(|(i, l)| {
                            let text = l.get(k).map_or("", String::as_str);
                            let padded = format!("{text}{}", " ".repeat(widths[i].saturating_sub(visible_width(text))));
                            if bold { (self.theme.bold)(&padded) } else { padded }
                        })
                        .collect();
                    format!("│ {} │", parts.join(" │ "))
                })
                .collect()
        };
        lines.push(rule("┌", "┬", "┐"));
        lines.extend(row_lines(header, true));
        let separator = rule("├", "┼", "┤");
        lines.push(separator.clone());
        for (k, row) in rows.iter().enumerate() {
            lines.extend(row_lines(row, false));
            if k + 1 < rows.len() {
                lines.push(separator.clone());
            }
        }
        lines.push(rule("└", "┴", "┘"));
        if next.is_some_and(|k| k != "space") {
            lines.push(String::new());
        }
        lines
    }
}

impl Component for Markdown {
    fn render(&mut self, width: usize) -> Vec<String> {
        if let Some((text, w, lines)) = &self.cache
            && *text == self.text
            && *w == width
        {
            return lines.clone();
        }
        let content_width = width.saturating_sub(self.padding_x * 2).max(1);
        let text = match &self.transform {
            Some(f) => f(&self.text, content_width),
            None => self.text.clone(),
        };
        if js_trim(&text).is_empty() {
            self.cache = Some((self.text.clone(), width, Vec::new()));
            return Vec::new();
        }
        let blocks = parse(&text.replace('\t', "   "));
        let ctx = StyleCtx::plain();
        let mut rendered = Vec::new();
        for (i, b) in blocks.iter().enumerate() {
            rendered.extend(self.render_block(b, content_width, blocks.get(i + 1).map(Block::kind), &ctx));
        }
        let margin = " ".repeat(self.padding_x);
        let mut content = Vec::new();
        for line in rendered {
            for w in wrap_text_with_ansi(&line, content_width) {
                let with_margins = format!("{margin}{w}{margin}");
                content.push(match self.bg {
                    Some(bg) => apply_background_to_line(&with_margins, width, bg),
                    None => {
                        let fill = width.saturating_sub(visible_width(&with_margins));
                        format!("{with_margins}{}", " ".repeat(fill))
                    }
                });
            }
        }
        let empty = " ".repeat(width);
        let pad_line = match self.bg {
            Some(bg) => apply_background_to_line(&empty, width, bg),
            None => empty,
        };
        let mut result = vec![pad_line.clone(); self.padding_y];
        result.extend(content);
        result.extend(std::iter::repeat_n(pad_line, self.padding_y));
        if result.is_empty() {
            result.push(String::new());
        }
        self.cache = Some((self.text.clone(), width, result.clone()));
        result
    }

    fn invalidate(&mut self) {
        self.cache = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_links_stop_before_trailing_punctuation() {
        assert_eq!(match_url("http://x.com/a."), Some("http://x.com/a".len()));
        assert_eq!(match_url("http://x.com/(a)"), Some("http://x.com/(a)".len()));
        assert_eq!(match_url("http://x.com/a)"), Some("http://x.com/a".len()));
        assert_eq!(match_url("http://"), None);
        assert_eq!(match_email("foo@bar.com now"), Some(11));
        assert_eq!(match_email("foo@bar"), None);
    }

    #[test]
    fn math_is_shown_as_its_source() {
        let mut m = Markdown::new("Math $x^2$ and $$y$$ and $a*b*c$", 0, 0, crate::theme::MARKDOWN_THEME);
        let lines = m.render(60);
        assert_eq!(js_trim(&lines[0]), "Math $x^2$ and $$y$$ and $a*b*c$");
    }

    // A streaming reply is rendered at every prefix: half a table, an open fence, a link cut in its
    // target. None may panic or draw a line wider than the screen.
    #[test]
    fn every_prefix_of_a_streaming_reply_renders_within_its_width() {
        let _g = crate::testing::global_lock();
        set_hyperlinks_override(Some(true));
        let source = "# 見出し **bold** `code`\n\n\
            - item with [a link](https://example.com/some/long/path) and 日本語の文\n  1. nested `x`\n\n\
            > quote with *emphasis* and ~~strike~~\n\n\
            | col | 列 |\n|:--|--:|\n| a very long cell value | 全角の値 |\n\n\
            ```rust\nfn main() { println!(\"{}\", 1); }\n```\n\n\
            Text with 🐱 emoji, ①, and a bare URL https://example.com/x.\n";
        let chars: Vec<(usize, char)> = source.char_indices().collect();
        // Two columns at least: a wide glyph alone is wider than one (the renderer cuts such a line).
        for (width, pad) in [(2usize, 0usize), (3, 0), (4, 1), (9, 1), (20, 2), (47, 1)] {
            for &(i, _) in &chars {
                let mut m = Markdown::new(&source[..i], pad, 0, crate::theme::MARKDOWN_THEME);
                for line in m.render(width) {
                    let w = crate::text::visible_width(&line);
                    assert!(w <= width, "width {width}, prefix {i}: {line:?} is {w} wide");
                }
            }
        }
        set_hyperlinks_override(None);
    }
}
