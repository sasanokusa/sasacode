//! The conversation's outline: each message sent and the headings of the replies to it, found by
//! what is typed in the palette, and the conversation scrolled to the one taken.

use iced::advanced::widget::Id;
use iced::advanced::widget::operation::{Operation, Outcome, Scrollable};
use iced::widget::operation::{self as op, AbsoluteOffset};
use iced::{Rectangle, Task, Vector};

use crate::app::{App, Msg, TIMELINE};
use crate::overlay::Entry;
use crate::transcript::Step;

/// How much of a heading or a message the outline shows.
const SHOWN: usize = 70;
/// Room above the message scrolled to.
const ABOVE: f32 = 12.0;

/// The id of exchange `i`'s place in the conversation.
pub fn exchange_id(i: usize) -> Id {
    Id::from(format!("exchange-{i}"))
}

/// The headings a reply's Markdown has, with their depth.
fn headings(md: &str) -> Vec<(usize, &str)> {
    let mut found = Vec::new();
    let mut fenced = false;
    for line in md.lines() {
        let bare = line.trim_start();
        if bare.starts_with("```") || bare.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let depth = bare.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&depth) && bare[depth..].starts_with(' ') {
            found.push((depth, bare[depth..].trim()));
        }
    }
    found
}

/// Where a container is in the conversation's content: how far below its top.
struct Locate {
    target: Id,
    top: Option<f32>,
    at: Option<f32>,
}

impl Operation<f32> for Locate {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<f32>)) {
        operate(self);
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        _bounds: Rectangle,
        content_bounds: Rectangle,
        _translation: Vector,
        _state: &mut dyn Scrollable,
    ) {
        if id == Some(&Id::new(TIMELINE)) {
            self.top = Some(content_bounds.y);
        }
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&self.target) {
            self.at = Some(bounds.y);
        }
    }

    fn finish(&self) -> Outcome<f32> {
        match (self.top, self.at) {
            (Some(top), Some(at)) => Outcome::Some((at - top - ABOVE).max(0.0)),
            _ => Outcome::None,
        }
    }
}

impl App {
    /// The outline's entries: each message, numbered, and under it its replies' headings.
    pub(crate) fn outline_entries(&self, query: &str) -> Vec<Entry> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut all = Vec::new();
        for (i, ex) in self.chat.transcript.exchanges.iter().enumerate() {
            let first = ex.prompt.lines().find(|l| !l.trim().is_empty()).unwrap_or_default();
            let mut lines = vec![format!("{}. {}", i + 1, crate::view::clip(first.trim(), SHOWN))];
            for step in &ex.steps {
                if let Step::Text(md) = step {
                    for (depth, heading) in headings(&md.raw) {
                        let indent = "  ".repeat(depth.min(3));
                        lines.push(format!("{indent}{}", crate::view::clip(heading, SHOWN)));
                    }
                }
            }
            for label in lines {
                let lower = label.to_lowercase();
                if words.iter().all(|w| lower.contains(w)) {
                    all.push(Entry {
                        label,
                        keys: None,
                        msg: Msg::JumpTo(i),
                    });
                }
            }
        }
        all
    }

    /// The outline's messages; any other comes back.
    pub(crate) fn update_outline(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        match msg {
            Msg::JumpTo(i) => {
                // A search inside the conversation may have hidden it.
                self.find_in = None;
                // Held by its start, so the place scrolled to stays as the conversation grows.
                self.reading = true;
                self.settling = true;
                Ok(iced::advanced::widget::operate(Locate {
                    target: exchange_id(i),
                    top: None,
                    at: None,
                })
                .then(|y| op::scroll_to(TIMELINE, AbsoluteOffset { x: 0.0, y }))
                .chain(Task::done(Msg::Settled)))
            }
            other => Err(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replys_headings_are_found_with_their_depth_but_not_in_code() {
        let md = "# 概要\n本文\n## 手順\n```sh\n# コメント\n```\n#タグ\n### 次";
        assert_eq!(headings(md), vec![(1, "概要"), (2, "手順"), (3, "次")]);
    }
}
