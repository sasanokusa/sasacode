//! /usage (`packages/bundled/src/usage.ts`): what this run used, what the ChatGPT plan has left,
//! and a record of every session on this machine (read back through the session index), with a
//! contributions graph by day.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Datelike, Days, Local, NaiveDate, NaiveDateTime, Offset, TimeZone};
use futures::future::BoxFuture;
use indexmap::{IndexMap, IndexSet};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use sasacode_ai::{Fetch, fmt_tokens};
use sasacode_host::{Lang, SessionUsage, current_lang, load_session_index, t};
use sasacode_plugin_api::{Command, CommandContext, NotifyLevel, PluginApi, SelectOption, hook, sasacode_home};

use crate::net_util::{js_num_str, js_round, js_trim, js_words, to_fixed, utf16_len};
use crate::openai_codex::auth::{codex_tokens, read_codex_auth};

/// One model response: epoch ms, `provider/model`, its token counts and cost.
pub type UsageRecord = SessionUsage;

fn tokens(r: &UsageRecord) -> u64 {
    r.input + r.output + r.cache_read + r.cache_write
}

/// Every model response recorded in the session logs under `home`, sorted by time. Served from the
/// session index, so this costs one stat per log and re-parses only logs that changed, not every
/// log in full. Subagent runs, side requests (compaction summaries, the agent-mode judge) and
/// discarded replies count too; a log whose header was torn away still does.
pub fn read_records(home: &Path) -> Vec<UsageRecord> {
    let mut out: Vec<UsageRecord> = load_session_index(&home.join("sessions"))
        .into_values()
        .flat_map(|s| s.usage)
        .collect();
    out.sort_by_key(|r| r.t);
    out
}

// ── aggregation ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sum {
    pub tokens: u64,
    pub output: u64,
    pub requests: u64,
    pub cost: f64,
}

impl Sum {
    fn add(&mut self, r: &UsageRecord) {
        self.tokens += tokens(r);
        self.output += r.output;
        self.requests += 1;
        self.cost += r.cost;
    }
}

/// The local calendar day of epoch ms `t`.
fn local_date(t: i64) -> NaiveDate {
    match Local.timestamp_millis_opt(t).earliest() {
        Some(d) => d.date_naive(),
        None => chrono::DateTime::from_timestamp_millis(t)
            .map(|d| d.date_naive())
            .unwrap_or_default(),
    }
}

/// Epoch ms of a local wall-clock time; one the clocks skip is read with the offset before the gap
/// (later, as JS's Date does).
fn local_ms(at: NaiveDateTime) -> i64 {
    match Local.from_local_datetime(&at).earliest() {
        Some(d) => d.timestamp_millis(),
        None => {
            let before = at - chrono::Duration::hours(3);
            let offset = Local
                .offset_from_local_datetime(&before)
                .earliest()
                .map(|o| o.fix().local_minus_utc())
                .unwrap_or(0);
            at.and_utc().timestamp_millis() - i64::from(offset) * 1000
        }
    }
}

fn midnight(d: NaiveDate) -> i64 {
    local_ms(d.and_time(chrono::NaiveTime::MIN))
}

fn date_key(d: NaiveDate) -> String {
    format!("{}-{:02}-{:02}", d.year(), d.month(), d.day())
}

/// The local day of epoch ms `t`, `YYYY-MM-DD`.
pub fn day_key(t: i64) -> String {
    date_key(local_date(t))
}

pub fn by_day(records: &[UsageRecord]) -> IndexMap<String, Sum> {
    let mut days: IndexMap<String, Sum> = IndexMap::new();
    for r in records {
        days.entry(day_key(r.t)).or_default().add(r);
    }
    days
}

fn start_of_day(t: i64) -> i64 {
    midnight(local_date(t))
}

fn days_before(d: NaiveDate, n: i64) -> NaiveDate {
    if n >= 0 {
        d.checked_sub_days(Days::new(n.unsigned_abs())).unwrap_or(d)
    } else {
        d.checked_add_days(Days::new(n.unsigned_abs())).unwrap_or(d)
    }
}

