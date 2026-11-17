//! A small MCP server for tests (packages/mcp/test/fixture-server.ts), written on rmcp so the
//! client is checked against an implementation that is not its own. The stdio binary is
//! examples/mcp-fixture.rs; the HTTP tests mount [`FixtureServer`] in-process.
//!
//! Environment switches, as in the TS fixture: FIXTURE_NO_TOOLS (prompts only, no tools/list),
//! FIXTURE_TOOLS_FAIL (tools/list fails), FIXTURE_ENDLESS_PROMPTS (a next cursor forever).

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode, GetPromptRequestParams,
    GetPromptResponse, GetPromptResult, Implementation, ListPromptsResult, ListToolsResult, PaginatedRequestParams,
    Prompt, PromptArgument, PromptMessage, Role, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Map, Value, json};

#[derive(Clone)]
pub struct FixtureServer {
    /// `toggle` lists and unlists the `late` tool, announcing the change like a real server would.
    late: Arc<AtomicBool>,
    no_tools: bool,
    tools_fail: bool,
    endless_prompts: bool,
}

impl Default for FixtureServer {
    fn default() -> Self {
        Self::new()
    }
}

impl FixtureServer {
    pub fn new() -> Self {
        let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
        FixtureServer {
            late: Arc::new(AtomicBool::new(false)),
            no_tools: set("FIXTURE_NO_TOOLS"),
            tools_fail: set("FIXTURE_TOOLS_FAIL"),
            endless_prompts: set("FIXTURE_ENDLESS_PROMPTS"),
        }
    }
}

fn schema(v: Value) -> Arc<Map<String, Value>> {
    Arc::new(v.as_object().cloned().unwrap_or_default())
}

fn empty() -> Arc<Map<String, Value>> {
    schema(json!({ "type": "object", "properties": {} }))
}

/// A result with only content, without `isError` (the TS fixture returned `{ content }`).
fn content_only(text: String) -> CallToolResult {
    let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
    r.is_error = None;
    r
}

impl ServerHandler for FixtureServer {
    fn get_info(&self) -> ServerConfig {
        let caps = if self.no_tools {
            ServerCapabilities::builder().enable_prompts().build()
        } else {
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .enable_prompts()
                .build()
        };
        ServerConfig::new(caps).with_server_info(Implementation::new("fixture", "1.0.0"))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if self.no_tools {
            return Err(ErrorData::new(ErrorCode::METHOD_NOT_FOUND, "Method not found", None));
        }
        if self.tools_fail {
            return Err(ErrorData::internal_error("tools are broken", None));
        }
        let mut tools = vec![
            Tool::new(
                "add",
                "Add two numbers",
                schema(json!({
                    "type": "object",
                    "properties": { "a": { "type": "number" }, "b": { "type": "number" } },
                    "required": ["a", "b"],
                })),
            )
            .annotate(ToolAnnotations::new().read_only(true)),
            Tool::new("fail", "Always fails", empty()),
            Tool::new("big", "Large output", empty()),
            Tool::new("toggle", "List or unlist the late tool", empty()),
        ];
        if self.late.load(Ordering::SeqCst) {
            tools.push(Tool::new("late", "Listed after toggle", empty()));
        }
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = request.arguments.unwrap_or_default();
        let num = |k: &str| args.get(k).and_then(Value::as_f64).unwrap_or(f64::NAN);
        let result = match request.name.as_ref() {
            "add" => content_only(sasacode_ai::json::js_number(num("a") + num("b")).to_string()),
            "big" => content_only("x".repeat(150_000)),
            "toggle" => {
                let late = !self.late.fetch_xor(true, Ordering::SeqCst);
                let _ = context.peer.notify_tool_list_changed().await;
                content_only(format!("late is now {}", if late { "listed" } else { "gone" }))
            }
            _ => {
                let mut r = CallToolResult::success(vec![ContentBlock::text("it broke")]);
                r.is_error = Some(true);
                r
            }
        };
        Ok(result.into())
    }

    // Two pages, as a server with many prompts sends them.
    async fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        if self.endless_prompts {
            // A broken server: always one more page (#49).
            let mut r = ListPromptsResult::with_all_items(vec![Prompt::new("again", Some("Again"), None)]);
            r.next_cursor = Some("again".into());
            return Ok(r);
        }
        if request.and_then(|r| r.cursor).as_deref() == Some("page2") {
            return Ok(ListPromptsResult::with_all_items(vec![Prompt::new(
                "later",
                Some("On the second page"),
                None,
            )]));
        }
        let mut r = ListPromptsResult::with_all_items(vec![
            Prompt::new(
                "review",
                Some("Review a file"),
                Some(vec![PromptArgument::new("file").with_required(true)]),
            ),
            Prompt::new(
                "greet",
                Some("Greet someone"),
                Some(vec![PromptArgument::new("who"), PromptArgument::new("greeting")]),
            ),
        ]);
        r.next_cursor = Some("page2".into());
        Ok(r)
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let text = if request.name == "greet" {
            match &request.arguments {
                Some(a) => format!("args {}", serde_json::to_string(a).unwrap_or_default()),
                None => "args undefined".to_string(),
            }
        } else {
            let file = request
                .arguments
                .as_ref()
                .and_then(|a| a.get("file"))
                .and_then(Value::as_str);
            format!("Please review {}", file.unwrap_or("undefined"))
        };
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)]).into())
    }
}
