//! text-tool-calls (`packages/bundled/src/text-tool-calls.ts`). Small models behind servers whose
//! chat template has no tool-call format (llama.cpp and others) sometimes write the call into the
//! reply instead of making it. Such a reply would end the run as a final answer; this plugin turns
//! the calls into real ones, which then run like any other.
//!
//! Only models served from this machine or the local network are looked at (settings.always: every
//! model); a model behind a cloud API makes native calls and writes such text only as an example.
//! Only a reply that ended by itself (stopReason "stop"), made no native call and carries no signed
//! reasoning (a rewritten turn would not replay) is looked at, and only the calls that end its text
//! (see [`extract_text_calls`]). Every name must be a known tool or resolve to one without a typo
//! ([`resolve_tool_name`]); otherwise the reply stays as it was. The arguments go in as written:
//! broken JSON is left in `raw_input` for tool-repair. The original text is recorded in the session
//! (`text_tool_call`), and the model is told with each result that native calls are expected.
//! Outside auto mode, a call that is not a read is asked about first ([`asks_first`]), also where
//! edits mode would run it unasked (a write inside the working directory) or agent mode would judge it.
//!
//! settings: { always?: boolean (default false) }

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use fancy_regex::Regex;
use futures::future::BoxFuture;
use serde_json::{Value, json};

use sasacode_ai::{AssistantContent, StopReason, ToolCall, parse_tool_input};
use sasacode_plugin_api::{
    AssistantMessageResult, Decision, PermissionMode, PermissionResult, PluginApi, Tool, ToolCallRawResult, ToolKind,
    VerdictSource, hook,
};

use crate::repair::js::{js_space, js_trim, utf16_len};
use crate::repair::json::repair_json;
use crate::tool_repair::resolve_tool_name;
use crate::web_search::private_address;

/// A tool call written in the reply text: the tool's name and its arguments' JSON as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextCall {
    pub name: String,
    pub args: String,
}

/// What [`extract_text_calls`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextCalls {
    pub prose: String,
    pub calls: Vec<TextCall>,
    pub skipped: usize,
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    // INFALLIBLE: constant patterns.
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

// Strict shapes only. A closing tag may be missing at the very end (some servers stop on it) or
// before the next call: a body ends at the first `}` followed by a closing tag, the end of the text
// or the next opening tag. An opening tag elsewhere in a body (in a string argument) is content.
/// Qwen / Hermes: <tool_call>{"name": …, "arguments": …}</tool_call>
fn tag() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"<tool_call>\s*(\{[\s\S]*?\})\s*(?:</tool_call>|$|(?=<tool_call>))")
}
/// Llama 3.x: <function=read>{"path": …}</function> (`\w` is ASCII, as in JS).
fn function() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"<function=([A-Za-z0-9_.:-]+)>\s*(\{[\s\S]*?\})\s*(?:</function>|$|(?=<function=))",
    )
}
/// A ```json fence holding nothing but {"name": …, "arguments" | "parameters": …}
fn fence() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"```json[ \t]*\r?\n\s*(\{[\s\S]*?\})\s*```")
}
/// Prose a ```json call may follow: ordinary answers end with JSON examples too, after longer text.
const FENCE_PROSE: usize = 300;

