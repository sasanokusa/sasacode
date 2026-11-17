//! What the conversations used, as `/usage` counts it: every model response and every response
//! outside a conversation (a subagent, a side request, a discarded reply), read from the logs of
//! every folder or of this one, by local day. Today, the last 7 and 30 days and all of it; a
//! graph of the days, as GitHub draws contributions; the last days one by one; each model; and
//! the conversations that used the most.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone};
use sasacode_protocol::{Message, SessionEntry, Usage};

/// How many days the history lists, and how many conversations the screen names.
pub const HISTORY: usize = 14;
pub const TOP: usize = 8;

/// What responses used, added up.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sum {
    pub tokens: u64,
    pub output: u64,
    pub requests: u64,
    pub cost: f64,
}

impl Sum {
    fn add(&mut self, u: &Usage) {
        self.tokens += u.input + u.output + u.cache_read + u.cache_write;
        self.output += u.output;
        self.requests += 1;
        self.cost += u.cost;
    }
}

/// A model, what it used, and the last day it was used.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelUse {
    pub model: String,
    pub sum: Sum,
    pub last: NaiveDate,
}

/// A conversation, what it used, and its log.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationUse {
    pub id: String,
    pub first: String,
    pub log: PathBuf,
    pub sum: Sum,
}

/// What the logs used.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// The day it was read on, in the local time zone.
    pub today: NaiveDate,
    /// Today, the last 7 days, the last 30 and all of it.
    pub spans: [Sum; 4],
    pub days: BTreeMap<NaiveDate, Sum>,
    /// The most used first.
    pub models: Vec<ModelUse>,
    pub top: Vec<ConversationUse>,
}

impl Default for Report {
    fn default() -> Self {
        Report {
            today: Local::now().date_naive(),
            spans: [Sum::default(); 4],
            days: BTreeMap::new(),
            models: Vec::new(),
            top: Vec::new(),
        }
    }
}

/// The local day of `ms` milliseconds after 1970.
pub fn day_of(ms: f64) -> Option<NaiveDate> {
    Local.timestamp_millis_opt(ms as i64).single().map(|t| t.date_naive())
}

/// What the logs used, as of `today`.
pub fn report(logs: &[PathBuf], today: NaiveDate) -> Report {
    let mut r = Report {
        today,
        ..Report::default()
    };
    let mut models: BTreeMap<String, ModelUse> = BTreeMap::new();
    let starts = [today, today - Duration::days(6), today - Duration::days(29)];
    for log in logs {
        let Ok(entries) = sasacode_protocol::read_entries(log) else {
            continue;
        };
        let mut this = ConversationUse {
            id: String::new(),
            first: String::new(),
            log: log.clone(),
            sum: Sum::default(),
        };
        // Only messages: a compaction lists earlier ones again.
        let records = entries.iter().filter_map(|e| match e {
            SessionEntry::Session { id, .. } => {
                this.id = id.clone();
                None
            }
            SessionEntry::Message {
                message: Message::User { content, .. },
            } => {
                if this.first.is_empty() {
                    this.first = sasacode_protocol::text_of(content)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .to_owned();
                }
                None
            }
            SessionEntry::Message {
                message: Message::Assistant(a),
            } if a.timestamp > 0.0 => {
                let model = if a.provider.is_empty() {
                    a.model.clone()
                } else {
                    format!("{}/{}", a.provider, a.model)
                };
                Some((a.timestamp, model, a.usage.clone()))
            }
            SessionEntry::Usage { usage, t, model, .. } if *t > 0.0 => Some((*t, model.clone(), usage.clone())),
            _ => None,
        });
        let records: Vec<(f64, String, Usage)> = records.collect();
        for (t, model, usage) in records {
            let Some(day) = day_of(t) else {
                continue;
            };
            this.sum.add(&usage);
            r.days.entry(day).or_default().add(&usage);
            r.spans[3].add(&usage);
            for (i, start) in starts.iter().enumerate() {
                if day >= *start {
                    r.spans[i].add(&usage);
                }
            }
            let m = models.entry(model.clone()).or_insert(ModelUse {
                model,
                sum: Sum::default(),
                last: day,
            });
            m.sum.add(&usage);
            m.last = m.last.max(day);
        }
        if this.sum.requests > 0 {
            r.top.push(this);
        }
    }
    r.models = models.into_values().filter(|m| !m.model.is_empty()).collect();
    r.models.sort_by_key(|m| std::cmp::Reverse(m.sum.tokens));
    r.top.sort_by_key(|c| std::cmp::Reverse(c.sum.tokens));
    r.top.truncate(TOP);
    r
}

