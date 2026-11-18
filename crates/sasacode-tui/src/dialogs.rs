//! Dialogs that take the input box's place (`dialogs.ts`): a picker, the tool approval, a one-line
//! input and a check list. Each answers once through the callback it was made with.

use std::collections::BTreeSet;

use crossterm::event::KeyEvent;
use sasacode_agent::permission::{PermissionCheck, PermissionPolicy};
use sasacode_agent::tool_exec::{ApprovalAnswer, ApprovalDecision, ApprovalRequest};
use sasacode_host::t;

use crate::keys::{Keybindings, matches_key, printable};
use crate::text::{
    graphemes, js_trim, strip_terminal_sequences, truncate_to_width, visible_width, wrap_text_with_ansi,
};
use crate::theme::{self, SELECT_THEME};
use crate::ui::input::{Input, InputAction};
use crate::ui::select_list::{SelectEvent, SelectItem, SelectLayout, SelectList};
use crate::ui::{Component, Text};
use crate::views::plain;

/// The rows a dialog may take (the terminal's height, as it is now).
pub type Rows = Box<dyn Fn() -> usize + Send>;

/// Something shown in the input box's place until it answers.
pub trait Dialog: Send {
    fn render(&mut self, width: usize) -> Vec<String>;
    fn handle_key(&mut self, ev: &KeyEvent, kb: &Keybindings);
    fn handle_paste(&mut self, _text: &str) {}
    fn set_focused(&mut self, _focused: bool) {}
    /// Answered (or cancelled): the dialog can go.
    fn finished(&self) -> bool;
}

fn page_hint(from: usize, to: usize, total: usize) -> String {
    t(
        "{from}–{to} / {total} 行 · shift+↑↓ で全文をスクロール",
        &[("from", &from), ("to", &to), ("total", &total)],
    )
}

/// A dialog's title (and message), wrapped so all of it can be read: ask_user's question runs to
/// 500 characters. On a short screen it gets the rows the rest of the dialog leaves, and scrolls
/// with shift+↑↓ as an approval does, so the choices stay in view.
struct Heading {
    lines: Vec<String>,
    scroll: usize,
    page: usize,
}

impl Heading {
    fn new(lines: Vec<String>) -> Self {
        Self {
            lines,
            scroll: 0,
            page: 1,
        }
    }

    fn handle_key(&mut self, ev: &KeyEvent) -> bool {
        if matches_key(ev, "shift+up") {
            self.scroll = self.scroll.saturating_sub(self.page);
        } else if matches_key(ev, "shift+down") {
            self.scroll += self.page;
        } else {
            return false;
        }
        true
    }

    /// At most `room` rows, and the hint saying where they are when that is not all of it.
    fn render(&mut self, width: usize, room: Option<i64>) -> (Vec<String>, Option<String>) {
        let all: Vec<String> = self.lines.iter().flat_map(|l| wrap_text_with_ansi(l, width)).collect();
        let Some(room) = room else { return (all, None) };
        let room = room.max(1) as usize;
        if all.len() <= room {
            return (all, None);
        }
        self.page = room.saturating_sub(1).max(1);
        self.scroll = self.scroll.min(all.len() - room);
        let lines = all[self.scroll..self.scroll + room].to_vec();
        let where_ = page_hint(self.scroll + 1, self.scroll + lines.len(), all.len());
        (lines, Some(where_))
    }
}

/// A title's lines: the first in bold, the rest (a message under it) in gray.
fn title_lines(title: &str) -> Vec<String> {
    plain(title)
        .split('\n')
        .enumerate()
        .map(|(i, l)| if i == 0 { theme::bold(l) } else { theme::gray(l) })
        .collect()
}

/// An option's description in full under the list, wrapped: its row had room for only part of it.
fn full_description(description: &str, width: usize) -> Vec<String> {
    wrap_text_with_ansi(&theme::gray(description), width.saturating_sub(4).max(1))
        .into_iter()
        .map(|l| format!("    {l}"))
        .collect()
}

fn rule(width: usize, color: fn(&str) -> String) -> String {
    truncate_to_width(&color(&"─".repeat(width)), width, "...", false)
}

fn hint_line(where_: Option<String>, hint: &str, width: usize) -> String {
    let text = match where_ {
        Some(w) => format!("{w} · {hint}"),
        None => hint.to_string(),
    };
    truncate_to_width(&theme::gray(&text), width, "...", false)
}

/// `[\r\n]+` → " ", trimmed.
fn one_line(s: &str) -> String {
    let mut out = String::new();
    let mut brk = false;
    for c in s.chars() {
        if c == '\r' || c == '\n' {
            if !brk {
                out.push(' ');
            }
            brk = true;
        } else {
            out.push(c);
            brk = false;
        }
    }
    js_trim(&out).to_string()
}