/// {"name", "arguments" | "parameters"}, in either order (Qwen puts the name first, Hermes last).
fn envelope(body: &str) -> Option<TextCall> {
    let v: Option<Value> = match serde_json::from_str::<Value>(body) {
        Ok(v) => Some(v),
        Err(_) => {
            // Broken JSON: cut the arguments out as written, for tool-repair to mend.
            static FIRST: OnceLock<Regex> = OnceLock::new();
            static LAST: OnceLock<Regex> = OnceLock::new();
            let first = re(
                &FIRST,
                r#"^\{\s*"name"\s*:\s*"([^"\\]+)"\s*,\s*"(?:arguments|parameters)"\s*:\s*([\s\S]*?)\s*\}$"#,
            );
            if let Ok(Some(m)) = first.captures(body)
                && let (Some(name), Some(args)) = (m.get(1), m.get(2))
            {
                return Some(TextCall {
                    name: name.as_str().to_string(),
                    args: args.as_str().to_string(),
                });
            }
            let last = re(
                &LAST,
                r#"^\{\s*"(?:arguments|parameters)"\s*:\s*([\s\S]*?)\s*,\s*"name"\s*:\s*"([^"\\]+)"\s*\}$"#,
            );
            if let Ok(Some(m)) = last.captures(body)
                && let (Some(args), Some(name)) = (m.get(1), m.get(2))
            {
                return Some(TextCall {
                    name: name.as_str().to_string(),
                    args: args.as_str().to_string(),
                });
            }
            // Broken around the name as well (Python-style quotes): repaired as a whole.
            repair_json(body).map(|r| Value::Object(r.value))
        }
    };
    let o = match v {
        Some(Value::Object(o)) => o,
        _ => serde_json::Map::new(),
    };
    // Exactly a name and its arguments: a tool definition (it has a description) or other JSON is no call.
    let Some(Value::String(name)) = o.get("name") else {
        return None;
    };
    if o.len() != 2
        || !o
            .keys()
            .all(|k| ["name", "arguments", "parameters"].contains(&k.as_str()))
    {
        return None;
    }
    let args = o
        .get("arguments")
        .filter(|v| !v.is_null())
        .or_else(|| o.get("parameters").filter(|v| !v.is_null()));
    Some(TextCall {
        name: name.clone(),
        args: match args {
            Some(Value::String(s)) => s.clone(),
            Some(v) => sasacode_ai::json::stringify(v),
            None => "{}".to_string(),
        },
    })
}

/// JSON that parses only once a string or bracket is closed for it. A call that ends the reply
/// without its closing tag may have been cut off where a server stopped on a closing tag inside its
/// arguments (an example written into a file): repaired, it ran with what was left (#82). JSON
/// nothing mends is cut off when `unreadable` (no closing tag); with one, it is a call to report (#85).
fn cut_off(json: &str, unreadable: bool) -> bool {
    match repair_json(json) {
        Some(r) => r.fixes.iter().any(|f| f.starts_with("closed")),
        None => unreadable,
    }
}

struct Found {
    start: usize,
    end: usize,
    call: Option<TextCall>,
    fence: bool,
}