/// Every log under `home`, of every folder, or of `cwd`'s only.
pub fn logs(home: &Path, cwd: Option<&Path>) -> Vec<PathBuf> {
    let dirs: Vec<PathBuf> = match cwd {
        Some(cwd) => vec![sasacode_protocol::session_dir(home, cwd)],
        None => std::fs::read_dir(home.join("sessions"))
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
    };
    dirs.iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect()
}

/// The graph's days: weeks as columns, Sunday on top, the last week holding `today`. A day
/// after today is none.
pub fn weeks(today: NaiveDate, n: usize) -> Vec<[Option<NaiveDate>; 7]> {
    let back = today.weekday().num_days_from_sunday() as i64;
    let start = today - Duration::days(back + 7 * (n as i64 - 1));
    (0..n)
        .map(|w| {
            std::array::from_fn(|d| {
                let day = start + Duration::days(w as i64 * 7 + d as i64);
                (day <= today).then_some(day)
            })
        })
        .collect()
}

/// A day's level, 0 to 4: the quartiles of the days with any use, as GitHub does.
pub fn levels(values: &[u64]) -> impl Fn(u64) -> usize {
    let mut used: Vec<u64> = values.iter().copied().filter(|v| *v > 0).collect();
    used.sort_unstable();
    let q = [0.25, 0.5, 0.75].map(|p| {
        used.get((p * (used.len().max(1) - 1) as f64) as usize)
            .copied()
            .unwrap_or(0)
    });
    move |v| match v {
        0 => 0,
        v if v <= q[0] => 1,
        v if v <= q[1] => 2,
        v if v <= q[2] => 3,
        _ => 4,
    }
}

/// The days in a row with any use: the run up to today (or yesterday, when today has none yet),
/// and the longest.
pub fn streaks(days: &BTreeMap<NaiveDate, Sum>, today: NaiveDate) -> (usize, usize) {
    let (mut longest, mut run, mut prev): (usize, usize, Option<NaiveDate>) = (0, 0, None);
    for day in days.keys() {
        run = if prev.is_some_and(|p| *day - p == Duration::days(1)) {
            run + 1
        } else {
            1
        };
        longest = longest.max(run);
        prev = Some(*day);
    }
    let mut day = if days.contains_key(&today) {
        today
    } else {
        today - Duration::days(1)
    };
    let mut current = 0;
    while days.contains_key(&day) {
        current += 1;
        day -= Duration::days(1);
    }
    (current, longest)
}

