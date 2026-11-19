//! context-slim: KV-cache-friendly observation masking without LLM calls.
//! Replaces old, oversized `read` / `bash` / tool outputs with compact placeholders in step batches
//! (`maskStep`) once estimated tokens cross `triggerRatio` of the context window, keeping the prefix
//! byte-identical between batch boundaries so local servers (Ollama / llama.cpp) keep their prompt cache.
//!
//! settings: { triggerRatio?: number (0.55), keepRecent?: number (6), maskStep?: number (4), minChars?: number (1200) }

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use serde_json::Value;

use sasacode_ai::{Message, ToolCall, ToolResultMessage, UserContent};
use sasacode_plugin_api::{BeforeRequestResult, PluginApi, hook};

use crate::compaction::estimate;
use crate::repair::js::{num_setting, utf16_prefix, utf16_suffix};

#[derive(Default, Clone, PartialEq, Eq)]
struct FirstSig {
    len: usize,
    first_ts: Option<i64>,
    first_head: String,
}

#[derive(Default)]
struct AgentCache {
    masked_up_to: usize,
    last: FirstSig,
}

fn lock(m: &Mutex<HashMap<String, AgentCache>>) -> std::sync::MutexGuard<'_, HashMap<String, AgentCache>> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn first_sig(messages: &[Message]) -> FirstSig {
    let first = messages.first();
    FirstSig {
        len: messages.len(),
        first_ts: first.map(Message::timestamp),
        first_head: first.map(|m| utf16_prefix(&m.text(), 64)).unwrap_or_default(),
    }
}

fn tool_text(t: &ToolResultMessage) -> String {
    t.content
        .iter()
        .filter_map(UserContent::as_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn find_call<'a>(messages: &'a [Message], tool_call_id: &str, tool_name: &str) -> Option<&'a ToolCall> {
    for m in messages.iter().rev() {
        let Message::Assistant(a) = m else { continue };
        if let Some(c) = a.tool_calls().find(|c| {
            (!tool_call_id.is_empty() && c.id == tool_call_id) || (tool_call_id.is_empty() && c.name == tool_name)
        }) {
            return Some(c);
        }
    }
    None
}

fn arg_hint(messages: &[Message], idx: usize, t: &ToolResultMessage) -> String {
    let Some(call) = find_call(&messages[..idx], &t.tool_call_id, &t.tool_name) else {
        return String::new();
    };
    for key in ["path", "command", "query", "pattern", "url", "name"] {
        if let Some(val) = call.input.get(key).and_then(Value::as_str)
            && !val.trim().is_empty()
        {
            let short = utf16_prefix(val.trim(), 60);
            let ell = if val.trim().chars().count() > 60 { "…" } else { "" };
            return format!("({short}{ell})");
        }
    }
    String::new()
}

fn mask_tool_message(messages: &[Message], idx: usize, min_chars: usize) -> Option<Message> {
    let Message::Tool(t) = &messages[idx] else {
        return None;
    };
    let text = tool_text(t);
    let char_len = text.chars().count();
    if char_len < min_chars || text.starts_with("[context-slim:") {
        return None;
    }
    let replacement = if t.is_error {
        if char_len <= 600 {
            return None;
        }
        let head = utf16_prefix(&text, 300);
        let tail = utf16_suffix(&text, 200);
        let omitted = char_len.saturating_sub(500);
        format!("{head}\n… [context-slim: omitted {omitted} chars of error output] …\n{tail}")
    } else {
        let lines = text.lines().count();
        let hint = arg_hint(messages, idx, t);
        format!(
            "[context-slim: omitted {lines} lines ({char_len} chars) of {}{hint}; re-run or re-read if needed]",
            t.tool_name
        )
    };
    let mut next = t.clone();
    let mut new_content: Vec<UserContent> = t
        .content
        .iter()
        .filter(|c| !matches!(c, UserContent::Text(_)))
        .cloned()
        .collect();
    new_content.insert(0, UserContent::text(replacement));
    next.content = new_content;
    Some(Message::Tool(next))
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let trigger_ratio = num_setting(&settings, "triggerRatio", 0.55).clamp(0.1, 0.95);
        let keep_recent = (num_setting(&settings, "keepRecent", 6.0) as usize).max(1);
        let mask_step = (num_setting(&settings, "maskStep", 4.0) as usize).max(1);
        let min_chars = (num_setting(&settings, "minChars", 1200.0) as usize).max(200);
        let caches: Arc<Mutex<HashMap<String, AgentCache>>> = Arc::default();

        let c = caches.clone();
        api.on::<hook::SessionStart, _, _>(move |_e, _ctx| {
            lock(&c).clear();
            async { Ok(None) }
        });

        let c = caches;
        api.on::<hook::BeforeRequest, _, _>(move |ev, ctx| {
            let caches = c.clone();
            async move {
                let agent_key = ctx.agent.unwrap_or_default();
                let sig = first_sig(&ev.messages);
                let limit = ev.messages.len().saturating_sub(keep_recent);

                let mut prev_boundary = {
                    let mut map = lock(&caches);
                    let entry = map.entry(agent_key.clone()).or_default();
                    let reset = ev.messages.len() < entry.last.len
                        || entry.last.first_ts != sig.first_ts
                        || entry.last.first_head != sig.first_head;
                    if reset {
                        entry.masked_up_to = 0;
                    }
                    entry.masked_up_to.min(limit)
                };

                let threshold = ev.model.context_window as f64 * trigger_ratio;
                let mut current = ev.messages.clone();
                let mut masked_any = false;
                for (i, msg) in current.iter_mut().enumerate().take(prev_boundary) {
                    if let Some(m) = mask_tool_message(&ev.messages, i, min_chars) {
                        *msg = m;
                        masked_any = true;
                    }
                }
                let mut tokens: f64 = current.iter().map(estimate).sum();

                while tokens > threshold && prev_boundary < limit {
                    let candidates: Vec<usize> = (prev_boundary..limit)
                        .filter(|&i| mask_tool_message(&ev.messages, i, min_chars).is_some())
                        .take(mask_step)
                        .collect();
                    if candidates.is_empty() {
                        prev_boundary = limit;
                        break;
                    }
                    let next_boundary = candidates[candidates.len() - 1] + 1;
                    for &i in &candidates {
                        if let Some(m) = mask_tool_message(&ev.messages, i, min_chars) {
                            tokens -= estimate(&current[i]);
                            tokens += estimate(&m);
                            current[i] = m;
                            masked_any = true;
                        }
                    }
                    prev_boundary = next_boundary;
                }

                {
                    let mut map = lock(&caches);
                    let entry = map.entry(agent_key).or_default();
                    entry.masked_up_to = prev_boundary;
                    entry.last = sig;
                }

                if !masked_any {
                    return Ok(None);
                }
                Ok(Some(BeforeRequestResult {
                    messages: Some(current),
                    ..BeforeRequestResult::default()
                }))
            }
        });

        Ok(())
    })
}
