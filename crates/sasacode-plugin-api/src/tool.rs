//! Tools: what the model can call (`ToolDefinition` in packages/plugin-api).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use sasacode_ai::{JsonObject, UserContent};

/// What a tool does, for the permission system: read = no side effects, edit = changes files under
/// `paths`, exec = arbitrary effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolKind {
    Read,
    Edit,
    Exec,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub content: Vec<UserContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// Structured data for renderers (diffs, exit codes). Not sent to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonObject>,
}

impl ToolResult {
    pub fn text(t: impl Into<String>) -> Self {
        ToolResult { content: vec![UserContent::text(t)], is_error: None, details: None }
    }
    /// `errorResult(message)`.
    pub fn error(message: impl Into<String>) -> Self {
        ToolResult { content: vec![UserContent::text(message)], is_error: Some(true), details: None }
    }
    pub fn is_error(&self) -> bool {
        self.is_error.unwrap_or(false)
    }
    pub fn with_details(mut self, details: JsonObject) -> Self {
        self.details = Some(details);
        self
    }
}

/// `text(t)` from the TS plugin API.
pub fn text(t: impl Into<String>) -> UserContent {
    UserContent::text(t)
}

/// `errorResult(message)` from the TS plugin API.
pub fn error_result(message: impl Into<String>) -> ToolResult {
    ToolResult::error(message)
}

/// Call another tool with the caller's context and its own validation and permission check.
pub type CallTool = Arc<dyn Fn(String, JsonObject) -> BoxFuture<'static, ToolResult> + Send + Sync>;
/// Stream partial output (e.g. bash stdout) to the UI while the tool runs.
pub type OnUpdate = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub signal: CancellationToken,
    pub on_update: Option<OnUpdate>,
    pub call_tool: Option<CallTool>,
}

impl ToolContext {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        ToolContext { cwd: cwd.into(), signal: CancellationToken::new(), on_update: None, call_tool: None }
    }
    pub fn update(&self, text: impl Into<String>) {
        if let Some(f) = &self.on_update {
            f(text.into());
        }
    }
}

/// A tool. Implement it directly, or build a [`ToolDef`].
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> String;
    fn parameters(&self) -> Value;
    fn kind(&self) -> ToolKind;
    /// Safe to run alongside other concurrent tools in the same turn (FR-L04).
    fn concurrent(&self) -> bool {
        false
    }
    /// Always send the full definition, even when tool search defers the rest.
    fn always_load(&self) -> bool {
        false
    }
    /// Files the call touches, for path-based permission rules. `None`: the tool has no `paths`.
    fn paths(&self, _args: &JsonObject, _cwd: &Path) -> Option<Vec<String>> {
        None
    }
    /// String matched by permission patterns such as `bash(git *)`. `None`: no matchTarget.
    fn match_target(&self, _args: &JsonObject) -> Option<String> {
        None
    }
    /// Also apply the rules for this tool name.
    fn permissions_as(&self) -> Option<String> {
        None
    }
    /// One-line label for UIs, e.g. the command or path. `None`: no summary.
    fn summary(&self, _args: &JsonObject) -> Option<String> {
        None
    }
    /// Run the call. An `Err` becomes the result "Tool failed: <message>".
    async fn execute(&self, args: JsonObject, ctx: ToolContext) -> anyhow::Result<ToolResult>;
}

pub type ToolRef = Arc<dyn Tool>;

/// The model's view of a tool.
pub fn spec_of(t: &dyn Tool) -> sasacode_ai::ToolSpec {
    sasacode_ai::ToolSpec { name: t.name().to_string(), description: t.description(), parameters: t.parameters() }
}

pub type ExecuteFn = Arc<dyn Fn(JsonObject, ToolContext) -> BoxFuture<'static, anyhow::Result<ToolResult>> + Send + Sync>;
pub type PathsFn = Arc<dyn Fn(&JsonObject, &Path) -> Vec<String> + Send + Sync>;
pub type StringFn = Arc<dyn Fn(&JsonObject) -> String + Send + Sync>;

/// A tool written as data, like the TS object literals plugins registered.
#[derive(Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub kind: ToolKind,
    pub concurrent: bool,
    pub always_load: bool,
    pub permissions_as: Option<String>,
    pub paths: Option<PathsFn>,
    pub match_target: Option<StringFn>,
    pub summary: Option<StringFn>,
    pub execute: ExecuteFn,
}

impl ToolDef {
    pub fn new<F, Fut>(name: &str, description: &str, parameters: Value, kind: ToolKind, execute: F) -> Self
    where
        F: Fn(JsonObject, ToolContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<ToolResult>> + Send + 'static,
    {
        ToolDef {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
            kind,
            concurrent: false,
            always_load: false,
            permissions_as: None,
            paths: None,
            match_target: None,
            summary: None,
            execute: Arc::new(move |a, c| Box::pin(execute(a, c))),
        }
    }
    pub fn concurrent(mut self, yes: bool) -> Self {
        self.concurrent = yes;
        self
    }
    pub fn always_load(mut self, yes: bool) -> Self {
        self.always_load = yes;
        self
    }
    pub fn summary(mut self, f: impl Fn(&JsonObject) -> String + Send + Sync + 'static) -> Self {
        self.summary = Some(Arc::new(f));
        self
    }
    pub fn match_target(mut self, f: impl Fn(&JsonObject) -> String + Send + Sync + 'static) -> Self {
        self.match_target = Some(Arc::new(f));
        self
    }
    pub fn paths(mut self, f: impl Fn(&JsonObject, &Path) -> Vec<String> + Send + Sync + 'static) -> Self {
        self.paths = Some(Arc::new(f));
        self
    }
    pub fn permissions_as(mut self, name: &str) -> Self {
        self.permissions_as = Some(name.to_string());
        self
    }
    pub fn into_ref(self) -> ToolRef {
        Arc::new(self)
    }
}

#[async_trait]
impl Tool for ToolDef {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> String {
        self.description.clone()
    }
    fn parameters(&self) -> Value {
        self.parameters.clone()
    }
    fn kind(&self) -> ToolKind {
        self.kind
    }
    fn concurrent(&self) -> bool {
        self.concurrent
    }
    fn always_load(&self) -> bool {
        self.always_load
    }
    fn paths(&self, args: &JsonObject, cwd: &Path) -> Option<Vec<String>> {
        self.paths.as_ref().map(|f| f(args, cwd))
    }
    fn match_target(&self, args: &JsonObject) -> Option<String> {
        self.match_target.as_ref().map(|f| f(args))
    }
    fn permissions_as(&self) -> Option<String> {
        self.permissions_as.clone()
    }
    fn summary(&self, args: &JsonObject) -> Option<String> {
        self.summary.as_ref().map(|f| f(args))
    }
    async fn execute(&self, args: JsonObject, ctx: ToolContext) -> anyhow::Result<ToolResult> {
        (self.execute)(args, ctx).await
    }
}