pub type PickerDone = Box<dyn FnOnce(Option<SelectItem>) + Send>;

/// A titled list that answers once. Typing narrows the list (substring match).
pub struct Picker {
    items: Vec<SelectItem>,
    /// What each row of the list stands for (the list shows them made safe).
    shown: Vec<SelectItem>,
    list: SelectList,
    filter: String,
    heading: Heading,
    max_visible: usize,
    rows: Option<Rows>,
    done: Option<PickerDone>,
    pub focused: bool,
}

impl Picker {
    /// `rows`: the most the dialog may take (the title gives way and scrolls); unlimited without it.
    pub fn new(title: &str, items: Vec<SelectItem>, done: PickerDone, max_visible: usize, rows: Option<Rows>) -> Self {
        let mut p = Self {
            items,
            shown: Vec::new(),
            list: SelectList::new(Vec::new(), max_visible, SELECT_THEME, SelectLayout::default()),
            filter: String::new(),
            heading: Heading::new(title_lines(title)),
            max_visible,
            rows,
            done: Some(done),
            focused: false,
        };
        p.rebuild();
        p
    }

    fn rebuild(&mut self) {
        let f = self.filter.to_lowercase();
        self.shown = self
            .items
            .iter()
            .filter(|i| {
                f.is_empty()
                    || format!("{} {} {}", i.value, i.label, i.description.as_deref().unwrap_or(""))
                        .to_lowercase()
                        .contains(&f)
            })
            .cloned()
            .collect();
        // Shown made safe: an option (ask_user's) may come from the model, and its escape sequences
        // must not reach the terminal.
        let safe: Vec<SelectItem> = self
            .shown
            .iter()
            .map(|i| SelectItem {
                value: i.value.clone(),
                label: plain(&i.label),
                description: i.description.as_deref().map(plain),
            })
            .collect();
        // Long values (model specs) need room; descriptions get what is left.
        self.list = SelectList::new(
            safe,
            self.max_visible,
            SELECT_THEME,
            SelectLayout {
                min_primary: Some(24),
                max_primary: Some(64),
            },
        );
    }

    fn finish(&mut self, item: Option<SelectItem>) {
        if let Some(done) = self.done.take() {
            done(item);
        }
    }
}

impl Dialog for Picker {
    fn handle_paste(&mut self, text: &str) {
        // A paste narrows the list like typing does; line breaks and control characters are left out.
        let mut clean = String::new();
        let mut in_ctrl = false;
        for c in text.chars() {
            if (c as u32) < 0x20 || c == '\x7f' {
                if !in_ctrl {
                    clean.push(' ');
                }
                in_ctrl = true;
            } else {
                clean.push(c);
                in_ctrl = false;
            }
        }
        self.filter.push_str(js_trim(&clean));
        self.rebuild();
    }