impl crate::app::App {
    /// The usage screen's messages, and the watch of the log shown; any other comes back.
    pub(crate) fn update_usage(
        &mut self,
        msg: crate::app::Msg,
    ) -> Result<iced::Task<crate::app::Msg>, crate::app::Msg> {
        use crate::app::Msg;
        let task = match msg {
            Msg::WatchLog => {
                self.watch_log();
                iced::Task::none()
            }
            Msg::Usage(here) => {
                self.overlay = Some(crate::app::Overlay::Usage);
                self.usage_here = here;
                self.usage = None;
                let Some(home) = self.home.clone() else {
                    self.usage = Some(Report::default());
                    return Ok(iced::Task::none());
                };
                let cwd = here.then(|| self.cwd.clone());
                iced::Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            report(&logs(&home, cwd.as_deref()), Local::now().date_naive())
                        })
                        .await
                        .unwrap_or_default()
                    },
                    move |r| Msg::UsageRead(here, Box::new(r)),
                )
            }
            Msg::UsageRead(here, report) => {
                if here == self.usage_here {
                    self.usage = Some(*report);
                }
                iced::Task::none()
            }
            other => return Err(other),
        };
        Ok(task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn the_graphs_weeks_end_with_today_sunday_on_top() {
        // 2026-11-17 is a Tuesday.
        let today = date(2026, 11, 17);
        let w = weeks(today, 3);
        assert_eq!(w.len(), 3);
        assert_eq!(w[2][0], Some(date(2026, 11, 15)));
        assert_eq!(w[2][2], Some(today));
        assert_eq!(w[2][3], None);
        assert_eq!(w[0][0], Some(date(2026, 11, 1)));
    }

    #[test]
    fn days_are_graded_by_quartiles_and_runs_of_days_counted() {
        let level = levels(&[0, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!([0, 1, 2, 3, 4, 5, 6, 7, 8].map(&level), [0, 1, 1, 2, 2, 3, 3, 4, 4]);
        let today = date(2026, 11, 17);
        let mut days = BTreeMap::new();
        for d in [10, 11, 12, 15, 16] {
            days.insert(date(2026, 11, d), Sum::default());
        }
        // Today not used yet: the run up to yesterday.
        assert_eq!(streaks(&days, today), (2, 3));
        days.insert(today, Sum::default());
        assert_eq!(streaks(&days, today), (3, 3));
    }

    #[test]
    fn what_the_logs_used_is_added_up_as_usage_does() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-usage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let today = Local::now().date_naive();
        let at = |days_ago: i64| {
            let day = today - Duration::days(days_ago);
            Local
                .from_local_datetime(&day.and_hms_opt(12, 0, 0).unwrap())
                .single()
                .unwrap()
                .timestamp_millis() as f64
        };
        let reply = |model: &str, input: u64, cost: f64, t: f64| {
            serde_json::json!({"type":"message","message":{"role":"assistant","provider":"p","model":model,
                "usage":{"input":input,"output":10,"cost":cost},"timestamp":t,"content":[]}})
        };
        let write = |name: &str, lines: Vec<serde_json::Value>| {
            let path = dir.join(name);
            std::fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
            path
        };
        let head = |id: &str| serde_json::json!({"type":"session","version":1,"id":id,"cwd":"/w","createdAt":"x"});
        let ask = |text: &str| serde_json::json!({"type":"message","message":{"role":"user","content":[{"type":"text","text":text}],"timestamp":1}});
        let a = write(
            "a.jsonl",
            vec![
                head("a"),
                ask("直して"),
                reply("big", 100, 0.5, at(0)),
                reply("small", 50, 0.0, at(10)),
            ],
        );
        let b = write(
            "b.jsonl",
            vec![
                head("b"),
                ask("調べて"),
                reply("small", 10, 0.0, at(40)),
                serde_json::json!({"type":"usage","t":at(0),"model":"p/side","usage":{"input":5,"cost":0.25},"source":"subagent"}),
                // A compaction's copy is not counted again.
                serde_json::json!({"type":"replace","messages":[{"role":"assistant","provider":"p","model":"big","usage":{"input":999},"timestamp":at(0),"content":[]}]}),
            ],
        );
        let r = report(&[a, b], today);
        // Today: big (110) and the subagent (5); a week: the same; a month: and small 10 days ago.
        assert_eq!(r.spans.map(|s| s.tokens), [115, 115, 175, 195]);
        assert_eq!(r.spans[3].requests, 4);
        assert!((r.spans[3].cost - 0.75).abs() < 1e-9);
        assert_eq!(r.models[0].model, "p/big");
        assert_eq!(r.days[&today].requests, 2);
        assert_eq!((r.top[0].id.as_str(), r.top[0].first.as_str()), ("a", "直して"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
