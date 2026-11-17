//! A list to choose one item from, scrolled around the selection (pi-tui `SelectList`).

use crossterm::event::KeyEvent;

use crate::keys::Keybindings;
use crate::text::{js_trim, truncate_to_width, visible_width};
use crate::theme::SelectTheme;

const DEFAULT_PRIMARY_COLUMN_WIDTH: usize = 32;
const PRIMARY_COLUMN_GAP: usize = 2;
const MIN_DESCRIPTION_WIDTH: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SelectItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

impl SelectItem {
    pub fn new(value: impl Into<String>, label: impl Into<String>, description: Option<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description,
        }
    }

    fn display(&self) -> &str {
        if self.label.is_empty() {
            &self.value
        } else {
            &self.label
        }
    }
}

/// The bounds of the label column: the widest label decides within them.
#[derive(Debug, Clone, Copy, Default)]
pub struct SelectLayout {
    pub min_primary: Option<usize>,
    pub max_primary: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectEvent {
    None,
    Changed,
    Selected(SelectItem),
    Cancelled,
}

/// `text.replace(/[\r\n]+/g, " ").trim()`.
fn single_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_break = false;
    for c in text.chars() {
        if c == '\r' || c == '\n' {
            if !in_break {
                out.push(' ');
            }
            in_break = true;
        } else {
            out.push(c);
            in_break = false;
        }
    }
    js_trim(&out).to_string()
}

pub struct SelectList {
    items: Vec<SelectItem>,
    filtered: Vec<SelectItem>,
    selected: usize,
    max_visible: usize,
    theme: SelectTheme,
    layout: SelectLayout,
}

impl SelectList {
    pub fn new(items: Vec<SelectItem>, max_visible: usize, theme: SelectTheme, layout: SelectLayout) -> Self {
        Self {
            filtered: items.clone(),
            items,
            selected: 0,
            max_visible,
            theme,
            layout,
        }
    }

    /// Keep the items whose value starts with `filter` (case-insensitive).
    pub fn set_filter(&mut self, filter: &str) {
        let f = filter.to_lowercase();
        self.filtered = self
            .items
            .iter()
            .filter(|i| i.value.to_lowercase().starts_with(&f))
            .cloned()
            .collect();
        self.selected = 0;
    }

    pub fn set_selected_index(&mut self, index: usize) {
        self.selected = index.min(self.filtered.len().saturating_sub(1));
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn selected_item(&self) -> Option<&SelectItem> {
        self.filtered.get(self.selected)
    }

    pub fn len(&self) -> usize {
        self.filtered.len()
    }

    pub fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }

    pub fn filtered_items(&self) -> &[SelectItem] {
        &self.filtered
    }

    /// The first and one-past-last rows shown.
    pub fn visible_range(&self) -> (usize, usize) {
        let len = self.filtered.len() as i64;
        let mv = self.max_visible as i64;
        let start = (self.selected as i64 - mv / 2).min(len - mv).max(0);
        (start as usize, ((start + mv).min(len)) as usize)
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        if self.filtered.is_empty() {
            return vec![(self.theme.no_match)()];
        }
        let primary = self.primary_column_width();
        let (start, end) = self.visible_range();
        let mut lines = Vec::new();
        for i in start..end {
            let item = &self.filtered[i];
            let desc = item.description.as_deref().map(single_line);
            lines.push(self.render_item(item, i == self.selected, width, desc.as_deref(), primary));
        }
        if start > 0 || end < self.filtered.len() {
            let scroll = format!("  ({}/{})", self.selected + 1, self.filtered.len());
            lines.push((self.theme.scroll_info)(&truncate_to_width(
                &scroll,
                width.saturating_sub(2),
                "",
                false,
            )));
        }
        lines
    }

    /// Up and down wrap around; enter selects; escape and ctrl+c cancel.
    pub fn handle_key(&mut self, ev: &KeyEvent, kb: &Keybindings) -> SelectEvent {
        let n = self.filtered.len();
        if kb.matches(ev, "tui.select.up") {
            self.selected = if self.selected == 0 {
                n.saturating_sub(1)
            } else {
                self.selected - 1
            };
            SelectEvent::Changed
        } else if kb.matches(ev, "tui.select.down") {
            self.selected = if n == 0 || self.selected == n - 1 {
                0
            } else {
                self.selected + 1
            };
            SelectEvent::Changed
        } else if kb.matches(ev, "tui.select.confirm") {
            match self.filtered.get(self.selected) {
                Some(item) => SelectEvent::Selected(item.clone()),
                None => SelectEvent::None,
            }
        } else if kb.matches(ev, "tui.select.cancel") {
            SelectEvent::Cancelled
        } else {
            SelectEvent::None
        }
    }

    /// A wheel step over the list moves the selection (without wrapping).
    pub fn wheel(&mut self, delta: i32) -> bool {
        if self.filtered.is_empty() {
            return false;
        }
        let before = self.selected;
        let next = (self.selected as i64 + if delta < 0 { -1 } else { 1 }).clamp(0, self.filtered.len() as i64 - 1);
        self.selected = next as usize;
        self.selected != before
    }

    /// A click on row `row` of the rendered list: that item, selected.
    pub fn click(&mut self, row: usize) -> Option<SelectItem> {
        let (start, end) = self.visible_range();
        let index = start + row;
        if index >= end {
            return None;
        }
        self.selected = index;
        self.filtered.get(index).cloned()
    }

    fn render_item(
        &self,
        item: &SelectItem,
        selected: bool,
        width: usize,
        desc: Option<&str>,
        primary: usize,
    ) -> String {
        let prefix = if selected { "→ " } else { "  " };
        let prefix_width = 2;
        if let Some(desc) = desc.filter(|d| !d.is_empty())
            && width > 40
        {
            let column = primary.min(width - prefix_width - 4).max(1);
            let max_primary = column.saturating_sub(PRIMARY_COLUMN_GAP).max(1);
            let value = truncate_to_width(item.display(), max_primary, "", false);
            let value_width = visible_width(&value);
            let spacing = " ".repeat(column.saturating_sub(value_width).max(1));
            let desc_start = prefix_width + value_width + spacing.len();
            let remaining = width as i64 - desc_start as i64 - 2;
            if remaining > MIN_DESCRIPTION_WIDTH as i64 {
                let d = truncate_to_width(desc, remaining as usize, "", false);
                if selected {
                    return (self.theme.selected_text)(&format!("{prefix}{value}{spacing}{d}"));
                }
                return format!("{prefix}{value}{}", (self.theme.description)(&format!("{spacing}{d}")));
            }
        }
        let max = width.saturating_sub(prefix_width + 2);
        let value = truncate_to_width(item.display(), max, "", false);
        if selected {
            (self.theme.selected_text)(&format!("{prefix}{value}"))
        } else {
            format!("{prefix}{value}")
        }
    }

    fn primary_column_width(&self) -> usize {
        let raw_min = self
            .layout
            .min_primary
            .or(self.layout.max_primary)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        let raw_max = self
            .layout
            .max_primary
            .or(self.layout.min_primary)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        let min = raw_min.min(raw_max).max(1);
        let max = raw_min.max(raw_max).max(1);
        let widest = self
            .filtered
            .iter()
            .map(|i| visible_width(i.display()) + PRIMARY_COLUMN_GAP)
            .max()
            .unwrap_or(0);
        widest.clamp(min, max)
    }
}
