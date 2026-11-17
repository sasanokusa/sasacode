//! Deferred tool definitions and the `tool_search` tool (`packages/agent/src/tool-search.ts`, 5.4).

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use sasacode_ai::{JsonObject, Message, ModelInfo, UserContent};
use sasacode_plugin_api::{Tool, ToolContext, ToolKind, ToolRef, ToolResult};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolSearchConfig {
    /// auto: defer when the thresholds are crossed; always / never force it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Defer when deferrable definitions reach this % of the context window (default 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    /// ...or when there are at least this many deferrable tools (default 30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
}

pub const TOOL_SEARCH_NAME: &str = "tool_search";

/// JS `string.length`.
fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Rough token estimate of a tool definition as sent to the model.
pub fn tool_tokens(t: &dyn Tool) -> usize {
    (js_len(t.name()) + js_len(&t.description()) + js_len(&sasacode_ai::json::stringify(&t.parameters()))).div_ceil(4)
}

pub fn should_defer(deferrable: &[ToolRef], model: &ModelInfo, cfg: &ToolSearchConfig) -> bool {
    let mode = cfg.mode.as_deref().unwrap_or("auto");
    if mode == "never" || deferrable.is_empty() {
        return false;
    }
    if mode == "always" {
        return true;
    }
    let tokens: usize = deferrable.iter().map(|t| tool_tokens(t.as_ref())).sum();
    tokens as f64 >= model.context_window as f64 * cfg.percent.unwrap_or(10.0) / 100.0
        || deferrable.len() >= cfg.count.unwrap_or(30)
}

/// Deferred tools the conversation already used or searched for stay loaded (so resumed sessions keep them).
pub fn loaded_from_history(messages: &[Message]) -> HashSet<String> {
    let mut names = HashSet::new();
    for m in messages {
        match m {
            Message::Assistant(a) => {
                for c in a.tool_calls() {
                    names.insert(c.name.clone());
                }
            }
            Message::Tool(t) if t.tool_name == TOOL_SEARCH_NAME => {
                for c in &t.content {
                    if let UserContent::Text(tc) = c {
                        for line in tc.text.split('\n') {
                            if let Some(rest) = line.strip_prefix("### ")
                                && !rest.is_empty()
                                && !rest.chars().any(char::is_whitespace)
                            {
                                names.insert(rest.to_string());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    names
}

/// JS `/[\s_\-.]+/` split: whitespace (Unicode), `_`, `-`, `.`.
fn split_terms(q: &str) -> Vec<String> {
    q.split(|c: char| c.is_whitespace() || c == '\u{feff}' || c == '_' || c == '-' || c == '.')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn search_tools(query: &str, tools: &[ToolRef], limit: usize) -> Vec<ToolRef> {
    let q = query.trim();
    if let Some(list) = q.strip_prefix("select:") {
        let wanted: Vec<&str> = list.split(',').map(str::trim).collect();
        return tools.iter().filter(|t| wanted.contains(&t.name())).cloned().collect();
    }
    let terms = split_terms(&q.to_lowercase());
    let mut scored: Vec<(ToolRef, u32)> = tools
        .iter()
        .map(|t| {
            let name = t.name().to_lowercase();
            let desc = t.description().to_lowercase();
            let mut score = 0;
            for term in &terms {
                if name == *term {
                    score += 10;
                } else if name.contains(term.as_str()) {
                    score += 5;
                }
                if desc.contains(term.as_str()) {
                    score += 1;
                }
            }
            (t.clone(), score)
        })
        .filter(|(_, s)| *s > 0)
        .collect();
    // Stable, like Array.prototype.sort.
    scored.sort_by(|a, b| b.1.cmp(&a.1));
    scored.into_iter().take(limit).map(|(t, _)| t).collect()
}

type Deferred = Arc<dyn Fn() -> Vec<ToolRef> + Send + Sync>;
type OnLoad = Arc<dyn Fn(Vec<String>) + Send + Sync>;

/// The core search tool offered when definitions are deferred. Its description carries the catalog.
pub struct ToolSearchTool {
    deferred: Deferred,
    on_load: OnLoad,
}

pub fn make_tool_search_tool(
    deferred: impl Fn() -> Vec<ToolRef> + Send + Sync + 'static,
    on_load: impl Fn(Vec<String>) + Send + Sync + 'static,
) -> ToolSearchTool {
    ToolSearchTool {
        deferred: Arc::new(deferred),
        on_load: Arc::new(on_load),
    }
}

/// JS `s.slice(0, n)` in UTF-16 units, without splitting a character.
fn js_slice(s: &str, n: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > n {
            return &s[..i];
        }
    }
    s
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        TOOL_SEARCH_NAME
    }
    fn description(&self) -> String {
        let catalog: Vec<String> = (self.deferred)()
            .iter()
            .map(|t| {
                let d = t.description();
                let first = d.split('\n').next().unwrap_or("");
                format!("- {}: {}", t.name(), js_slice(first, 160))
            })
            .collect();
        format!(
            "Load the full definitions of additional tools so you can call them. Query with keywords, or \
             \"select:name1,name2\" for exact names. Tools available through this search:\n{}",
            catalog.join("\n")
        )
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Keywords, or \"select:<name>,<name>\"" },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum results (default 5)" }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    // Touches no files: runs without asking wherever reads in the working directory do.
    fn paths(&self, _args: &JsonObject, _cwd: &Path) -> Option<Vec<String>> {
        Some(Vec::new())
    }
    fn concurrent(&self) -> bool {
        true
    }
    fn always_load(&self) -> bool {
        true
    }
    fn summary(&self, args: &JsonObject) -> Option<String> {
        Some(args.get("query").and_then(Value::as_str).unwrap_or("").to_string())
    }
    async fn execute(&self, args: JsonObject, _ctx: ToolContext) -> anyhow::Result<ToolResult> {
        let query = args.get("query").and_then(Value::as_str).unwrap_or("").to_string();
        let limit = args
            .get("limit")
            .and_then(Value::as_f64)
            .map(|l| l as usize)
            .unwrap_or(5);
        let found = search_tools(&query, &(self.deferred)(), limit);
        if found.is_empty() {
            return Ok(ToolResult::text(format!("No tools match \"{query}\".")));
        }
        (self.on_load)(found.iter().map(|t| t.name().to_string()).collect());
        let body: Vec<String> = found
            .iter()
            .map(|t| {
                format!(
                    "### {}\n{}\nparameters: {}",
                    t.name(),
                    t.description(),
                    sasacode_ai::json::stringify(&t.parameters())
                )
            })
            .collect();
        Ok(ToolResult::text(format!(
            "Loaded {} tool(s); you can call them now.\n\n{}",
            found.len(),
            body.join("\n\n")
        )))
    }
}