/// Midnight `n` calendar days before `now`'s day: a day is not 24 hours where the clocks change (DST).
pub fn days_ago(n: i64, now: i64) -> i64 {
    midnight(days_before(local_date(now), n))
}

fn since(records: &[UsageRecord], t: i64) -> Sum {
    let mut s = Sum::default();
    records.iter().filter(|r| r.t >= t).for_each(|r| s.add(r));
    s
}

// ── formatting ──────────────────────────────────────────────────────

fn gray(s: &str) -> String {
    format!("\x1b[90m{s}\x1b[0m")
}
fn bold(s: &str) -> String {
    format!("\x1b[1m{s}\x1b[0m")
}
/// A day is two colored spaces, about square in a terminal cell grid. Block characters (■, █) would
/// be simpler but are ambiguous-width: two columns in CJK terminals, which breaks the grid.
fn cell(color: u32) -> String {
    format!("\x1b[48;5;{color}m  \x1b[0m")
}
/// Columns per week: a two-column day and a gap.
const WEEK: usize = 3;
const LEVELS: [u32; 5] = [237, 22, 28, 34, 40];

// One format for token counts everywhere (footer, exit summary, /usage).
fn fmt_count(n: u64) -> String {
    fmt_tokens(n)
}
fn fmt_cost(c: f64) -> String {
    if c > 0.0 {
        format!(" · ${}", to_fixed(c, if c < 1.0 { 3 } else { 2 }))
    } else {
        String::new()
    }
}
fn sum_line(s: &Sum) -> String {
    t!(
        "{tokens} トークン（出力 {output}）· {n} 回",
        tokens = fmt_count(s.tokens),
        output = fmt_count(s.output),
        n = s.requests
    ) + &fmt_cost(s.cost)
}
fn en() -> bool {
    current_lang() == Lang::En
}
fn month_name(d: NaiveDate) -> String {
    if en() {
        d.format("%b").to_string()
    } else {
        format!("{}月", d.month())
    }
}
fn weekday_name(d: NaiveDate) -> String {
    if en() {
        d.format("%a").to_string()
    } else {
        let names = ['日', '月', '火', '水', '木', '金', '土'];
        names[d.weekday().num_days_from_sunday() as usize].to_string()
    }
}

/// Display width without escape sequences (Japanese is two columns a character).
fn vis(s: &str) -> usize {
    let mut plain = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find("\x1b[") {
        plain.push_str(&rest[..at]);
        let tail = &rest[at + 2..];
        let end = tail
            .find(|c: char| !(c.is_ascii_digit() || c == ';'))
            .unwrap_or(tail.len());
        if tail[end..].starts_with('m') {
            rest = &tail[end + 1..];
        } else {
            plain.push_str("\x1b[");
            rest = tail;
        }
    }
    plain.push_str(rest);
    plain.chars().filter(|c| !c.is_control()).collect::<String>().width()
}
fn pad(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(vis(s)).max(1)))
}
/// `padStart` / `padEnd` with spaces, counted in UTF-16 units.
fn pad_start(s: &str, n: usize) -> String {
    format!("{}{s}", " ".repeat(n.saturating_sub(utf16_len(s))))
}
fn pad_end(s: &str, n: usize) -> String {
    format!("{s}{}", " ".repeat(n.saturating_sub(utf16_len(s))))
}

fn bar(fraction: f64, width: usize, color: u32) -> String {
    let n = (js_round(fraction.clamp(0.0, 1.0) * width as f64).max(0.0) as usize).min(width);
    format!(
        "\x1b[48;5;{color}m{}\x1b[48;5;237m{}\x1b[0m",
        " ".repeat(n),
        " ".repeat(width - n)
    )
}

fn fmt_wait(ms: f64) -> String {
    let m = js_round(ms / 60_000.0).max(0.0) as u64;
    if m < 60 {
        return t!("{m}分", m = m);
    }
    let h = m / 60;
    if h < 48 {
        return if !m.is_multiple_of(60) {
            t!("{h}時間{m}分", h = h, m = m % 60)
        } else {
            t!("{h}時間", h = h)
        };
    }
    if !h.is_multiple_of(24) {
        t!("{d}日{h}時間", d = h / 24, h = h % 24)
    } else {
        t!("{d}日", d = h / 24)
    }
}

