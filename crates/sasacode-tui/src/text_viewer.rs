//! A read-only pager for api.ui.showText (`text-viewer.ts`). The content is data: no HTML, no link
//! opening, and the clipboard is written only when the copy key is pressed.

use crossterm::event::KeyEvent;
use sasacode_plugin_api::{Lang, ShowTextOptions, TextFormat};

use crate::clipboard::Copied;
use crate::dialogs::Rows;
use crate::keys::{Keybindings, matches_key, printable};
use crate::text::{strip_terminal_sequences, truncate_to_width};
use crate::theme::{self, MARKDOWN_THEME};
use crate::ui::input::{Input, InputAction};
use crate::ui::markdown::Markdown;
use crate::ui::{Component, Text};
use crate::views::plain;

/// What a key asks of the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerAction {
    None,
    /// Closed (once).
    Close,
    /// Copy this text; the outcome goes to [`TextViewer::copied`].
    Copy(String),
}

enum Body {
    Text(Text),
    Markdown(Markdown),
}

impl Body {
    fn render(&mut self, width: usize) -> Vec<String> {
        match self {
            Body::Text(t) => t.render(width),
            Body::Markdown(m) => m.render(width),
        }
    }
    fn invalidate(&mut self) {
        match self {
            Body::Text(t) => t.invalidate(),
            Body::Markdown(m) => m.invalidate(),
        }
    }
}

pub struct TextViewer {
    title: String,
    source: String,
    body: Body,
    offset: usize,
    query: String,
    match_index: usize,
    input: Option<Input>,
    lines: Vec<String>,
    width: usize,
    note: String,
    closed: bool,
    height: Rows,
    ja: bool,
}

