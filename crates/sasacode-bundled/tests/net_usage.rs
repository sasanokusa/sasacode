//! packages/bundled/test/usage.test.ts: /usage's records, graph and plan lines, from session logs
//! in a temp home. The DST case runs in a child test process with TZ set.

mod support_net;

use std::path::Path;

use chrono::{Local, TimeZone};
use serde_json::{Value, json};

use sasacode_bundled::usage::{UsageRecord, by_day, day_key, days_ago, graph, plan_lines, read_records};
use sasacode_plugin_api::Lang;
use support_net::*;

fn plain(s: &str) -> String {
    let re = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap_or_else(|e| panic!("{e}"));
    re.replace_all(s, "").into_owned()
}

/// `new Date("<d>T<h>:00:00").getTime()`: local time.
fn day(d: &str, h: u32) -> i64 {
    let date = chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap_or_else(|e| panic!("{e}"));
    let at = date.and_hms_opt(h, 0, 0).unwrap_or_else(|| panic!("hour {h}"));
    Local
        .from_local_datetime(&at)
        .earliest()
        .unwrap_or_else(|| panic!("no local {d} {h}"))
        .timestamp_millis()
}

/// An assistant message as the session log holds it. The TS fixture had no `api`: the Rust session
/// reader (sasacode-host) skips a message without one, as every real log line has it.
fn assistant(t: i64, input: u64, output: u64, model: &str) -> String {
    json!({ "type": "message", "message": { "role": "assistant", "api": "a", "provider": "p", "model": model, "content": [], "timestamp": t, "stopReason": "stop", "usage": { "input": input, "output": output, "cacheRead": 0, "cacheWrite": 0, "cost": 0.01 } } })
        .to_string()
}

fn message_of(line: &str) -> Value {
    serde_json::from_str::<Value>(line)
        .map(|v| v["message"].clone())
        .unwrap_or(Value::Null)
}

fn write(path: &Path, lines: &[String]) {
    std::fs::write(path, lines.join("\n")).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn mkdir(path: &Path) {
    std::fs::create_dir_all(path).unwrap_or_else(|e| panic!("mkdir {}: {e}", path.display()));
}

/// The two session logs of the first test, under `home`.
fn logs(home: &Path) {
    mkdir(&home.join("sessions").join("proj-a"));
    mkdir(&home.join("sessions").join("proj-b"));
    write(
        &home.join("sessions").join("proj-a").join("s1.jsonl"),
        &[
            json!({ "type": "session", "version": 1, "id": "s1", "cwd": "/a", "createdAt": "" }).to_string(),
            assistant(day("2026-09-24", 12), 100, 10, "m"),
            json!({ "type": "message", "message": { "role": "user", "content": [], "timestamp": day("2026-09-24", 12) } })
                .to_string(),
            json!({ "type": "replace", "messages": [message_of(&assistant(day("2026-09-24", 12), 100, 10, "m"))] })
                .to_string(),
            "{ torn".into(),
        ],
    );
    write(
        &home.join("sessions").join("proj-b").join("s2.jsonl"),
        &[
            assistant(day("2026-09-26", 9), 50, 5, "other"),
            assistant(day("2026-09-26", 18), 50, 5, "other"),
        ],
    );
}

#[test]
fn records_come_from_every_session_log_compactions_replace_entries_are_not_counted_twice() {
    let home = temp_dir("sasacode-usage-");
    logs(home.path());
    let records = read_records(home.path());
    let models: Vec<&str> = records.iter().map(|r| r.model.as_str()).collect();
    assert_eq!(models, ["p/m", "p/other", "p/other"]);
    let days = by_day(&records);
    let d24 = days.get("2026-09-24").copied().unwrap_or_default();
    assert_eq!((d24.tokens, d24.output, d24.requests), (110, 10, 1));
    let d26 = days.get("2026-09-26").copied().unwrap_or_default();
    assert_eq!((d26.tokens, d26.requests), (110, 2));
    assert!(read_records(&home.path().join("nowhere")).is_empty());
}

fn cells(row: &str) -> Vec<u32> {
    let re = regex::Regex::new(r"\x1b\[48;5;(\d+)m {2}").unwrap_or_else(|e| panic!("{e}"));
    re.captures_iter(row).filter_map(|c| c[1].parse().ok()).collect()
}

#[tokio::test]
async fn the_graph_has_a_week_per_column_sunday_on_top_today_in_the_last_column_and_nothing_after_it() {
    let _l = lang(Lang::Ja).await;
    let home = temp_dir("sasacode-usage-");
    logs(home.path());
    let days = by_day(&read_records(home.path()));
    let now = day("2026-09-26", 12); // a Saturday: the last column is full
    let rows = graph(&days, 10, now);
    assert_eq!(rows.len(), 9); // months, 7 days, legend
    assert!(plain(&rows[0]).contains("9月"), "{}", plain(&rows[0]));
    assert_eq!(cells(&rows[7]).len(), 10); // Saturday row
    assert_ne!(cells(&rows[7]).last(), Some(&237)); // today was used
    assert_ne!(cells(&rows[5]).last(), Some(&237)); // Thursday the 24th
    assert_eq!(cells(&rows[1]).last(), Some(&237)); // Sunday the 20th: nothing
    let wednesday = graph(&days, 10, day("2026-09-23", 12));
    assert_eq!(cells(&wednesday[7]).len(), 9); // Saturday the 26th is still ahead
}

#[tokio::test]
async fn plan_windows_show_use_time_to_reset_and_a_warning_at_the_limit() {
    let _l = lang(Lang::Ja).await;
    let now = sasacode_ai::now_ms() as f64;
    let plan = json!({ "plan_type": "plus", "rate_limit": {
        "limit_reached": true,
        "primary_window": { "used_percent": 95, "limit_window_seconds": 18_000, "reset_at": now / 1000.0 + 90.0 * 60.0 },
        "secondary_window": { "used_percent": 40, "limit_window_seconds": 604_800, "reset_at": now / 1000.0 + 3.0 * 86_400.0 },
    } });
    let lines: Vec<String> = plan_lines(&plan, 100, now).iter().map(|l| plain(l)).collect();
    assert_eq!(lines[0], "ChatGPT plus");
    let re = |p: &str| regex::Regex::new(p).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        re(r"5時間の枠.*95% 使用 · あと 1時間30分でリセット").is_match(&lines[1]),
        "{}",
        lines[1]
    );
    assert!(
        re(r"週の枠.*40% 使用 · あと 3日でリセット").is_match(&lines[2]),
        "{}",
        lines[2]
    );
    assert!(lines[3].contains("上限に達しています"), "{}", lines[3]);
}

