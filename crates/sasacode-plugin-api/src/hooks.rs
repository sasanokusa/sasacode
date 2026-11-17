//! Hooks (requirements 5.3): typed events and results, and the runner that calls handlers in
//! registration order (`HookMap` in packages/plugin-api, `HookRunner` in packages/agent).
//!
//! Each hook is a marker type implementing [`Hook`]; register with `api.on::<hook::UserPrompt, _, _>(..)`.
//! A handler gets its own clone of the event and returns `Ok(None)` (no change), `Ok(Some(result))`,
//! or `Err` (reported as a plugin_error, the next handler still runs).

use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

use sasacode_ai::{AssistantMessage, Fetch, JsonObject, Message, ModelInfo, SamplingOptions, StopReason, ToolCall, Usage, UserContent};

use crate::tool::{ToolRef, ToolResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookName {
    SessionStart,
    SessionEnd,
    UserPrompt,
    SystemPrompt,
    BeforeRequest,
    StreamDelta,
    AssistantMessage,
    ToolCallRaw,
    ToolCall,
    Permission,
    ToolResult,
    TurnEnd,
    Usage,
    AgentEnd,
    ContextLimit,
}

impl HookName {
    pub fn as_str(self) -> &'static str {
        match self {
            HookName::SessionStart => "session_start",
            HookName::SessionEnd => "session_end",
            HookName::UserPrompt => "user_prompt",
            HookName::SystemPrompt => "system_prompt",
            HookName::BeforeRequest => "before_request",
            HookName::StreamDelta => "stream_delta",
            HookName::AssistantMessage => "assistant_message",
            HookName::ToolCallRaw => "tool_call_raw",
            HookName::ToolCall => "tool_call",
            HookName::Permission => "permission",
            HookName::ToolResult => "tool_result",
            HookName::TurnEnd => "turn_end",
            HookName::Usage => "usage",
            HookName::AgentEnd => "agent_end",
            HookName::ContextLimit => "context_limit",
        }
    }
}

// ── shared vocabulary ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Ask,
    Deny,
}

impl Decision {
    /// allow < ask < deny.
    pub fn rank(self) -> u8 {
        self as u8
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Ask => "ask",
            Decision::Deny => "deny",
        }
    }
}

/// auto: allow everything; agent: the model judges; ask: ask for every call; edits: reads and edits
/// in the working directory run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    Auto,
    Agent,
    Ask,
    Edits,
}

impl PermissionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionMode::Auto => "auto",
            PermissionMode::Agent => "agent",
            PermissionMode::Ask => "ask",
            PermissionMode::Edits => "edits",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "agent" => Some(Self::Agent),
            "ask" => Some(Self::Ask),
            "edits" => Some(Self::Edits),
            _ => None,
        }
    }
}

/// Where a verdict came from: "rule" the user's (or a plugin's) allow/ask/deny rule, "soft" a
/// softDeny rule, "mode" the permission mode's default, "plugin" a tool_call hook's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerdictSource {
    Rule,
    Soft,
    Mode,
    Plugin,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub decision: Decision,
    pub reason: String,
    pub source: VerdictSource,
}

/// `stopped`: a plugin ended the run from turn_end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCause {
    Done,
    Aborted,
    Error,
    Refusal,
    ContextLimit,
    MaxTurns,
    Stopped,
}

impl StopCause {
    pub fn as_str(self) -> &'static str {
        match self {
            StopCause::Done => "done",
            StopCause::Aborted => "aborted",
            StopCause::Error => "error",
            StopCause::Refusal => "refusal",
            StopCause::ContextLimit => "context_limit",
            StopCause::MaxTurns => "max_turns",
            StopCause::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeltaKind {
    Text,
    Thinking,
    Toolcall,
}

/// Which plugin stopped generation or the run, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stopped {
    pub plugin: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpendSource {
    Subagent,
    Side,
    Discarded,
}

/// What a response outside the conversation used.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spend {
    pub t: i64,
    pub model: String,
    pub usage: Usage,
    pub source: SpendSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionEndReason {
    Switch,
    Exit,
}

/// Where a hook fired. `agent`: None for the main agent, a unique id for each subagent run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookContext {
    pub agent: Option<String>,
}

// ── events and results ───────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SessionStartEvent {
    pub session_id: Option<String>,
    pub resumed: bool,
}