    fn handle_key(&mut self, ev: &KeyEvent, kb: &Keybindings) {
        if self.heading.handle_key(ev) {
            return;
        }
        if matches_key(ev, "backspace") {
            // A whole character, an emoji too: half of one would be left as a broken one.
            let keep: Vec<&str> = graphemes(&self.filter).collect();
            self.filter = keep[..keep.len().saturating_sub(1)].concat();
            self.rebuild();
        } else if let Some(c) = printable(ev) {
            self.filter.push(c);
            self.rebuild();
        } else {
            match self.list.handle_key(ev, kb) {
                SelectEvent::Selected(item) => {
                    // The option as given, not the safe copy shown (the list holds `shown` in order).
                    let original = self.shown.get(self.list.selected_index()).cloned().unwrap_or(item);
                    self.finish(Some(original));
                }
                SelectEvent::Cancelled => self.finish(None),
                _ => {}
            }
        }
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        let list = self.list.render(width);
        // The selected option's description, when its row cut it short.
        let description = self
            .list
            .selected_item()
            .and_then(|i| i.description.as_deref())
            .map(one_line)
            .filter(|d| !d.is_empty());
        let row = list
            .iter()
            .map(|l| strip_terminal_sequences(l))
            .find(|l| l.starts_with("→ "));
        let detail = match &description {
            Some(d) if !row.as_deref().is_some_and(|r| r.contains(d.as_str())) => full_description(d, width),
            _ => Vec::new(),
        };
        let room = self
            .rows
            .as_ref()
            .map(|r| r() as i64 - list.len() as i64 - detail.len() as i64 - 2);
        let (heading, where_) = self.heading.render(width, room);
        let hint = if self.filter.is_empty() {
            t("入力で絞り込み · ↑↓ で選択 · enter で決定 · esc でキャンセル", &[])
        } else {
            t("絞り込み: {filter}", &[("filter", &self.filter)])
        };
        let mut out = vec![rule(width, theme::gray)];
        out.extend(heading);
        out.extend(list);
        out.extend(detail);
        out.push(hint_line(where_, &hint, width));
        out
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn finished(&self) -> bool {
        self.done.is_none()
    }
}

impl Component for Picker {
    fn render(&mut self, width: usize) -> Vec<String> {
        Dialog::render(self, width)
    }
}

pub type ApprovalDone = Box<dyn FnOnce(ApprovalAnswer) + Send>;
pub type Interrupt = Box<dyn FnMut() + Send>;

/// Tool approval: allow once, always allow (adds a session rule), deny, or deny with instructions
/// for the model. Esc stops the whole run (`interrupt`), as it does everywhere else while the agent
/// works. What is asked about (the command, a warning, the reason) shares the rows the choices leave
/// on the screen; when it needs more it scrolls with shift+↑↓, so the choices, the one selected and
/// how to refuse always stay in view.
pub struct ApprovalDialog {
    focused: bool,
    list: SelectList,
    input: Option<Input>,
    title: String,
    body: Text,
    scroll: usize,
    page: usize,
    rows: Rows,
    done: Option<ApprovalDone>,
    interrupt: Interrupt,
    interrupted: bool,
}

impl ApprovalDialog {
    pub fn new(
        req: &ApprovalRequest,
        summary: &str,
        done: ApprovalDone,
        interrupt: Interrupt,
        rows: Option<Rows>,
    ) -> Self {
        let rule = PermissionPolicy::rule_for(&PermissionCheck {
            tool: req.tool.clone(),
            args: req.args.clone(),
            cwd: req.cwd.clone(),
        });
        let raw = if req.tool.name() == "bash" {
            match req.args.get("command") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => "undefined".to_string(),
            }
        } else if !summary.is_empty() {
            summary.to_string()
        } else {
            serde_json::to_string(&req.args).unwrap_or_default()
        };
        // What is approved must be exactly what is shown: control characters are made visible, and called out.
        let has_control = raw
            .chars()
            .any(|c| matches!(c, '\x00'..='\x08' | '\x0b'..='\x1f' | '\x7f'..='\u{9f}'));
        let warn = if has_control {
            format!(
                "\n{}",
                theme::red(&t(
                    "⚠ 制御文字（端末のエスケープシーケンスなど）を含んでいます。␛ などの表示を確かめてください。",
                    &[]
                ))
            )
        } else {
            String::new()
        };
        let title = format!(
            "{} {}",
            theme::yellow("?"),
            theme::bold(&t("{tool} を実行しますか？", &[("tool", &req.tool.name())]))
        );
        let body = Text::new(
            format!(
                "{}{warn}\n{}",
                theme::cyan(&plain(&raw)),
                theme::gray(&plain(&req.reason))
            ),
            0,
            0,
        );
        let list = SelectList::new(
            vec![
                SelectItem::new("allow", t("はい", &[]), None),
                SelectItem::new("always", t("常に許可（終了するまで）", &[]), Some(plain(&rule))),
                SelectItem::new("deny", t("いいえ", &[]), None),
                SelectItem::new("feedback", t("いいえ、代わりの指示を伝える", &[]), None),
            ],
            4,
            SELECT_THEME,
            SelectLayout::default(),
        );
        Self {
            focused: false,
            list,
            input: None,
            title,
            body,
            scroll: 0,
            page: 1,
            rows: rows.unwrap_or_else(|| Box::new(|| 24)),
            done: Some(done),
            interrupt,
            interrupted: false,
        }
    }

    fn finish(&mut self, answer: ApprovalAnswer) {
        if let Some(done) = self.done.take() {
            done(answer);
        }
    }
}