/// The pre-index algorithm, kept as the reference the index is checked against.
fn full_scan(root: &Path) -> Vec<UsageRecord> {
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(root) else { return out };
    for d in dirs.flatten() {
        let Ok(files) = std::fs::read_dir(d.path()) else {
            continue;
        };
        for f in files.flatten() {
            if !f.file_name().to_string_lossy().ends_with(".jsonl") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(f.path()) else {
                continue;
            };
            for line in text.split('\n') {
                if !line.starts_with(r#"{"type":"message""#) || !line.contains(r#""role":"assistant""#) {
                    continue;
                }
                let m = message_of(line);
                let (Some(usage), Some(t)) = (m.get("usage"), m.get("timestamp").and_then(Value::as_i64)) else {
                    continue;
                };
                if t == 0 {
                    continue;
                }
                let n = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
                out.push(UsageRecord {
                    t,
                    model: format!(
                        "{}/{}",
                        m["provider"].as_str().unwrap_or(""),
                        m["model"].as_str().unwrap_or("")
                    ),
                    input: n("input"),
                    output: n("output"),
                    cache_read: n("cacheRead"),
                    cache_write: n("cacheWrite"),
                    cost: usage.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
                });
            }
        }
    }
    out.sort_by_key(|r| r.t);
    out
}

#[test]
fn usage_served_from_the_index_matches_a_full_scan_of_the_same_logs_before_and_after_a_change() {
    let h = temp_dir("sasacode-usage-scan-");
    let dir = h.path().join("sessions").join("proj");
    mkdir(&dir);
    let one = vec![
        json!({ "type": "session", "version": 1, "id": "s1", "cwd": "/a", "createdAt": "" }).to_string(),
        assistant(day("2026-09-24", 12), 100, 10, "m"),
        json!({ "type": "replace", "messages": [message_of(&assistant(day("2026-09-24", 12), 100, 10, "m"))] })
            .to_string(),
    ];
    // No session header and a torn line: counted by both algorithms alike.
    let two = vec![
        assistant(day("2026-09-25", 8), 50, 5, "other"),
        "{ torn".into(),
        assistant(day("2026-09-25", 20), 7, 3, "other"),
    ];
    write(&dir.join("one.jsonl"), &one);
    write(&dir.join("two.jsonl"), &two);
    let sessions = h.path().join("sessions");
    assert_eq!(read_records(h.path()), full_scan(&sessions));
    // one.jsonl is appended to (so it alone is re-parsed); two.jsonl keeps its indexed values.
    let mut more = one.clone();
    more.push(assistant(day("2026-09-26", 12), 1, 1, "m"));
    write(&dir.join("one.jsonl"), &more);
    let records = read_records(h.path());
    assert_eq!(records, full_scan(&sessions));
    let days = |r: &[UsageRecord]| by_day(r).into_iter().collect::<Vec<_>>();
    assert_eq!(days(&records), days(&full_scan(&sessions)));
}

// chrono's Local follows TZ on unix only (Windows asks the system).
#[cfg(unix)]
#[test]
fn history_days_are_calendar_days_the_day_the_clocks_go_forward_is_not_skipped() {
    // chrono reads TZ for Local: this process's own cannot be changed safely.
    let test = "history_days_are_calendar_days_the_day_the_clocks_go_forward_is_not_skipped";
    if !in_child(test, &[("TZ", "America/New_York".into())]) {
        return;
    }
    let at = chrono::NaiveDate::from_ymd_opt(2026, 3, 9).and_then(|d| d.and_hms_opt(12, 0, 0));
    let at = at.unwrap_or_else(|| panic!("date"));
    let local = Local
        .from_local_datetime(&at)
        .earliest()
        .unwrap_or_else(|| panic!("local"));
    assert_eq!(local.offset().local_minus_utc(), -4 * 3600, "TZ is not New York's here");
    let now = local.timestamp_millis(); // the day after DST began in the US
    let keys: Vec<String> = [3, 2, 1, 0].iter().map(|n| day_key(days_ago(*n, now))).collect();
    assert_eq!(keys, ["2026-03-06", "2026-03-07", "2026-03-08", "2026-03-09"]);
}
