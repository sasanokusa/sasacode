//! repetition-guard (`packages/bundled/src/repetition-guard.ts`): stops a response that loops on the
//! same text (common with small or heavily quantized models), trims the repeats, and tells the
//! model. Tool-call arguments get looser limits, since file contents can repeat legitimately.
//!
//! settings: { minRepeats?, minSpan?, minUnit?, shortMinSpan?, toolcallMinRepeats?, toolcallMinSpan?,
//!             toolcallShortMinSpan?, checkEvery?, maxStopsPerRun?, samplingAfterStop?: SamplingOptions }

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use serde_json::Value;

use sasacode_ai::{AssistantContent, SamplingOptions};
use sasacode_plugin_api::{AssistantMessageResult, BeforeRequestResult, DeltaKind, PluginApi, StreamDeltaResult, hook};

use crate::repair::js::{num_setting, utf16_prefix};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repetition {
    /// The repeated unit.
    pub unit: String,
    pub count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RepetitionLimits {
    /// Consecutive copies needed.
    pub min_repeats: f64,
    /// Total characters (UTF-16 units) the copies must cover.
    pub min_span: f64,
    /// Shortest unit for the normal rule (short runs like "-----" are usually intentional).
    pub min_unit: f64,
    /// Units shorter than min_unit ("なるほど。") stop only after covering this many characters.
    pub short_min_span: f64,
}

/// `text.lastIndexOf(needle, from)` on UTF-16 units.
fn last_index_of(text: &[u16], needle: &[u16], from: isize) -> Option<usize> {
    if needle.len() > text.len() {
        return None;
    }
    let from = from.clamp(0, (text.len() - needle.len()) as isize) as usize;
    (0..=from).rev().find(|&i| &text[i..i + needle.len()] == needle)
}

fn starts_with_at(text: &[u16], unit: &[u16], at: usize) -> bool {
    text.len() >= at && text[at..].starts_with(unit)
}

/// Does `text` end with the same unit repeated back to back? The period is found from the nearest
/// earlier occurrence of the last few characters, then copies are counted backwards. Lengths are
/// UTF-16 units, as the TS counted them.
pub fn detect_repetition(text: &str, limits: &RepetitionLimits) -> Option<Repetition> {
    let t: Vec<u16> = text.encode_utf16().collect();
    let len = t.len();
    if (len as f64) < limits.min_span {
        return None;
    }
    let probe = &t[len.saturating_sub(24)..];
    let prev = last_index_of(&t, probe, len as isize - probe.len() as isize - 1)?;
    let period = len - probe.len() - prev;
    let unit: &[u16] = if period == 0 { &t } else { &t[len - period..] };
    let mut count = 1usize;
    while len >= (count + 1) * period && &t[len - (count + 1) * period..len - count * period] == unit {
        count += 1;
        if period == 0 {
            break;
        }
    }
    let span = (count * period) as f64;
    let found = || Repetition {
        unit: String::from_utf16_lossy(unit),
        count,
    };
    if (period as f64) < limits.min_unit {
        return (span >= limits.short_min_span && count >= 20).then(found);
    }
    (count as f64 >= limits.min_repeats && span >= limits.min_span).then(found)
}

/// `text` with the run of `unit` it ends in cut to one copy: the run at the end, not the first two
/// copies anywhere, which for a unit of "\n" was the answer's first paragraph break (#119). A text
/// that does not end in the run is kept.
pub fn trim_run(text: &str, unit: &str) -> String {
    let t: Vec<u16> = text.encode_utf16().collect();
    let u: Vec<u16> = unit.encode_utf16().collect();
    let n = u.len();
    if n == 0 {
        return text.to_string();
    }
    let twice: Vec<u16> = u.iter().chain(u.iter()).copied().collect();
    let Some(at) = last_index_of(&t, &twice, t.len() as isize) else {
        return text.to_string();
    };
    let mut end = at + 2 * n;
    while starts_with_at(&t, &u, end) {
        end += n;
    }
    // The run is not at the end: what follows it was not repeated.
    if !u.starts_with(&t[end.min(t.len())..]) {
        return text.to_string();
    }
    let mut start = at;
    while start >= n && starts_with_at(&t, &u, start - n) {
        start -= n;
    }
    String::from_utf16_lossy(&t[..start + n])
}

const INJECT: &str = "[harness] Your previous response was stopped because it kept repeating the same text. Do not repeat it; continue the task from where it was still useful.";

#[derive(Default)]
struct State {
    /// Per message (by timestamp, as each delta carries a new snapshot) and block: the length last checked.
    last_checked: HashMap<i64, HashMap<usize, usize>>,
    /// Keyed by timestamp: the finished message is not always the same object as the streamed partial.
    found: HashMap<i64, Repetition>,
    stops: u32,
    adjust_next: bool,
}

fn lock(s: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let num = |k: &str, d: f64| num_setting(&settings, k, d);
        let prose = RepetitionLimits {
            min_repeats: num("minRepeats", 4.0),
            min_span: num("minSpan", 200.0),
            min_unit: num("minUnit", 10.0),
            short_min_span: num("shortMinSpan", 400.0),
        };
        let toolcall = RepetitionLimits {
            min_repeats: num("toolcallMinRepeats", 8.0),
            min_span: num("toolcallMinSpan", 2000.0),
            min_unit: num("minUnit", 10.0),
            short_min_span: num("toolcallShortMinSpan", 4000.0),
        };
        let check_every = num("checkEvery", 128.0);
        let max_stops = num("maxStopsPerRun", 2.0);
        let sampling_after_stop: Option<SamplingOptions> = match settings.get("samplingAfterStop") {
            Some(v @ Value::Object(_)) => serde_json::from_value(v.clone()).ok(),
            _ => None,
        };
        let state: Arc<Mutex<State>> = Arc::default();

        let st = state.clone();
        api.on::<hook::UserPrompt, _, _>(move |_e, _c| {
            lock(&st).stops = 0;
            async { Ok(None) }
        });

        let st = state.clone();
        api.on_sync::<hook::StreamDelta, _>(move |ev, _c| {
            let mut s = lock(&st);
            let len = crate::repair::js::utf16_len(&ev.text);
            if s.last_checked.len() > 64 && !s.last_checked.contains_key(&ev.message.timestamp) {
                s.last_checked.clear();
            }
            let seen = s.last_checked.entry(ev.message.timestamp).or_default();
            let before = seen.get(&ev.index).copied().unwrap_or(0);
            // Another message with the same timestamp: its block starts over.
            let before = if len < before { 0 } else { before };
            // Checking every delta would be quadratic; look again only after `check_every` new characters.
            if ((len - before) as f64) < check_every {
                if before == 0 {
                    seen.insert(ev.index, 0);
                }
                return Ok(None);
            }
            seen.insert(ev.index, len);
            let limits = if ev.kind == DeltaKind::Toolcall {
                &toolcall
            } else {
                &prose
            };
            let Some(rep) = detect_repetition(&ev.text, limits) else {
                return Ok(None);
            };
            let kind = match ev.kind {
                DeltaKind::Text => "text",
                DeltaKind::Thinking => "thinking",
                DeltaKind::Toolcall => "toolcall",
            };
            let stop = format!(
                "repeating {} ×{} in {kind}",
                sasacode_ai::json::stringify(&utf16_prefix(&rep.unit, 40)),
                rep.count
            );
            s.found.insert(ev.message.timestamp, rep);
            Ok(Some(StreamDeltaResult { stop: Some(stop) }))
        });

        let st = state.clone();
        let name = api.name().to_string();
        let adjusts = sampling_after_stop.is_some();
        api.on::<hook::AssistantMessage, _, _>(move |ev, _c| {
            let st = st.clone();
            let name = name.clone();
            async move {
                let mut s = lock(&st);
                s.last_checked.remove(&ev.message.timestamp);
                if ev.stopped.as_ref().map(|x| x.plugin.as_str()) != Some(name.as_str()) {
                    return Ok(None);
                }
                s.stops += 1;
                s.adjust_next = adjusts;
                let rep = s.found.remove(&ev.message.timestamp);
                let stops = s.stops;
                drop(s);
                let mut message = ev.message;
                // Keep one copy of the repeated unit. Thinking and tool-call blocks were already dropped by the core.
                if let Some(rep) = rep {
                    for block in &mut message.content {
                        if let AssistantContent::Text(t) = block {
                            t.text = trim_run(&t.text, &rep.unit);
                        }
                    }
                }
                let inject = (f64::from(stops) <= max_stops).then(|| INJECT.to_string());
                Ok(Some(AssistantMessageResult {
                    message: Some(message),
                    retry: false,
                    inject,
                }))
            }
        });

        let st = state;
        api.on::<hook::BeforeRequest, _, _>(move |_e, _c| {
            let sampling = sampling_after_stop.clone();
            let next = {
                let mut s = lock(&st);
                std::mem::take(&mut s.adjust_next)
            };
            async move {
                Ok(if next {
                    Some(BeforeRequestResult {
                        sampling,
                        ..Default::default()
                    })
                } else {
                    None
                })
            }
        });
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_run_keeps_text_that_does_not_end_in_the_run() {
        assert_eq!(trim_run("abab tail", "ab"), "abab tail");
        assert_eq!(trim_run("x ababab", "ab"), "x ab");
        assert_eq!(trim_run("x ababa", "ab"), "x ab");
    }
}
