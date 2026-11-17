//! The interface bundled plugins use (`packages/plugin-api`). Since the Rust rewrite it is
//! internal: there are no external plugins (rust/DECISIONS.md ADR-002), but every built-in tool,
//! command and behaviour still registers through it, as before (P2).

pub mod api;
pub mod hooks;
pub mod host;
pub mod settings;
pub mod tool;
pub mod validate;

pub use api::*;
pub use hooks::{
    hook, Decision, DeltaKind, Hook, HookContext, HookName, HookRunner, PermissionMode, SessionEndReason, Spend,
    SpendSource, StopCause, Stopped, Verdict, VerdictSource,
};
pub use host::*;
pub use sasacode_ai::{
    AssistantContent, AssistantMessage, JsonObject, Message, ModelInfo, Provider, ProviderConfig, SamplingOptions,
    StopReason, ToolCall, UserContent,
};
pub use tool::*;