#[derive(Debug, Clone)]
pub struct SessionEndEvent {
    pub session_id: Option<String>,
    pub reason: Option<SessionEndReason>,
}

#[derive(Debug, Clone)]
pub struct UserPromptEvent {
    pub content: Vec<UserContent>,
}
/// Transform input, or consume it (`handled`) so it never reaches the model.
#[derive(Debug, Clone, Default)]
pub struct UserPromptResult {
    pub content: Option<Vec<UserContent>>,
    pub handled: bool,
}

#[derive(Debug, Clone)]
pub struct SystemPromptEvent {
    pub prompt: String,
}
#[derive(Debug, Clone, Default)]
pub struct SystemPromptResult {
    pub prompt: Option<String>,
}

#[derive(Clone)]
pub struct BeforeRequestEvent {
    pub messages: Vec<Message>,
    pub model: ModelInfo,
    pub sampling: SamplingOptions,
    /// The transport composed so far (None: the default client).
    pub fetch: Option<Arc<dyn Fetch>>,
}
#[derive(Clone, Default)]
pub struct BeforeRequestResult {
    pub messages: Option<Vec<Message>>,
    pub sampling: Option<SamplingOptions>,
    pub fetch: Option<Arc<dyn Fetch>>,
}

/// Synchronous, cheap streaming handler: accumulated text and early stop.
#[derive(Debug, Clone)]
pub struct StreamDeltaEvent {
    pub kind: DeltaKind,
    pub index: usize,
    pub delta: String,
    /// The block's text so far (for tool calls: the argument JSON so far).
    pub text: String,
    pub message: Arc<AssistantMessage>,
}
#[derive(Debug, Clone, Default)]
pub struct StreamDeltaResult {
    pub stop: Option<String>,
}

/// Rewrite, retry, or inject after the finished response, before storage.
#[derive(Debug, Clone)]
pub struct AssistantMessageEvent {
    pub message: AssistantMessage,
    pub stopped: Option<Stopped>,
}
#[derive(Debug, Clone, Default)]
pub struct AssistantMessageResult {
    pub message: Option<AssistantMessage>,
    pub retry: bool,
    pub inject: Option<String>,
}

/// Repair model-produced calls before validation; never sees calls cut off by max_tokens.
#[derive(Clone)]
pub struct ToolCallRawEvent {
    pub call: ToolCall,
    pub name: String,
    pub input: JsonObject,
    pub raw_input: Option<String>,
    pub tools: Vec<ToolRef>,
    pub stop_reason: StopReason,
}
/// `input: Some(..)` counts as a change even when equal to the original (TS compared identity).
#[derive(Debug, Clone, Default)]
pub struct ToolCallRawResult {
    pub name: Option<String>,
    pub input: Option<JsonObject>,
    pub note: Option<String>,
}

/// Rewrite arguments or decide permission before the core policy runs.
#[derive(Clone)]
pub struct ToolCallEvent {
    pub call: ToolCall,
    pub tool: ToolRef,
    pub args: JsonObject,
}
#[derive(Debug, Clone, Default)]
pub struct ToolCallResult {
    pub args: Option<JsonObject>,
    pub decision: Option<Decision>,
    pub reason: Option<String>,
}

/// Refine permission within `lowest`; rules and hook decisions only tighten.
#[derive(Clone)]
pub struct PermissionEvent {
    pub call: ToolCall,
    pub tool: ToolRef,
    pub args: JsonObject,
    pub cwd: PathBuf,
    pub mode: PermissionMode,
    pub verdict: Verdict,
    pub lowest: Decision,
}
#[derive(Debug, Clone, Default)]
pub struct PermissionResult {
    pub decision: Option<Decision>,
    pub reason: Option<String>,
}

/// Results include calls that never ran (`ran: false`).
#[derive(Debug, Clone)]
pub struct ToolResultEvent {
    pub call: ToolCall,
    pub result: ToolResult,
    pub ran: bool,
}
#[derive(Debug, Clone, Default)]
pub struct ToolResultResult {
    pub result: Option<ToolResult>,
}

