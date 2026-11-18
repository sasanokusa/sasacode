//! Tokens and cost of the whole sitting, and the summary printed after the UI closes
//! (`summary.ts`).

use indexmap::IndexSet;
use sasacode_ai::format::fmt_tokens;
use sasacode_ai::{AssistantMessage, Usage};
use sasacode_host::t;

use crate::jsfmt::{js_round, to_fixed};
use crate::text::visible_width;
use crate::theme::{bold, cyan, gray};

/// Tokens and cost of everything sent since sasacode started, across /clear and /resume.
#[derive(Debug, Clone)]
pub struct UsageTally {
    pub requests: u64,
    /// Of the requests, those outside the conversation: subagents, side requests, discarded replies.
    pub side: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: f64,
    pub models: IndexSet<String>,
    pub started_at: i64,
}

impl Default for UsageTally {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageTally {
    pub fn new() -> Self {
        Self {
            requests: 0,
            side: 0,
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cost: 0.0,
            models: IndexSet::new(),
            started_at: sasacode_ai::now_ms(),
        }
    }

    pub fn add(&mut self, m: &AssistantMessage) {
        self.add_usage(&format!("{}/{}", m.provider, m.model), &m.usage, false);
    }

    pub fn add_usage(&mut self, model: &str, u: &Usage, side: bool) {
        self.requests += 1;
        if side {
            self.side += 1;
        }
        self.input += u.input;
        self.output += u.output;
        self.cache_read += u.cache_read;
        self.cache_write += u.cache_write;
        self.cost += u.cost;
        self.models.insert(model.to_string());
    }

    /// Share of prompt tokens served from the cache.
    pub fn hit_rate(&self) -> f64 {
        let prompt = self.input + self.cache_read + self.cache_write;
        if prompt == 0 {
            0.0
        } else {
            self.cache_read as f64 / prompt as f64
        }
    }
}

pub fn fmt_duration(ms: i64) -> String {
    let s = js_round(ms as f64 / 1000.0) as i64;
    if s < 60 {
        return t("{s}秒", &[("s", &s)]);
    }
    let m = s / 60;
    if m < 60 {
        return t("{m}分{s}秒", &[("m", &m), ("s", &(s % 60))]);
    }
    t("{h}時間{m}分", &[("h", &(m / 60)), ("m", &(m % 60))])
}

/// A gray label padded to 12 columns (by display width: Japanese labels are two a character).
fn label(s: &str) -> String {
    gray(&format!(
        "{s}{}",
        " ".repeat(12usize.saturating_sub(visible_width(s)).max(1))
    ))
}

/// The lines for /usage and the exit summary.
pub fn usage_lines(u: &UsageTally) -> Vec<String> {
    let mut tokens = vec![
        t("入力 {n}", &[("n", &fmt_tokens(u.input))]),
        t("出力 {n}", &[("n", &fmt_tokens(u.output))]),
        t("キャッシュ読込 {n}", &[("n", &fmt_tokens(u.cache_read))])
            + &if u.cache_read > 0 {
                format!("（{}%）", js_round(u.hit_rate() * 100.0))
            } else {
                String::new()
            },
    ];
    if u.cache_write > 0 {
        tokens.push(t("キャッシュ書込 {n}", &[("n", &fmt_tokens(u.cache_write))]));
    }
    let mut lines = vec![format!("{}{}", label(&t("トークン", &[])), tokens.join(&gray(" · ")))];
    let side = if u.side > 0 {
        gray(&t("（うちサブエージェントなど {n} 回）", &[("n", &u.side)]))
    } else {
        String::new()
    };
    let cost = if u.cost > 0.0 {
        format!("{}${}", gray(" · "), to_fixed(u.cost, 3))
    } else {
        String::new()
    };
    lines.push(format!(
        "{}{}{side}{cost}",
        label(&t("リクエスト", &[])),
        t("{n} 回", &[("n", &u.requests)])
    ));
    if !u.models.is_empty() {
        let models: Vec<&str> = u.models.iter().map(String::as_str).collect();
        lines.push(format!("{}{}", label(&t("モデル", &[])), models.join(", ")));
    }
    lines
}

/// Printed on the normal screen after the UI closes: what was used and how to come back.
pub fn exit_summary(u: &UsageTally, session_id: Option<&str>) -> String {
    let elapsed = fmt_duration(sasacode_ai::now_ms() - u.started_at);
    let head = format!(
        "{} {}",
        bold("sasacode"),
        gray(&t("を終了しました · {time}", &[("time", &elapsed)]))
    );
    let mut lines = vec![head];
    if u.requests > 0 {
        lines.extend(usage_lines(u));
    }
    if let Some(id) = session_id {
        lines.push(format!(
            "{}{}",
            label(&t("再開", &[])),
            cyan(&format!("sasacode -r {id}"))
        ));
    }
    let body: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| if i > 0 { format!("  {l}") } else { l.clone() })
        .collect();
    format!("{}\n", body.join("\n"))
}
