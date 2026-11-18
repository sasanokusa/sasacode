//! tool-repair (`packages/bundled/src/tool-repair.ts`): repairs tool calls before they are
//! validated: broken JSON, misspelled tool names and other harnesses' names for the built-in tools,
//! renamed arguments ("file" → "path"), and values of the wrong type ("20" → 20). Only unambiguous
//! fixes are made; the core tells the model what was changed. Unknown arguments are dropped only
//! for read-only tools; elsewhere they may carry intent, so validation sends them back to the model.
//!
//! settings: { json?: boolean, names?: boolean, schema?: boolean }  (all default true)

use futures::future::BoxFuture;
use serde_json::Value;

use sasacode_plugin_api::{PluginApi, ToolCallRawResult, ToolKind, hook};

pub use crate::repair::json::repair_json;
pub use crate::repair::schema::{FitOptions, Synonyms, TOOL_SYNONYMS, closest_name, edit_distance, fit_to_schema};

/// Prefixes some models put before function names (`/^(functions|function|tools|tool|default_api|api)[.:/]/i`).
fn strip_name_prefix(name: &str) -> &str {
    for p in ["functions", "function", "tools", "tool", "default_api", "api"] {
        if name.len() > p.len()
            && name.is_char_boundary(p.len())
            && name[..p.len()].eq_ignore_ascii_case(p)
            && matches!(name.as_bytes()[p.len()], b'.' | b':' | b'/')
        {
            return &name[p.len() + 1..];
        }
    }
    name
}

/// Anthropic's text editor tool (str_replace_editor) names its operation in `command`.
fn editor_command(command: &str) -> Option<&'static str> {
    match command {
        "view" => Some("read"),
        "create" => Some("write"),
        "str_replace" => Some("edit"),
        _ => None,
    }
}

/// The tool a model meant by `name`, among `names`: the name itself, the name with a prefix
/// (`functions.read`) or without its MCP prefix (`get_file_contents` for
/// `mcp__github__get_file_contents`), another harness's name for it (`read_file`) or, unless
/// `typos` is false, a typo. None unless exactly one tool fits.
pub fn resolve_tool_name<S: AsRef<str>>(name: &str, names: &[S], typos: bool) -> Option<String> {
    if names.iter().any(|n| n.as_ref() == name) {
        return Some(name.to_string());
    }
    let bare = strip_name_prefix(name);
    if names.iter().any(|n| n.as_ref() == bare) {
        return Some(bare.to_string());
    }
    // An MCP tool's own name means that tool, not the built-in another harness calls the same.
    let suffix = format!("__{bare}");
    let mcp: Vec<&str> = names
        .iter()
        .map(AsRef::as_ref)
        .filter(|n| n.ends_with(&suffix))
        .collect();
    if !mcp.is_empty() {
        return (mcp.len() == 1).then(|| mcp[0].to_string());
    }
    closest_name(bare, names, TOOL_SYNONYMS, typos)
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let on = move |key: &str| settings.get(key) != Some(&Value::Bool(false));
        let (json_on, names_on, schema_on) = (on("json"), on("names"), on("schema"));
        api.on::<hook::ToolCallRaw, _, _>(move |ev, _ctx| async move {
            let mut notes: Vec<String> = Vec::new();
            let mut value = ev.input.clone();
            let mut changed = false;
            let fixed = match &ev.raw_input {
                Some(raw) if json_on => repair_json(raw),
                _ => None,
            };
            if let Some(f) = &fixed {
                value = f.value.clone();
                changed = true;
                notes.push(if f.fixes.is_empty() {
                    "JSON re-parsed".to_string()
                } else {
                    format!("JSON: {}", f.fixes.join(", "))
                });
            }

            let mut tool = ev.tools.iter().find(|t| t.name() == ev.name).cloned();
            let mut new_name: Option<String> = None;
            let names: Vec<String> = ev.tools.iter().map(|t| t.name().to_string()).collect();
            let hit = if tool.is_none() && names_on {
                resolve_tool_name(&ev.name, &names, true)
            } else {
                None
            };
            if let Some(mut hit) = hit {
                // A `command` the tool does not take, naming an editor operation, picks that operation's tool.
                let params = ev
                    .tools
                    .iter()
                    .find(|t| t.name() == hit)
                    .map(|t| t.parameters())
                    .unwrap_or_default();
                let takes_command = params
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some_and(|p| p.contains_key("command"));
                let op = match value.get("command") {
                    Some(Value::String(c)) if !takes_command => editor_command(c),
                    _ => None,
                };
                match op {
                    Some(op) if names.iter().any(|n| n == op) => {
                        let command = value
                            .get("command")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        notes.insert(0, format!("tool \"{}\" (command \"{command}\") → \"{op}\"", ev.name));
                        hit = op.to_string();
                        value.shift_remove("command");
                        changed = true;
                    }
                    _ => notes.insert(0, format!("tool \"{}\" → \"{hit}\"", ev.name)),
                }
                tool = ev.tools.iter().find(|t| t.name() == hit).cloned();
                new_name = Some(hit);
            }
            if ev.raw_input.is_some() && fixed.is_none() {
                return Ok(new_name.map(|n| ToolCallRawResult {
                    name: Some(n),
                    input: None,
                    note: Some(notes.join("; ")),
                }));
            }

            if let Some(tool) = &tool
                && schema_on
            {
                let fitted = fit_to_schema(
                    &value,
                    &tool.parameters(),
                    FitOptions {
                        drop_unknown: tool.kind() == ToolKind::Read,
                    },
                );
                if !fitted.fixes.is_empty() {
                    value = fitted.value;
                    changed = true;
                    notes.extend(fitted.fixes);
                }
            }
            if new_name.is_none() && !changed {
                return Ok(None);
            }
            Ok(Some(ToolCallRawResult {
                name: new_name,
                input: changed.then_some(value),
                note: Some(notes.join("; ")),
            }))
        });
        Ok(())
    })
}