impl Dialog for ApprovalDialog {
    fn handle_key(&mut self, ev: &KeyEvent, kb: &Keybindings) {
        if let Some(input) = self.input.as_mut() {
            match input.handle_key(ev, kb) {
                InputAction::Submit(text) => {
                    let feedback = js_trim(&text).to_string();
                    self.finish(ApprovalAnswer {
                        decision: ApprovalDecision::Deny,
                        feedback: (!feedback.is_empty()).then_some(feedback),
                        args: None,
                    });
                }
                // Back to the choices: the instructions were not what the user wanted after all.
                InputAction::Escape => self.input = None,
                InputAction::None => {}
            }
            return;
        }
        if matches_key(ev, "shift+up") {
            self.scroll = self.scroll.saturating_sub(self.page);
            return;
        }
        if matches_key(ev, "shift+down") {
            self.scroll += self.page;
            return;
        }
        match self.list.handle_key(ev, kb) {
            SelectEvent::Selected(item) => {
                let decision = match item.value.as_str() {
                    "feedback" => {
                        let mut input = Input::default();
                        input.focused = self.focused;
                        self.input = Some(input);
                        return;
                    }
                    "always" => ApprovalDecision::Always,
                    "deny" => ApprovalDecision::Deny,
                    _ => ApprovalDecision::Allow,
                };
                self.finish(ApprovalAnswer::new(decision));
            }
            SelectEvent::Cancelled => {
                self.interrupted = true;
                (self.interrupt)();
            }
            _ => {}
        }
    }

    fn handle_paste(&mut self, text: &str) {
        if let Some(input) = self.input.as_mut() {
            input.handle_paste(text);
        }
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        let choices = match self.input.as_mut() {
            Some(input) => {
                let mut v = vec![theme::gray(&t(
                    "モデルへの指示（enter で送信 · esc で選択に戻る）:",
                    &[],
                ))];
                v.extend(input.render(width));
                v
            }
            None => self.list.render(width),
        };
        let body = self.body.render(width);
        // The rule, the title and the hint line take one row each.
        let room = ((self.rows)() as i64 - choices.len() as i64 - 3).max(1) as usize;
        let mut shown = body.clone();
        let mut hint = if self.input.is_some() {
            String::new()
        } else {
            t("esc で中断", &[])
        };
        if body.len() > room {
            self.page = room.saturating_sub(1).max(1);
            self.scroll = self.scroll.min(body.len() - room);
            shown = body[self.scroll..self.scroll + room].to_vec();
            let where_ = page_hint(self.scroll + 1, self.scroll + shown.len(), body.len());
            hint = if hint.is_empty() {
                where_
            } else {
                format!("{hint} · {where_}")
            };
        }
        let mut lines = vec![
            rule(width, theme::yellow),
            truncate_to_width(&self.title, width, "...", false),
        ];
        lines.extend(shown);
        lines.extend(choices);
        if !hint.is_empty() {
            lines.push(truncate_to_width(&theme::gray(&hint), width, "...", false));
        }
        lines
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if let Some(input) = self.input.as_mut() {
            input.focused = focused;
        }
    }

    fn finished(&self) -> bool {
        self.done.is_none() || self.interrupted
    }
}

impl Component for ApprovalDialog {
    fn render(&mut self, width: usize) -> Vec<String> {
        Dialog::render(self, width)
    }
}

pub type InputDone = Box<dyn FnOnce(Option<String>) + Send>;

/// One line of free text (api.ui.input). Enter submits what was typed, esc cancels.
pub struct InputDialog {
    input: Input,
    heading: Heading,
    rows: Option<Rows>,
    done: Option<InputDone>,
}

impl InputDialog {
    pub fn new(
        title: &str,
        message: Option<&str>,
        placeholder: Option<&str>,
        initial: Option<&str>,
        done: InputDone,
        rows: Option<Rows>,
    ) -> Self {
        let mut lines = title_lines(title);
        if let Some(m) = message {
            lines.extend(plain(m).split('\n').map(theme::gray));
        }
        let mut input = Input::new("> ", placeholder.map(plain).unwrap_or_default(), theme::gray);
        // As a paste, so the cursor ends up after it and control characters are left out.
        if let Some(initial) = initial.filter(|s| !s.is_empty()) {
            input.handle_paste(&initial.replace('\x1b', ""));
        }
        Self {
            input,
            heading: Heading::new(lines),
            rows,
            done: Some(done),
        }
    }

    fn finish(&mut self, value: Option<String>) {
        if let Some(done) = self.done.take() {
            done(value);
        }
    }
}

impl Dialog for InputDialog {
    fn handle_key(&mut self, ev: &KeyEvent, kb: &Keybindings) {
        if self.heading.handle_key(ev) {
            return;
        }
        match self.input.handle_key(ev, kb) {
            InputAction::Submit(v) => self.finish(Some(v)),
            InputAction::Escape => self.finish(None),
            InputAction::None => {}
        }
    }

    fn handle_paste(&mut self, text: &str) {
        self.input.handle_paste(text);
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        let input = self.input.render(width);
        let room = self.rows.as_ref().map(|r| r() as i64 - input.len() as i64 - 2);
        let (heading, where_) = self.heading.render(width, room);
        let mut out = vec![rule(width, theme::gray)];
        out.extend(heading);
        out.extend(input);
        out.push(hint_line(where_, &t("enter で決定 · esc でキャンセル", &[]), width));
        out
    }

