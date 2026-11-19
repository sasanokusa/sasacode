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

use crate::repair::js::{num_setting, utf16_prefix, utf16_suffix};

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

fn clip_middle(text: &str, max_chars: usize) -> Option<String> {
    let total = text.chars().count();
    if total <= max_chars || max_chars < 200 {
        return None;
    }
    let head_len = (max_chars * 60) / 100;
    let tail_len = (max_chars * 35) / 100;
    let omitted = total.saturating_sub(head_len + tail_len);
    let head = utf16_prefix(text, head_len);
    let tail = utf16_suffix(text, tail_len);
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