/// The tool calls a reply writes as text, when they end it: the matches at the very end of `text`,
/// with nothing but whitespace after and between them. Prose before them stays (`prose`); a call
/// that is followed by more text, such as an example inside an explanation or a code block, is not
/// one, and neither is a call inside a code block left open. A ```json call also needs the prose
/// before it to be short and to hold no other code block. `skipped` counts the readable calls left
/// in the prose outside code blocks and inline code. None when the text does not end with calls, or
/// when one of them cannot be read.
pub fn extract_text_calls(text: &str) -> Option<TextCalls> {
    let mut found: Vec<Found> = Vec::new();
    // Ends the text and its JSON had to be closed: cut off, not a call. With its closing tag too: a
    // server may keep the stop string it stopped on (#136).
    let open = |whole: &fancy_regex::Match, close: &str, json: &str| {
        whole.end() == text.len() && cut_off(json, !whole.as_str().trim_end_matches(js_space).ends_with(close))
    };
    for m in tag().captures_iter(text).flatten() {
        let (Some(whole), Some(body)) = (m.get(0), m.get(1)) else {
            continue;
        };
        let call = if open(&whole, "</tool_call>", body.as_str()) {
            None
        } else {
            envelope(body.as_str())
        };
        found.push(Found {
            start: whole.start(),
            end: whole.end(),
            call,
            fence: false,
        });
    }
    for m in function().captures_iter(text).flatten() {
        let (Some(whole), Some(name), Some(body)) = (m.get(0), m.get(1), m.get(2)) else {
            continue;
        };
        let call = if open(&whole, "</function>", body.as_str()) {
            None
        } else {
            Some(TextCall {
                name: name.as_str().to_string(),
                args: body.as_str().to_string(),
            })
        };
        found.push(Found {
            start: whole.start(),
            end: whole.end(),
            call,
            fence: false,
        });
    }
    for m in fence().captures_iter(text).flatten() {
        let (Some(whole), Some(body)) = (m.get(0), m.get(1)) else {
            continue;
        };
        found.push(Found {
            start: whole.start(),
            end: whole.end(),
            call: envelope(body.as_str()),
            fence: true,
        });
    }
    found.sort_by_key(|f| std::cmp::Reverse(f.end));
    let mut run: Vec<usize> = Vec::new();
    let mut end = text.len();
    let mut i = 0;
    while i < found.len() {
        let f = &found[i];
        if f.end > end {
            // Inside a call already taken (a tag in a string argument).
            i += 1;
            continue;
        }
        if !js_trim(&text[f.end..end]).is_empty() {
            break;
        }
        run.insert(0, i);
        end = f.start;
        i += 1;
    }
    if run.is_empty() || run.iter().any(|&r| found[r].call.is_none()) {
        return None;
    }
    let prose = text[..end].trim_end_matches(js_space);
    let fences = prose.matches("```").count();
    if fences % 2 == 1 || (run.iter().any(|&r| found[r].fence) && (utf16_len(prose) > FENCE_PROSE || fences > 0)) {
        return None;
    }
    // Calls left in the prose, but not examples: one in a code block or `…`, or inside another match.
    static CODE: OnceLock<Regex> = OnceLock::new();
    let code: Vec<(usize, usize)> = re(&CODE, r"```[\s\S]*?```|`[^`\n]+`")
        .find_iter(prose)
        .flatten()
        .map(|m| (m.start(), m.end()))
        .collect();
    let left: Vec<usize> = (i..found.len())
        .filter(|&k| {
            let f = &found[k];
            f.call.is_some() && f.end <= end && !code.iter().any(|&(s, e)| s <= f.start && f.end <= e)
        })
        .collect();
    let skipped = left
        .iter()
        .filter(|&&k| {
            let f = &found[k];
            !found
                .iter()
                .enumerate()
                .any(|(j, o)| j != k && o.start <= f.start && f.end <= o.end)
        })
        .count();
    Some(TextCalls {
        prose: prose.to_string(),
        calls: run.iter().filter_map(|&r| found[r].call.clone()).collect(),
        skipped,
    })
}

/// A server on this machine or the local network: a private address, or a name without a dot or
/// with a local suffix.
pub(crate) fn local_server(base_url: Option<&str>) -> bool {
    let host = url::Url::parse(base_url.unwrap_or(""))
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    let host = host.strip_prefix('[').unwrap_or(&host);
    let host = host.strip_suffix(']').unwrap_or(host);
    if host.is_empty() {
        return false;
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        return private_address(host);
    }
    let lower = host.to_ascii_lowercase();
    !host.contains('.')
        || [".localhost", ".local", ".internal", ".lan"]
            .iter()
            .any(|s| lower.ends_with(s))
}

const ID: &str = "textcall";
const NOTE: &str = "this call was written as text in your reply and was picked up from there; call tools with native tool calls instead";