    fn set_focused(&mut self, focused: bool) {
        self.input.focused = focused;
    }

    fn finished(&self) -> bool {
        self.done.is_none()
    }
}

impl Component for InputDialog {
    fn render(&mut self, width: usize) -> Vec<String> {
        Dialog::render(self, width)
    }
}

pub type CheckDone = Box<dyn FnOnce(Option<Vec<String>>) + Send>;

/// Check any number of items (api.ui.selectMany). Space toggles, enter confirms, esc cancels.
pub struct CheckList {
    items: Vec<SelectItem>,
    cursor: usize,
    checked: BTreeSet<String>,
    heading: Heading,
    max_visible: usize,
    rows: Option<Rows>,
    done: Option<CheckDone>,
}

impl CheckList {
    pub fn new(
        title: &str,
        items: Vec<SelectItem>,
        selected: &[String],
        done: CheckDone,
        max_visible: usize,
        rows: Option<Rows>,
    ) -> Self {
        let checked = selected
            .iter()
            .filter(|v| items.iter().any(|i| &i.value == *v))
            .cloned()
            .collect();
        Self {
            items,
            cursor: 0,
            checked,
            heading: Heading::new(title_lines(title)),
            max_visible,
            rows,
            done: Some(done),
        }
    }

    fn finish(&mut self, value: Option<Vec<String>>) {
        if let Some(done) = self.done.take() {
            done(value);
        }
    }
}

impl Dialog for CheckList {
    fn handle_key(&mut self, ev: &KeyEvent, _kb: &Keybindings) {
        let n = self.items.len();
        if self.heading.handle_key(ev) {
            return;
        }
        if matches_key(ev, "escape") {
            self.finish(None);
        } else if matches_key(ev, "enter") {
            let values = self
                .items
                .iter()
                .map(|i| i.value.clone())
                .filter(|v| self.checked.contains(v))
                .collect();
            self.finish(Some(values));
        } else if n == 0 {
        } else if matches_key(ev, "up") {
            self.cursor = (self.cursor + n - 1) % n;
        } else if matches_key(ev, "down") {
            self.cursor = (self.cursor + 1) % n;
        } else if matches_key(ev, "space") {
            let v = self.items[self.cursor].value.clone();
            if !self.checked.remove(&v) {
                self.checked.insert(v);
            }
        }
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        let len = self.items.len() as i64;
        let mv = self.max_visible as i64;
        let start = (self.cursor as i64 - mv / 2).min(len - mv).max(0) as usize;
        let shown = &self.items[start..(start + self.max_visible).min(self.items.len())];
        let mut detail = Vec::new();
        let mut rows: Vec<String> = shown
            .iter()
            .enumerate()
            .map(|(k, item)| {
                let at = start + k == self.cursor;
                let check = if self.checked.contains(&item.value) {
                    "[x]"
                } else {
                    "[ ]"
                };
                let label = format!("{} {check} {}", if at { "›" } else { " " }, plain(&item.label));
                let desc = item
                    .description
                    .as_deref()
                    .filter(|d| !d.is_empty())
                    .map(|d| format!("  {}", theme::gray(&plain(d))))
                    .unwrap_or_default();
                let row = format!("{}{desc}", if at { theme::bold(&theme::cyan(&label)) } else { label });
                // The description of the option at the cursor in full, when its row cuts it short.
                if at
                    && !desc.is_empty()
                    && visible_width(&row) > width
                    && let Some(d) = &item.description
                {
                    detail = full_description(&one_line(&plain(d)), width);
                }
                truncate_to_width(&row, width, "...", false)
            })
            .collect();
        rows.extend(detail);
        if self.items.len() > self.max_visible {
            rows.push(truncate_to_width(
                &theme::gray(&format!("({}/{})", self.cursor + 1, self.items.len())),
                width,
                "...",
                false,
            ));
        }
        let room = self.rows.as_ref().map(|r| r() as i64 - rows.len() as i64 - 2);
        let (heading, where_) = self.heading.render(width, room);
        let mut out = vec![rule(width, theme::gray)];
        out.extend(heading);
        out.extend(rows);
        out.push(hint_line(
            where_,
            &t("space で選択/解除 · ↑↓ で移動 · enter で決定 · esc でキャンセル", &[]),
            width,
        ));
        out
    }

    fn finished(&self) -> bool {
        self.done.is_none()
    }
}

impl Component for CheckList {
    fn render(&mut self, width: usize) -> Vec<String> {
        Dialog::render(self, width)
    }
}
