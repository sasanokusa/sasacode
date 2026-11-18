//! write-guard (`packages/bundled/src/write-guard.ts`): guards write against the damage small
//! models did in the Gemma4 measurement (#68):
//! - content copied from read with its line numbers: removed when every line has one and they count
//!   up one by one (the model is told); refused when most lines have one but not like that (lines
//!   were dropped or added among them);
//! - an existing file of `minLines` lines or more replaced by less than half as many (a fragment
//!   meant as an edit): refused.
//!
//! A refused write goes through when the model sends the same one again, so a file that really
//! looks like that can be written.
//!
//! settings: { lineNumbers?: boolean, shrink?: boolean (both default true), minLines?: number (default 20) }

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use futures::future::BoxFuture;
use serde_json::Value;

use sasacode_plugin_api::{Decision, PluginApi, ToolCallRawResult, ToolCallResult, hook};

use crate::repair::js::num_setting;

/// read's line-number prefix (`/^ +(\d+)(?:\t| *(?=\r?$))/`): the number padded with spaces, then
/// a tab (gone on a blank line whose trailing tab was trimmed). The padding is required: a TSV whose
/// first column counts 1, 2, 3 … is not read's output. Returns the number and the prefix length.
fn prefix(line: &str) -> Option<(f64, usize)> {
    let b = line.as_bytes();
    let spaces = b.iter().take_while(|&&c| c == b' ').count();
    if spaces == 0 {
        return None;
    }
    let digits = b[spaces..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let end = spaces + digits;
    let n: f64 = line[spaces..end].parse().ok()?;
    if b.get(end) == Some(&b'\t') {
        return Some((n, end + 1));
    }
    let pad = b[end..].iter().take_while(|&&c| c == b' ').count();
    let rest = &line[end + pad..];
    (rest.is_empty() || rest == "\r").then_some((n, end + pad))
}

/// `/^ +\d+\t/`.
fn numbered(line: &str) -> bool {
    let t = line.trim_start_matches(' ');
    t.len() < line.len() && {
        let d = t.bytes().take_while(u8::is_ascii_digit).count();
        d > 0 && t.as_bytes().get(d) == Some(&b'\t')
    }
}

fn rows(s: &str) -> Vec<&str> {
    s.strip_suffix('\n').unwrap_or(s).split('\n').collect()
}

/// `content` without read's line numbers, when every line has one and they count up one by one;
/// else None. At least one has its tab: indented numbers alone (wc -l's output, a column of counts)
/// are the file's text (#92).
pub fn strip_line_numbers(content: &str) -> Option<String> {
    let lines = rows(content);
    let first = prefix(lines[0])?.0;
    if !lines.iter().any(|l| numbered(l))
        || !lines
            .iter()
            .enumerate()
            .all(|(i, l)| prefix(l).is_some_and(|(n, _)| n == first + i as f64))
    {
        return None;
    }
    let body: Vec<&str> = lines
        .iter()
        .map(|l| prefix(l).map_or(*l, |(_, len)| &l[len..]))
        .collect();
    Some(body.join("\n") + if content.ends_with('\n') { "\n" } else { "" })
}

/// Lines as read and write count them: a final newline ends the last line, it does not start one.
fn count(s: &str) -> usize {
    if s.is_empty() { 0 } else { rows(s).len() }
}

fn lock(m: &Mutex<HashMap<String, String>>) -> MutexGuard<'_, HashMap<String, String>> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let line_numbers = settings.get("lineNumbers") != Some(&Value::Bool(false));
        let shrink = settings.get("shrink") != Some(&Value::Bool(false));
        let min_lines = num_setting(&settings, "minLines", 20.0);
        // Per agent: the write refused last, which goes through if it comes again.
        let refused: Arc<Mutex<HashMap<String, String>>> = Arc::default();
        let r = refused.clone();
        api.on::<hook::UserPrompt, _, _>(move |_e, _c| {
            lock(&r).clear();
            async { Ok(None) }
        });

        // After tool-repair (load order): the name and arguments are the repaired ones.
        api.on::<hook::ToolCallRaw, _, _>(move |ev, _c| {
            let out = match ev.input.get("content") {
                Some(Value::String(content)) if ev.name == "write" && line_numbers => {
                    strip_line_numbers(content).map(|plain| {
                        let n = count(content);
                        let mut input = ev.input.clone();
                        input.insert("content".into(), Value::String(plain));
                        ToolCallRawResult {
                            name: None,
                            input: Some(input),
                            note: Some(format!(
                                "removed read's line numbers from content ({n} line{}); write takes the file's text alone",
                                if n == 1 { "" } else { "s" }
                            )),
                        }
                    })
                }
                _ => None,
            };
            async move { Ok(out) }
        });

        let cwd = api.cwd();
        api.on::<hook::ToolCall, _, _>(move |ev, ctx| {
            let refused = refused.clone();
            let cwd = cwd.clone();
            async move {
                let (Some(Value::String(content)), Some(Value::String(path_arg))) =
                    (ev.args.get("content"), ev.args.get("path"))
                else {
                    return Ok(None);
                };
                if ev.tool.name() != "write" {
                    return Ok(None);
                }
                let key = format!("{path_arg}\0{content}");
                let agent = ctx.agent.unwrap_or_default();
                {
                    let mut r = lock(&refused);
                    if r.get(&agent) == Some(&key) {
                        r.remove(&agent);
                        return Ok(None);
                    }
                }
                let deny = |reason: String| {
                    lock(&refused).insert(agent.clone(), key.clone());
                    Ok(Some(ToolCallResult {
                        args: None,
                        decision: Some(Decision::Deny),
                        reason: Some(format!("write-guard: {reason}; if you really mean it, send the same write again")),
                    }))
                };
                let lines = rows(content);
                let with_numbers = lines.iter().filter(|l| numbered(l)).count();
                if line_numbers && with_numbers >= 2 && with_numbers * 2 > lines.len() {
                    return deny(format!(
                        "{with_numbers} of content's {} lines start with a line number as read shows it, but not every line has one counting up by one, so they were not removed and nothing was written. Send the file's text without line numbers",
                        lines.len()
                    ));
                }
                let path = ev.tool.paths(&ev.args, &cwd).and_then(|p| p.into_iter().next()).filter(|p| !p.is_empty());
                let before = match path {
                    Some(p) if shrink => {
                        tokio::fs::read(&p).await.ok().map(|b| String::from_utf8_lossy(&b).into_owned())
                    }
                    _ => None,
                };
                let Some(before) = before else {
                    return Ok(None);
                };
                let (was, now) = (count(&before), count(content));
                if was as f64 >= min_lines && now * 2 < was {
                    return deny(format!(
                        "{path_arg} has {was} lines and this content has {now}; write replaces the whole file, so the rest would be lost and nothing was written. To change part of the file, use edit"
                    ));
                }
                Ok(None)
            }
        });
        Ok(())
    })
}