/// Only SGR styling is kept: OSC sequences (links the renderer made too) are dropped and any other
/// ESC is shown as ␛.
fn display_only(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(i) = rest.find('\x1b') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        if let Some(body) = after.strip_prefix("\x1b]") {
            // Up to BEL or ST; an unterminated one runs to the end.
            let end = match (body.find('\x07'), body.find("\x1b\\")) {
                (Some(b), Some(s)) if s < b => s + 2,
                (Some(b), _) => b + 1,
                (None, Some(s)) => s + 2,
                (None, None) => body.len(),
            };
            rest = &body[end..];
            continue;
        }
        let sgr = after.strip_prefix("\x1b[").and_then(|r| {
            let n = r.bytes().take_while(|b| b.is_ascii_digit() || *b == b';').count();
            (r.as_bytes().get(n) == Some(&b'm')).then_some(n + 3)
        });
        match sgr {
            Some(len) => {
                out.push_str(&after[..len]);
                rest = &after[len..];
            }
            None => {
                out.push('␛');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

impl TextViewer {
    pub fn new(options: &ShowTextOptions, height: Rows, lang: Lang) -> Self {
        let source = plain(&options.text);
        let body = match options.format {
            TextFormat::Markdown => Body::Markdown(Markdown::new(source.clone(), 0, 0, MARKDOWN_THEME)),
            TextFormat::Text => Body::Text(Text::new(source.clone(), 0, 0)),
        };
        Self {
            title: options.title.clone(),
            source,
            body,
            offset: 0,
            query: String::new(),
            match_index: 0,
            input: None,
            lines: Vec::new(),
            width: 0,
            note: String::new(),
            closed: false,
            height,
            ja: lang == Lang::Ja,
        }
    }

    fn say(&self, ja: &str, en: &str) -> String {
        if self.ja { ja } else { en }.to_string()
    }

    /// How the copy went: `None` for a failure.
    pub fn copied(&mut self, how: Option<Copied>) {
        self.note = match how {
            Some(Copied::Terminal) => self.say("端末にコピーを要求しました", "Copy requested from terminal"),
            Some(Copied::Native) => self.say("コピーしました", "Copied"),
            None => self.say("コピーに失敗しました", "Copy failed"),
        };
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn handle_paste(&mut self, text: &str) {
        if let Some(input) = self.input.as_mut() {
            input.handle_paste(text);
        }
    }

    pub fn handle_key(&mut self, ev: &KeyEvent, kb: &Keybindings) -> ViewerAction {
        if self.closed {
            return ViewerAction::None;
        }
        if let Some(input) = self.input.as_mut() {
            if matches_key(ev, "escape") {
                self.input = None;
                return ViewerAction::None;
            }
            if let InputAction::Submit(query) = input.handle_key(ev, kb) {
                self.query = plain(&query).to_lowercase();
                self.match_index = self.offset;
                self.input = None;
                self.find(1, true);
            }
            return ViewerAction::None;
        }
        let page = (self.height)().max(1) as i64;
        let ch = printable(ev);
        let mut offset = self.offset as i64;
        let mut action = ViewerAction::None;
        if matches_key(ev, "escape") || ch == Some('q') {
            self.closed = true;
            action = ViewerAction::Close;
        } else if matches_key(ev, "up") || ch == Some('k') {
            offset -= 1;
        } else if matches_key(ev, "down") || ch == Some('j') {
            offset += 1;
        } else if matches_key(ev, "pageup") {
            offset -= page;
        } else if matches_key(ev, "pagedown") || ch == Some(' ') {
            offset += page;
        } else if matches_key(ev, "home") || ch == Some('g') {
            offset = 0;
        } else if matches_key(ev, "end") || ch == Some('G') {
            offset = self.lines.len() as i64 - page;
        } else if ch == Some('/') {
            let mut input = Input::default();
            input.focused = true;
            self.input = Some(input);
        } else if ch == Some('n') {
            self.find(1, false);
            offset = self.offset as i64;
        } else if ch == Some('N') {
            self.find(-1, false);
            offset = self.offset as i64;
        } else if ch == Some('y') {
            self.note = self.say("コピー中…", "Copying…");
            action = ViewerAction::Copy(self.source.clone());
        }
        let max = (self.lines.len() as i64 - page).max(0);
        self.offset = offset.clamp(0, max) as usize;
        action
    }

    fn find(&mut self, direction: i64, include_current: bool) {
        if self.query.is_empty() || self.lines.is_empty() {
            return;
        }
        let n = self.lines.len() as i64;
        let first = if include_current { 0 } else { 1 };
        for step in first..=n {
            let index = ((self.match_index as i64 + direction * step) % n + n) % n;
            let text = strip_terminal_sequences(&self.lines[index as usize]).to_lowercase();
            if text.contains(&self.query) {
                self.offset = index as usize;
                self.match_index = index as usize;
                self.note = format!("/{}", self.query);
                return;
            }
        }
        self.note = self.say("見つかりません", "No match");
    }
}

impl Component for TextViewer {
    fn render(&mut self, width: usize) -> Vec<String> {
        if width != self.width {
            self.width = width;
            self.lines = self.body.render(width.max(1)).iter().map(|l| display_only(l)).collect();
        }
        let page = (self.height)().max(1);
        self.offset = self.offset.min(self.lines.len().saturating_sub(page));
        let hint = self.say(
            "↑↓/PgUp/PgDn:移動 /:検索 n/N:次/前 y:コピー esc:閉じる",
            "↑↓/PgUp/PgDn:scroll /:search n/N:next/prev y:copy esc:close",
        );
        let footer = format!(
            "{}–{}/{} {}",
            self.offset + 1,
            (self.offset + page).min(self.lines.len()),
            self.lines.len(),
            self.note
        );
        let mut out = vec![truncate_to_width(
            &theme::bold(&plain(&self.title)),
            width,
            "...",
            false,
        )];
        out.extend(self.lines.iter().skip(self.offset).take(page).cloned());
        match self.input.as_mut() {
            Some(input) => out.extend(input.render(width)),
            None => out.push(truncate_to_width(&theme::gray(&hint), width, "...", false)),
        }
        out.push(truncate_to_width(&theme::gray(&footer), width, "...", false));
        out
    }

    fn invalidate(&mut self) {
        self.width = 0;
        self.body.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::key_event;

    fn plain_lines(lines: &[String]) -> String {
        strip_terminal_sequences(&lines.join("\n"))
    }

    fn opts(title: &str, text: &str, format: TextFormat) -> ShowTextOptions {
        ShowTextOptions {
            title: title.into(),
            text: text.into(),
            format,
        }
    }

    fn press(v: &mut TextViewer, id: &str) -> ViewerAction {
        v.handle_key(&key_event(id), &Keybindings::new())
    }

    // "pager scrolls, searches forwards/backwards and closes once"
    #[test]
    fn pager_scrolls_searches_and_closes_once() {
        let text: Vec<String> = (0..30).map(|i| format!("row-{i:02}")).collect();
        let mut v = TextViewer::new(
            &opts("Preview", &text.join("\n"), TextFormat::Text),
            Box::new(|| 4),
            Lang::En,
        );
        assert!(plain_lines(&v.render(90)).contains("row-00"));
        press(&mut v, "space");
        assert!(plain_lines(&v.render(90)).contains("row-04"));
        press(&mut v, "/");
        for c in "row-2".chars() {
            press(&mut v, &c.to_string());
        }
        press(&mut v, "enter");
        assert!(plain_lines(&v.render(90)).contains("row-20"));
        press(&mut v, "n");
        assert!(plain_lines(&v.render(90)).contains("row-21"));
        press(&mut v, "shift+n");
        assert!(plain_lines(&v.render(90)).contains("row-20"));
        press(&mut v, "shift+g");
        assert!(plain_lines(&v.render(90)).contains("row-29"));
        press(&mut v, "g");
        assert!(plain_lines(&v.render(90)).contains("row-00"));
        let mut closed = 0;
        for id in ["escape", "q"] {
            if press(&mut v, id) == ViewerAction::Close {
                closed += 1;
            }
        }
        assert_eq!(closed, 1);
    }

    // "untrusted text cannot emit control sequences and only a copy key writes the clipboard"
    #[test]
    fn untrusted_text_cannot_emit_control_sequences() {
        let mut v = TextViewer::new(
            &opts(
                "Title\x1b[2J",
                "# heading\n[link](https://example.invalid)\n\x1b]52;c;evil\x07\n<script>bad</script>",
                TextFormat::Markdown,
            ),
            Box::new(|| 12),
            Lang::En,
        );
        let rendered = v.render(100).join("\n");
        assert!(!rendered.contains("\x1b]"));
        assert!(!rendered.contains("\x1b[2J"));
        assert!(rendered.contains("heading"));
        let mut copied = Vec::new();
        for id in ["j", "k", "space", "n"] {
            if let ViewerAction::Copy(s) = press(&mut v, id) {
                copied.push(s);
            }
        }
        assert!(copied.is_empty());
        if let ViewerAction::Copy(s) = press(&mut v, "y") {
            copied.push(s);
        }
        assert_eq!(copied.len(), 1);
        assert!(!copied[0].contains('\x1b'));
    }

    // "escape exits search before closing the viewer and narrow layouts stay bounded"
    #[test]
    fn escape_exits_search_before_closing() {
        let mut v = TextViewer::new(
            &opts("A long title", &"text\n".repeat(100), TextFormat::Text),
            Box::new(|| 3),
            Lang::Ja,
        );
        v.render(10);
        press(&mut v, "/");
        assert_eq!(press(&mut v, "escape"), ViewerAction::None);
        assert!(!v.is_closed());
        assert!(v.render(10).len() <= 6);
        assert_eq!(press(&mut v, "escape"), ViewerAction::Close);
        assert!(v.is_closed());
    }
}