/// Continue via inject or stop the run with a reason.
#[derive(Debug, Clone)]
pub struct TurnEndEvent {
    pub turn: u32,
    pub message: AssistantMessage,
}
#[derive(Debug, Clone, Default)]
pub struct TurnEndResult {
    pub inject: Option<String>,
    pub stop: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AgentEndEvent {
    pub cause: StopCause,
    pub stopped: Option<Stopped>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Default)]
pub struct AgentEndResult {
    pub inject: Option<String>,
}

/// Free up context (e.g. session.replace_messages) and return `retry` to continue.
#[derive(Debug, Clone)]
pub struct ContextLimitEvent {
    /// -1 when the provider reported the overflow as an error.
    pub tokens: i64,
    pub context_window: u64,
}
#[derive(Debug, Clone, Default)]
pub struct ContextLimitResult {
    pub retry: bool,
}

// ── the hook markers ─────────────────────────────────────────────────

pub trait Hook: 'static {
    const NAME: HookName;
    type Event: Clone + Send + Sync + 'static;
    type Result: Send + 'static;
}

pub mod hook {
    use super::*;

    macro_rules! hooks {
        ($($marker:ident => $name:ident, $event:ty, $result:ty;)*) => {
            $(
                pub struct $marker;
                impl Hook for $marker {
                    const NAME: HookName = HookName::$name;
                    type Event = $event;
                    type Result = $result;
                }
            )*
        };
    }

    hooks! {
        SessionStart => SessionStart, SessionStartEvent, ();
        SessionEnd => SessionEnd, SessionEndEvent, ();
        UserPrompt => UserPrompt, UserPromptEvent, UserPromptResult;
        SystemPrompt => SystemPrompt, SystemPromptEvent, SystemPromptResult;
        BeforeRequest => BeforeRequest, BeforeRequestEvent, BeforeRequestResult;
        StreamDelta => StreamDelta, StreamDeltaEvent, StreamDeltaResult;
        AssistantMessage => AssistantMessage, AssistantMessageEvent, AssistantMessageResult;
        ToolCallRaw => ToolCallRaw, ToolCallRawEvent, ToolCallRawResult;
        ToolCall => ToolCall, ToolCallEvent, ToolCallResult;
        Permission => Permission, PermissionEvent, PermissionResult;
        ToolResult => ToolResult, ToolResultEvent, ToolResultResult;
        TurnEnd => TurnEnd, TurnEndEvent, TurnEndResult;
        Usage => Usage, Spend, ();
        AgentEnd => AgentEnd, AgentEndEvent, AgentEndResult;
        ContextLimit => ContextLimit, ContextLimitEvent, ContextLimitResult;
    }
}

// ── the runner ───────────────────────────────────────────────────────

pub type AsyncHandler<H> = Arc<
    dyn Fn(<H as Hook>::Event, HookContext) -> BoxFuture<'static, anyhow::Result<Option<<H as Hook>::Result>>>
        + Send
        + Sync,
>;
pub type SyncHandler<H> =
    Arc<dyn Fn(&<H as Hook>::Event, &HookContext) -> anyhow::Result<Option<<H as Hook>::Result>> + Send + Sync>;

#[derive(Clone)]
struct Entry {
    owner: String,
    /// `AsyncHandler<H>` or `SyncHandler<H>` for the hook it is filed under.
    handler: Arc<dyn Any + Send + Sync>,
}

pub type OnHookError = Arc<dyn Fn(&str, HookName, &anyhow::Error) + Send + Sync>;

/// Runs hook handlers in registration order. A failing handler is reported and skipped; the loop goes on.
#[derive(Clone, Default)]
pub struct HookRunner {
    handlers: Arc<RwLock<HashMap<HookName, Vec<Entry>>>>,
    on_error: Arc<RwLock<Option<OnHookError>>>,
    /// Passed to every handler: which agent the hook fired for.
    pub ctx: HookContext,
}

impl HookRunner {
    pub fn new(ctx: HookContext) -> Self {
        HookRunner { ctx, ..Default::default() }
    }