/// A call picked up from text that is not a read: it may be an example, so the user decides it
/// (agent-judge does not).
pub fn asks_first(call: &ToolCall, tool: &dyn Tool) -> bool {
    call.id.starts_with(ID) && tool.kind() != ToolKind::Read
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let always = api.settings().get("always") == Some(&Value::Bool(true));
        // Per first picked-up call: how many calls earlier in the text did not run.
        let skipped: Arc<Mutex<HashMap<String, usize>>> = Arc::default();

        let (sk, a) = (skipped.clone(), api.clone());
        api.on::<hook::AssistantMessage, _, _>(move |ev, ctx| {
            let (sk, a) = (sk.clone(), a.clone());
            async move {
                let message = ev.message;
                if ev.stopped.is_some() || message.stop_reason != StopReason::Stop {
                    return Ok(None);
                }
                // The main agent's model; a subagent on another provider is not known to be local.
                let model = a.agent().model();
                if !always && !(model.provider == message.provider && local_server(model.base_url.as_deref())) {
                    return Ok(None);
                }
                if message.content.iter().any(|b| match b {
                    AssistantContent::ToolCall(_) => true,
                    AssistantContent::Thinking(t) => t.signature.as_deref().is_some_and(|s| !s.is_empty()),
                    AssistantContent::Text(_) => false,
                }) {
                    return Ok(None);
                }
                let Some(i) = message
                    .content
                    .iter()
                    .rposition(|b| matches!(b, AssistantContent::Text(_)))
                else {
                    return Ok(None);
                };
                let AssistantContent::Text(block) = &message.content[i] else {
                    return Ok(None);
                };
                let Some(found) = extract_text_calls(&block.text) else {
                    return Ok(None);
                };
                // The main agent's tools; a subagent that lacks one gets "Unknown tool" back, as for any call.
                let names: Vec<String> = a.agent().tools().iter().map(|t| t.name().to_string()).collect();
                if found
                    .calls
                    .iter()
                    .any(|c| resolve_tool_name(&c.name, &names, false).is_none())
                {
                    return Ok(None);
                }
                let calls: Vec<ToolCall> = found
                    .calls
                    .iter()
                    .map(|c| {
                        let (input, raw_input) = parse_tool_input(&c.args);
                        let uuid = uuid::Uuid::new_v4().simple().to_string();
                        ToolCall {
                            id: format!("{ID}{}", &uuid[..16]),
                            name: c.name.clone(),
                            input,
                            raw_input,
                        }
                    })
                    .collect();
                if found.skipped > 0 {
                    sk.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(calls[0].id.clone(), found.skipped);
                }
                let mut entry =
                    json!({ "text": block.text, "calls": calls.iter().map(|c| c.id.clone()).collect::<Vec<_>>() });
                if let Some(agent) = &ctx.agent {
                    entry["agent"] = json!(agent);
                }
                a.session().append("text_tool_call", entry)?;
                let mut message = message.clone();
                if found.prose.is_empty() {
                    message.content.remove(i);
                } else if let AssistantContent::Text(t) = &mut message.content[i] {
                    t.text = found.prose;
                }
                message
                    .content
                    .extend(calls.into_iter().map(AssistantContent::ToolCall));
                message.stop_reason = StopReason::ToolUse;
                Ok(Some(AssistantMessageResult {
                    message: Some(message),
                    retry: false,
                    inject: None,
                }))
            }
        });

        let sk = skipped;
        api.on::<hook::ToolCallRaw, _, _>(move |ev, _c| {
            let out = ev.call.id.starts_with(ID).then(|| {
                // tool-repair (loaded before) has already renamed it; without tool-repair this does.
                let names: Vec<String> = ev.tools.iter().map(|t| t.name().to_string()).collect();
                let rename = if names.contains(&ev.name) {
                    None
                } else {
                    resolve_tool_name(&ev.name, &names, false)
                };
                let n = sk
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&ev.call.id)
                    .unwrap_or(0);
                let note = if n > 0 {
                    format!(
                        "{NOTE}; {n} other call{} written earlier in the text did not run",
                        if n > 1 { "s" } else { "" }
                    )
                } else {
                    NOTE.to_string()
                };
                // Still broken JSON: validation reports it, with the note.
                ToolCallRawResult {
                    name: rename,
                    input: None,
                    note: Some(note),
                }
            });
            async move { Ok(out) }
        });

        // What the mode itself decides becomes a question: edits mode would run a write inside the
        // working directory unasked, and in agent mode agent-judge leaves the call to the user
        // (asks_first). auto mode, rules and other plugins' decisions stay as they are.
        api.on::<hook::Permission, _, _>(|ev, _c| {
            let out = (asks_first(&ev.call, ev.tool.as_ref())
                && ev.mode != PermissionMode::Auto
                && ev.verdict.source == VerdictSource::Mode)
                .then(|| PermissionResult {
                    decision: Some(Decision::Ask),
                    reason: Some("a tool call written as text in the reply, which may be an example".to_string()),
                });
            async move { Ok(out) }
        });
        Ok(())
    })
}