// ── contributions graph ─────────────────────────────────────────────

/// Level 0–4 for each day: quartiles of the days with any use, as GitHub does.
fn levels(values: &[u64]) -> impl Fn(u64) -> usize + use<> {
    let mut used: Vec<u64> = values.iter().copied().filter(|v| *v > 0).collect();
    used.sort_unstable();
    let q: Vec<u64> = [0.25, 0.5, 0.75]
        .iter()
        .map(|p| {
            let at = (p * (used.len() as f64 - 1.0)).floor();
            if at < 0.0 {
                0
            } else {
                used.get(at as usize).copied().unwrap_or(0)
            }
        })
        .collect();
    move |v| {
        if v == 0 {
            0
        } else if v <= q[0] {
            1
        } else if v <= q[1] {
            2
        } else if v <= q[2] {
            3
        } else {
            4
        }
    }
}

/// Weeks as columns (Sunday on top), the last one holding today; month names above.
pub fn graph(days: &IndexMap<String, Sum>, weeks: usize, now: i64) -> Vec<String> {
    let today = local_date(now);
    let back = i64::from(today.weekday().num_days_from_sunday()) + 7 * (weeks as i64 - 1);
    let start = days_before(today, back);
    let grid: Vec<Vec<(String, NaiveDate, bool)>> = (0..weeks)
        .map(|w| {
            (0..7)
                .map(|d| {
                    let date = days_before(start, -((w * 7 + d) as i64));
                    (date_key(date), date, date > today)
                })
                .collect()
        })
        .collect();
    let used = |key: &str| days.get(key).map(|s| s.tokens).unwrap_or(0);
    let all: Vec<u64> = grid.iter().flatten().map(|(k, _, _)| used(k)).collect();
    let level = levels(&all);

    // Month labels over the week in which the month begins, where there is room.
    let mut months = String::new();
    for (w, col) in grid.iter().enumerate() {
        let first = col.iter().find(|c| c.1.day() == 1);
        let label = if w == 0 {
            month_name(grid[0][0].1)
        } else {
            first.map(|c| month_name(c.1)).unwrap_or_default()
        };
        let at = 3 + w * WEEK;
        let width = vis(&months);
        if !label.is_empty() && at >= width + usize::from(width > 0) {
            months += &" ".repeat(at - width);
            months += &label;
        }
    }
    let mut rows = vec![gray(&months)];
    let names = if en() {
        ["", "M", "", "W", "", "F", ""]
    } else {
        ["", "月", "", "水", "", "金", ""]
    };
    for (d, name) in names.iter().enumerate() {
        let mut row = gray(&if name.is_empty() {
            "   ".to_string()
        } else {
            pad(name, 3)
        });
        for col in &grid {
            let (key, _, future) = &col[d];
            if *future {
                row += &" ".repeat(WEEK);
            } else {
                row += &format!("{} ", cell(LEVELS[level(used(key))]));
            }
        }
        rows.push(row.trim_end_matches(crate::net_util::is_js_space).to_string());
    }
    let legend: String = LEVELS.iter().map(|l| format!("{} ", cell(*l))).collect();
    rows.push(format!(
        "{}{legend}{}",
        gray(&format!("   {} ", t!("少"))),
        gray(&t!("多"))
    ));
    rows
}

/// Longest and current runs of days with any use.
fn streaks(days: &IndexMap<String, Sum>, now: i64) -> (u64, u64) {
    let mut keys: Vec<&String> = days.keys().collect();
    keys.sort();
    let (mut longest, mut run) = (0u64, 0u64);
    let mut prev: Option<NaiveDate> = None;
    for k in keys {
        let Ok(d) = NaiveDate::parse_from_str(k, "%Y-%m-%d") else {
            continue;
        };
        run = if prev.is_some_and(|p| (d - p).num_days() == 1) {
            run + 1
        } else {
            1
        };
        longest = longest.max(run);
        prev = Some(d);
    }
    let mut current = 0;
    let mut d = local_date(now);
    if !days.contains_key(&date_key(d)) {
        d = days_before(d, 1); // today not used yet: count up to yesterday
    }
    while days.contains_key(&date_key(d)) {
        current += 1;
        d = days_before(d, 1);
    }
    (current, longest)
}