    pub fn set_on_error(&self, f: OnHookError) {
        *self.on_error.write().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    pub fn has_on_error(&self) -> bool {
        self.on_error.read().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    fn report(&self, owner: &str, name: HookName, e: &anyhow::Error) {
        let f = self.on_error.read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(f) = f {
            f(owner, name, e);
        }
    }

    fn push(&self, name: HookName, owner: &str, handler: Arc<dyn Any + Send + Sync>) {
        self.handlers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(name)
            .or_default()
            .push(Entry { owner: owner.to_string(), handler });
    }

    /// Register an async handler for hook `H`, owned by `owner` (a plugin name or "core").
    pub fn on<H, F, Fut>(&self, owner: &str, f: F)
    where
        H: Hook,
        F: Fn(H::Event, HookContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<Option<H::Result>>> + Send + 'static,
    {
        let h: AsyncHandler<H> = Arc::new(move |e, c| Box::pin(f(e, c)));
        self.push(H::NAME, owner, Arc::new(h));
    }

    /// Register a synchronous handler (for stream_delta: it runs on the streaming path).
    pub fn on_sync<H, F>(&self, owner: &str, f: F)
    where
        H: Hook,
        F: Fn(&H::Event, &HookContext) -> anyhow::Result<Option<H::Result>> + Send + Sync + 'static,
    {
        let h: SyncHandler<H> = Arc::new(f);
        self.push(H::NAME, owner, Arc::new(h));
    }

    /// A runner with these handlers except the named hooks (for subagents, with their own ctx).
    pub fn without(&self, names: &[HookName], ctx: HookContext) -> HookRunner {
        let map: HashMap<HookName, Vec<Entry>> = self
            .handlers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(k, _)| !names.contains(k))
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        HookRunner { handlers: Arc::new(RwLock::new(map)), on_error: self.on_error.clone(), ctx }
    }

    pub fn has(&self, name: HookName) -> bool {
        self.handlers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&name)
            .is_some_and(|v| !v.is_empty())
    }

    fn entries(&self, name: HookName) -> Vec<Entry> {
        self.handlers.read().unwrap_or_else(|e| e.into_inner()).get(&name).cloned().unwrap_or_default()
    }

    /// Call each handler with the current event; `step` folds its result into the event and
    /// returns false to stop early. Returns the event as the handlers left it.
    pub async fn run<H: Hook>(
        &self,
        mut event: H::Event,
        mut step: impl FnMut(H::Result, &mut H::Event, &str) -> bool,
    ) -> H::Event {
        for entry in self.entries(H::NAME) {
            let result = if let Some(h) = entry.handler.downcast_ref::<AsyncHandler<H>>() {
                h(event.clone(), self.ctx.clone()).await
            } else if let Some(h) = entry.handler.downcast_ref::<SyncHandler<H>>() {
                h(&event, &self.ctx)
            } else {
                continue;
            };
            match result {
                Err(e) => self.report(&entry.owner, H::NAME, &e),
                Ok(Some(r)) => {
                    if !step(r, &mut event, &entry.owner) {
                        break;
                    }
                }
                Ok(None) => {}
            }
        }
        event
    }

    /// `run` without folding results (hooks whose result is `()`).
    pub async fn emit<H: Hook>(&self, event: H::Event) {
        self.run::<H>(event, |_, _, _| true).await;
    }

    /// For hooks on the streaming path: async handlers are reported as errors and skipped.
    pub fn run_sync<H: Hook>(&self, event: &mut H::Event, mut step: impl FnMut(H::Result, &mut H::Event, &str) -> bool) {
        for entry in self.entries(H::NAME) {
            if let Some(h) = entry.handler.downcast_ref::<SyncHandler<H>>() {
                match h(event, &self.ctx) {
                    Err(e) => self.report(&entry.owner, H::NAME, &e),
                    Ok(Some(r)) => {
                        if !step(r, event, &entry.owner) {
                            break;
                        }
                    }
                    Ok(None) => {}
                }
            } else if entry.handler.downcast_ref::<AsyncHandler<H>>().is_some() {
                let e = anyhow::anyhow!("{} handlers must be synchronous; the result was ignored", H::NAME.as_str());
                self.report(&entry.owner, H::NAME, &e);
            }
        }
    }
}
