//! slim-profile (opt-in via `plugins.enabled`): trims distractions and oversized single-call
//! outputs for small local models. When enabled, `PluginChoice` also leaves out heavy optional
//! plugins listed in [`SLIM_DROPPED_PLUGINS`] (and Skills unless `dropSkills: false` or explicitly
//! enabled) so fewer tools and instructions compete for small attention/context budgets.
//!
//! settings: { readMaxChars?: number (16000), bashMaxChars?: number (8000), dropSkills?: boolean (true), conciseHint?: boolean (true) }

use futures::future::BoxFuture;
use serde_json::Value;

use sasacode_ai::UserContent;
use sasacode_plugin_api::{PluginApi, SystemPromptResult, ToolResultResult, hook};

use crate::repair::js::num_setting;

/// Plugins skipped automatically when `slim-profile` is enabled (unless explicitly listed in `plugins.enabled`).
pub const SLIM_DROPPED_PLUGINS: &[&str] = &[
    "subagent",
    "todo",
    "goal",
    "web-fetch",
    "web-search",
    "browsr",
    "background-sessions",
    "progress-notes",
];

fn char_offset_from_start(s: &str, max_chars: usize) -> usize {
    s.char_indices().nth(max_chars).map_or(s.len(), |(i, _)| i)
}

fn char_offset_from_end(s: &str, max_chars: usize) -> usize {
    if max_chars == 0 {
        return s.len();
    }
    s.char_indices().rev().nth(max_chars - 1).map_or(0, |(i, _)| i)
}

fn clip_middle(text: &str, max_chars: usize) -> Option<String> {
    let total = text.chars().count();
    if total <= max_chars || max_chars < 200 {
        return None;
    }
    let head_budget = (max_chars * 60) / 100;
    let tail_budget = (max_chars * 35) / 100;

    let head_raw_end = char_offset_from_start(text, head_budget);
    // Cut on a line boundary so complete lines stay intact; keep the char-safe slice only when
    // a single line exceeds the entire head budget.
    let head_end = match text[..head_raw_end].rfind('\n') {
        Some(nl) if nl > 0 => nl,
        _ => head_raw_end,
    };

    let tail_raw_start = char_offset_from_end(text, tail_budget).max(head_end);
    // Skip any partial line at the tail start so prefixes like `1234:ab|` are never cut in half;
    // keep the char-safe slice only if no newline separates it from the final line.
    let tail_start = if tail_raw_start == 0 || text[..tail_raw_start].ends_with('\n') {
        tail_raw_start
    } else if let Some(rel_nl) = text[tail_raw_start..].find('\n')
        && tail_raw_start + rel_nl + 1 < text.len()
    {
        tail_raw_start + rel_nl + 1
    } else {
        tail_raw_start
    };

    let head = &text[..head_end];
    let tail = &text[tail_start..];
    let omitted = total.saturating_sub(head.chars().count() + tail.chars().count());
    Some(format!(
        "{head}\n… [slim-profile: truncated {omitted} of {total} chars to fit small context] …\n{tail}"
    ))
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let read_max = num_setting(&settings, "readMaxChars", 16_000.0).max(500.0) as usize;
        let bash_max = num_setting(&settings, "bashMaxChars", 8_000.0).max(500.0) as usize;
        let concise_hint = settings.get("conciseHint") != Some(&Value::Bool(false));

        if concise_hint {
            api.on::<hook::SystemPrompt, _, _>(|ev, _ctx| async move {
                let prompt = format!(
                    "{}\n\nSmall-model rules:\n- Act immediately with tools instead of describing plans.\n- Read target files before editing, and verify fixes with tests before finishing.",
                    ev.prompt
                );
                Ok(Some(SystemPromptResult { prompt: Some(prompt) }))
            });
        }

        api.on::<hook::ToolResult, _, _>(move |ev, _ctx| async move {
            if !ev.ran {
                return Ok(None);
            }
            let cap = match ev.call.name.as_str() {
                "read" => read_max,
                "bash" => bash_max,
                _ => return Ok(None),
            };
            let mut changed = false;
            let mut next_content = Vec::with_capacity(ev.result.content.len());
            for item in &ev.result.content {
                if let UserContent::Text(tc) = item
                    && let Some(clipped) = clip_middle(&tc.text, cap)
                {
                    changed = true;
                    next_content.push(UserContent::text(clipped));
                } else {
                    next_content.push(item.clone());
                }
            }
            if !changed {
                return Ok(None);
            }
            let mut result = ev.result;
            result.content = next_content;
            Ok(Some(ToolResultResult { result: Some(result) }))
        });

        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_middle_cuts_at_line_boundaries_and_handles_japanese() {
        let lines: Vec<String> = (1..=120)
            .map(|i| format!("{i:04}:ab|日本語テキスト行-{i:03}-テスト内容"))
            .collect();
        let text = lines.join("\n");
        let clipped = clip_middle(&text, 500).expect("should truncate");
        let parts: Vec<&str> = clipped.split('\n').collect();
        let marker_idx = parts
            .iter()
            .position(|l| l.contains("[slim-profile: truncated"))
            .expect("marker line");
        for line in parts[..marker_idx]
            .iter()
            .chain(parts[marker_idx + 1..].iter())
            .filter(|l| !l.is_empty())
        {
            let prefix = line.split('|').next().expect("pipe");
            let (num, hash) = prefix.split_once(':').expect("colon");
            assert_eq!(num.len(), 4, "truncated line prefix in: {line}");
            assert!(num.chars().all(|c| c.is_ascii_digit()), "{line}");
            assert_eq!(hash, "ab", "{line}");
        }
    }

    #[test]
    fn clip_middle_handles_single_long_japanese_line() {
        let single = "あいうえお".repeat(200);
        let clipped = clip_middle(&single, 300).expect("should truncate");
        assert!(clipped.contains("[slim-profile: truncated"));
    }
}