// ── the ChatGPT plan ────────────────────────────────────────────────

/// What the plan has left (`Plan`: plan_type, rate_limit, credits), from the endpoint Codex's
/// /status reads. None when not signed in.
pub async fn codex_plan(fetch: Option<&Arc<dyn Fetch>>) -> anyhow::Result<Option<Value>> {
    if read_codex_auth().is_none() {
        return Ok(None);
    }
    let Some(tokens) = codex_tokens().await? else {
        return Ok(None);
    };
    let Some(account) = tokens.account_id.clone() else {
        return Ok(None);
    };
    let req = sasacode_ai::http::client()
        .get("https://chatgpt.com/backend-api/wham/usage")
        .header("Authorization", format!("Bearer {}", tokens.access))
        .header("chatgpt-account-id", account)
        .header("originator", "sasacode")
        .build()?;
    let res = tokio::time::timeout(Duration::from_secs(10), sasacode_ai::send(fetch, req))
        .await
        .map_err(|_| anyhow::anyhow!("The operation timed out."))??;
    if !res.status().is_success() {
        anyhow::bail!("ChatGPT returned {}", res.status().as_u16());
    }
    Ok(Some(res.json().await?))
}

fn window_name(s: Option<f64>) -> String {
    match s.filter(|s| *s != 0.0 && !s.is_nan()) {
        None => t!("枠"),
        Some(604_800.0) => t!("週の枠"),
        Some(s) if s % 86_400.0 == 0.0 => t!("{n}日の枠", n = js_num_str(s / 86_400.0)),
        Some(s) => t!("{n}時間の枠", n = js_num_str(js_round(s / 3600.0))),
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// The plan's windows as lines `width` columns wide, at `now` (epoch ms).
pub fn plan_lines(plan: &Value, width: i64, now: f64) -> Vec<String> {
    let plan_type = plan.get("plan_type").and_then(Value::as_str).unwrap_or("");
    let mut lines = vec![bold(js_trim(&format!("ChatGPT {plan_type}")))];
    let limit = plan.get("rate_limit");
    let windows: Vec<&Value> = ["primary_window", "secondary_window"]
        .iter()
        .filter_map(|k| limit.and_then(|l| l.get(*k)))
        .filter(|w| truthy(Some(w)))
        .collect();
    for w in &windows {
        let used = w.get("used_percent").and_then(Value::as_f64).unwrap_or(0.0);
        let color = if used >= 90.0 {
            160
        } else if used >= 70.0 {
            178
        } else {
            34
        };
        let reset_at = w
            .get("reset_at")
            .and_then(Value::as_f64)
            .filter(|r| *r != 0.0 && !r.is_nan());
        let reset = reset_at
            .map(|r| t!("あと {time}でリセット", time = fmt_wait(r * 1000.0 - now)))
            .unwrap_or_default();
        let bar_width = (width - 50).clamp(8, 20) as usize;
        let head = format!(
            "  {}{} {}",
            pad(&window_name(w.get("limit_window_seconds").and_then(Value::as_f64)), 12),
            bar(used / 100.0, bar_width, color),
            t!("{n}% 使用", n = js_num_str(used))
        );
        lines.push(if reset.is_empty() {
            head
        } else if (vis(&head) + utf16_len(&reset) * 2 + 3) as i64 <= width {
            format!("{head}{}", gray(&format!(" · {reset}")))
        } else {
            format!("{head}\n{}{}", " ".repeat(14), gray(&reset))
        });
    }
    if windows.is_empty() {
        lines.push(gray(&format!("  {}", t!("上限の情報がありません"))));
    }
    if truthy(limit.and_then(|l| l.get("limit_reached"))) {
        lines.push(format!("  \x1b[31m{}\x1b[0m", t!("上限に達しています")));
    }
    let credits = plan.get("credits");
    if truthy(credits.and_then(|c| c.get("has_credits"))) {
        let left = if truthy(credits.and_then(|c| c.get("unlimited"))) {
            t!("無制限")
        } else {
            match credits.and_then(|c| c.get("balance")) {
                None | Some(Value::Null) => "?".to_string(),
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.as_f64().map(js_num_str).unwrap_or_else(|| v.to_string()),
            }
        };
        lines.push(format!("  {}{left}", pad(&t!("クレジット"), 12)));
    }
    lines
}

// ── plugin ──────────────────────────────────────────────────────────

fn subcommands() -> Vec<SelectOption> {
    let option = |value: &str, description: String| SelectOption {
        value: value.into(),
        label: value.into(),
        description: Some(description),
    };
    vec![
        option("graph", t!("日ごとの使用量（Contributions Graph 風）")),
        option("history", t!("日別の記録（既定: 直近14日）")),
        option("models", t!("モデル別の合計")),
        option("plan", t!("ChatGPT プランの残り")),
    ]
}

/// The terminal's width less a margin (80 columns when stdout is not a terminal).
fn width() -> i64 {
    use std::io::IsTerminal;
    let columns = if std::io::stdout().is_terminal() {
        crossterm::terminal::size()
            .map(|(c, _)| i64::from(c))
            .ok()
            .filter(|c| *c > 0)
            .unwrap_or(80)
    } else {
        80
    };
    (columns - 5).max(40)
}

/// What this run used: since start, every response and the models they came from.
struct Run {
    started: i64,
    sum: Sum,
    models: IndexSet<String>,
}

struct Usage {
    api: PluginApi,
    run: Mutex<Run>,
}

impl Usage {
    fn show(&self, lines: &[String]) {
        self.api.ui().notify(&lines.join("\n"), NotifyLevel::Info);
    }

    async fn overview(&self) {
        let records = read_records(&sasacode_home());
        let now = sasacode_ai::now_ms();
        let today = start_of_day(now);
        let (started, sum, models) = {
            let run = self.run.lock().unwrap_or_else(|e| e.into_inner());
            (run.started, run.sum, run.models.iter().cloned().collect::<Vec<_>>())
        };
        let mut lines = vec![
            bold(&t!("この起動（{time}）", time = fmt_wait((now - started) as f64))),
            format!("  {}", sum_line(&sum)),
        ];
        if !models.is_empty() {
            lines.push(gray(&format!("  {}", models.join(", "))));
        }
        lines.push(String::new());
        lines.push(bold(&t!("これまで")));
        let spans = [
            (t!("今日"), today),
            (t!("直近7日"), days_ago(6, now)),
            (t!("直近30日"), days_ago(29, now)),
            (t!("全期間"), 0),
        ];
        let label_width = spans.iter().map(|(l, _)| vis(l)).max().unwrap_or(0) + 2;
        for (label, from) in &spans {
            lines.push(format!(
                "  {}{}",
                pad(label, label_width),
                sum_line(&since(&records, *from))
            ));
        }
        if let Ok(Some(plan)) = codex_plan(None).await {
            lines.push(String::new());
            lines.extend(plan_lines(&plan, width(), sasacode_ai::now_ms() as f64));
        }
        lines.push(String::new());
        lines.push(gray(&t!(
            "/usage graph · /usage history [日数] · /usage models · /usage plan"
        )));
        self.show(&lines);
    }

    fn graph_view(&self) {
        let now = sasacode_ai::now_ms();
        let days = by_day(&read_records(&sasacode_home()));
        let weeks = ((width() - 3).max(0) as usize / WEEK).clamp(4, 53);
        let from = days_ago(weeks as i64 * 7 - 1, now);
        let mut in_range: Vec<(&String, &Sum)> = days
            .iter()
            .filter(|(k, _)| {
                NaiveDate::parse_from_str(k, "%Y-%m-%d")
                    .map(midnight)
                    .is_ok_and(|t| t >= from)
            })
            .collect();
        let total: u64 = in_range.iter().map(|(_, v)| v.tokens).sum();
        in_range.sort_by_key(|b| std::cmp::Reverse(b.1.tokens));
        let top = in_range
            .first()
            .map(|(day, s)| {
                format!(
                    " · {}",
                    t!("最多 {day}（{tokens}）", day = day, tokens = fmt_count(s.tokens))
                )
            })
            .unwrap_or_default();
        let (current, longest) = streaks(&days, now);
        let mut lines = vec![bold(&t!("過去 {n} 週の使用量", n = weeks))];
        lines.extend(graph(&days, weeks, now));
        lines.push(String::new());
        lines.push(format!(
            "  {}{top}",
            t!(
                "合計 {tokens} トークン · 使った日 {n} 日",
                tokens = fmt_count(total),
                n = in_range.len()
            )
        ));
        lines.push(gray(&format!(
            "  {}",
            t!(
                "連続 {current} 日（最長 {longest} 日）",
                current = current,
                longest = longest
            )
        )));
        self.show(&lines);
    }

    fn history_view(&self, arg: &str) {
        let now = sasacode_ai::now_ms();
        let asked = crate::net_util::js_number(&Value::String(arg.to_string()));
        let asked = if asked.is_nan() || asked == 0.0 { 14.0 } else { asked };
        let n = asked.clamp(1.0, 365.0);
        let days = by_day(&read_records(&sasacode_home()));
        let mut rows: Vec<(String, Sum)> = Vec::new();
        let mut i = n - 1.0;
        while i >= 0.0 {
            let key = day_key(days_ago(i.trunc() as i64, now));
            let sum = days.get(&key).copied().unwrap_or_default();
            rows.push((key, sum));
            i -= 1.0;
        }
        let max = rows.iter().map(|(_, s)| s.tokens).max().unwrap_or(0).max(1);
        let bar_width = (width() - 60).clamp(8, 30) as usize;
        let mut lines = vec![bold(&t!("直近 {n} 日", n = js_num_str(n)))];
        for (k, s) in &rows {
            let date = NaiveDate::parse_from_str(k, "%Y-%m-%d").unwrap_or_default();
            let day = format!(
                "{} {}",
                k.get(5..).unwrap_or(""),
                pad_end(&weekday_name(date), if en() { 3 } else { 1 })
            );
            if s.requests == 0 {
                lines.push(gray(&format!("  {day}  {} -", " ".repeat(bar_width))));
                continue;
            }
            lines.push(format!(
                "  {day}  {} {} {}",
                bar(s.tokens as f64 / max as f64, bar_width, 34),
                pad_start(&fmt_count(s.tokens), 6),
                gray(&(t!("出力 {output} · {n} 回", output = fmt_count(s.output), n = s.requests) + &fmt_cost(s.cost)))
            ));
        }
        let mut total = Sum::default();
        for (_, s) in &rows {
            total.tokens += s.tokens;
            total.output += s.output;
            total.requests += s.requests;
            total.cost += s.cost;
        }
        lines.push(String::new());
        lines.push(format!("  {} {}", t!("合計"), sum_line(&total)));
        self.show(&lines);
    }

    fn models_view(&self) {
        let records = read_records(&sasacode_home());
        let mut models: IndexMap<String, (Sum, i64)> = IndexMap::new();
        for r in &records {
            let m = models.entry(r.model.clone()).or_default();
            m.0.add(r);
            m.1 = r.t;
        }
        let mut list: Vec<(String, (Sum, i64))> = models.into_iter().collect();
        list.sort_by_key(|b| std::cmp::Reverse(b.1.0.tokens));
        let total = list.iter().map(|(_, v)| v.0.tokens).sum::<u64>().max(1);
        let name_width = (list.iter().map(|(k, _)| utf16_len(k)).max().unwrap_or(0).max(10) + 2).min(40);
        let width = width();
        let row = |(k, (s, last)): &(String, (Sum, i64))| {
            let share = js_num_str(js_round(s.tokens as f64 / total as f64 * 100.0));
            let figures = format!(
                "{} {} {}",
                bar(s.tokens as f64 / total as f64, 12, 34),
                pad_start(&fmt_count(s.tokens), 6),
                gray(&format!(
                    "{share}% · {}{} · {}",
                    t!("{n} 回", n = s.requests),
                    fmt_cost(s.cost),
                    t!("最終 {day}", day = day_key(*last).get(5..).unwrap_or(""))
                ))
            );
            let one = format!("  {}{figures}", pad_end(k, name_width));
            // Narrow terminals: the name on its own line rather than wrapped through the bar.
            if vis(&one) as i64 <= width {
                one
            } else {
                format!("  {k}\n    {figures}")
            }
        };
        let mut lines = vec![bold(&t!("モデル別（全期間）"))];
        if list.is_empty() {
            lines.push(gray(&format!("  {}", t!("記録がありません"))));
        } else {
            lines.extend(list.iter().map(row));
        }
        self.show(&lines);
    }

    async fn plan_view(&self) -> anyhow::Result<()> {
        let plan = codex_plan(None).await?;
        match plan {
            Some(plan) => self.show(&plan_lines(&plan, width(), sasacode_ai::now_ms() as f64)),
            None => self.show(&[gray(&t!(
                "ChatGPT プランにログインしていません（sasacode login）。ほかのプロバイダーは残量を返す API がありません"
            ))]),
        }
        Ok(())
    }

    async fn run(&self, args: &str) -> anyhow::Result<()> {
        let words = js_words(args);
        let (sub, arg) = (
            words.first().copied().unwrap_or(""),
            words.get(1).copied().unwrap_or(""),
        );
        match sub {
            "" => self.overview().await,
            "graph" => self.graph_view(),
            "history" => self.history_view(arg),
            "models" => self.models_view(),
            "plan" => return self.plan_view().await,
            _ => {
                let list = subcommands()
                    .into_iter()
                    .map(|s| s.value)
                    .collect::<Vec<_>>()
                    .join(" / ");
                anyhow::bail!(t!("サブコマンドは {list} のいずれかです", list = list));
            }
        }
        Ok(())
    }
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let usage = Arc::new(Usage {
            api: api.clone(),
            run: Mutex::new(Run {
                started: sasacode_ai::now_ms(),
                sum: Sum::default(),
                models: IndexSet::new(),
            }),
        });
        let on_turn = usage.clone();
        api.on::<hook::TurnEnd, _, _>(move |e, _ctx| {
            let m = &e.message;
            let model = format!("{}/{}", m.provider, m.model);
            let record = UsageRecord {
                t: m.timestamp,
                model: model.clone(),
                input: m.usage.input,
                output: m.usage.output,
                cache_read: m.usage.cache_read,
                cache_write: m.usage.cache_write,
                cost: m.usage.cost,
            };
            {
                let mut run = on_turn.run.lock().unwrap_or_else(|p| p.into_inner());
                run.sum.add(&record);
                run.models.insert(model);
            }
            async { Ok(None) }
        });
        let on_spend = usage.clone();
        api.on::<hook::Usage, _, _>(move |s, _ctx| {
            let record = UsageRecord {
                t: s.t,
                model: s.model.clone(),
                input: s.usage.input,
                output: s.usage.output,
                cache_read: s.usage.cache_read,
                cache_write: s.usage.cache_write,
                cost: s.usage.cost,
            };
            {
                let mut run = on_spend.run.lock().unwrap_or_else(|p| p.into_inner());
                run.sum.add(&record);
                run.models.insert(s.model);
            }
            async { Ok(None) }
        });
        let runner = usage.clone();
        let command = Command::new(
            "usage",
            &t!("使用量：この起動・これまでの記録・プランの残り"),
            move |CommandContext { args }| {
                let usage = runner.clone();
                async move { usage.run(&args).await }
            },
        )
        .hint(&t!("[graph | history [日数] | models | plan]"))
        .complete(|prefix: String| async move {
            subcommands()
                .into_iter()
                .filter(|s| s.value.starts_with(&prefix))
                .collect()
        });
        api.register_command(command);
        Ok(())
    })
}
