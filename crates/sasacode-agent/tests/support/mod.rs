//! Shared set-up for the ports of `packages/agent/test/*.test.ts`: the scripted replies, the
//! replay provider under an api name of its own, and helpers for what JS did implicitly.
#![allow(dead_code)]

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::future::BoxFuture;
use serde_json::{Value, json};

use sasacode_agent::agent::Agent;
use sasacode_agent::events::AgentEvent;
use sasacode_agent::permission::PermissionPolicy;
use sasacode_ai::{
    AssistantContent, AssistantMessage, JsonObject, Message, ModelInfo, StopReason, ToolCall, Usage, UserContent,
    register_api,
};
use sasacode_plugin_api::{PermissionMode, PermissionRules, PluginApi, PluginInit, ToolRef};
use sasacode_testing::{ReplayItem, ReplayOptions, ReplayProvider};

/// A replay api name no other test uses: the provider registry is global and tests run in parallel,
/// where the TS files ran one test at a time under "replay".
pub fn api_name() -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    format!(
        "replay-agent-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// `registerApi(api, replayProvider(script, options))`; the provider is kept to read its requests.
pub fn register<T: Into<ReplayItem>>(api: &str, script: Vec<T>, chunk_size: Option<usize>) -> Arc<ReplayProvider> {
    let provider = Arc::new(ReplayProvider::new(
        script,
        ReplayOptions {
            api: Some(api.to_string()),
            chunk_size,
        },
    ));
    register_api(api, provider.clone());
    provider
}

pub fn obj(v: Value) -> JsonObject {
    v.as_object().cloned().unwrap_or_default()
}

pub fn say(text: &str) -> AssistantContent {
    AssistantContent::text(text)
}

pub fn call(id: &str, name: &str, input: Value) -> AssistantContent {
    AssistantContent::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        input: obj(input),
        raw_input: None,
    })
}

/// A reply from provider `provider`, model "m": stopReason tool_use when it calls a tool, else stop;
/// empty usage, timestamp 0.
pub fn reply(content: Vec<AssistantContent>, provider: &str) -> AssistantMessage {
    let tool = content.iter().any(|c| matches!(c, AssistantContent::ToolCall(_)));
    AssistantMessage {
        content,
        api: "replay".into(),
        provider: provider.into(),
        model: "m".into(),
        usage: Usage::default(),
        stop_reason: if tool { StopReason::ToolUse } else { StopReason::Stop },
        timestamp: 0,
        replay_scope: None,
        error_message: None,
    }
}

pub fn usage(input: u64, output: u64) -> Usage {
    Usage {
        input,
        output,
        ..Usage::default()
    }
}

pub fn model(api: &str, provider: &str, context_window: u64, max_output: u64) -> ModelInfo {
    serde_json::from_value(json!({
        "id": "m", "provider": provider, "api": api, "contextWindow": context_window, "maxOutput": max_output,
    }))
    .unwrap_or_else(|e| panic!("model: {e}"))
}

pub fn policy(mode: PermissionMode, rules: Value) -> Arc<PermissionPolicy> {
    let rules: PermissionRules = serde_json::from_value(rules).unwrap_or_else(|e| panic!("rules: {e}"));
    Arc::new(PermissionPolicy::new(mode, &rules).unwrap_or_else(|e| panic!("policy: {e}")))
}

/// read, write, edit and bash (`const tools = [readTool, writeTool, editTool, bashTool]`).
pub fn tools() -> Vec<ToolRef> {
    sasacode_tools::builtin_tools()
}

/// `builtinTools` as a plugin, for `host.load("builtin-tools", builtinTools)`.
pub fn builtin_tools(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(sasacode_tools::plugin(api))
}

/// A plugin whose setup is synchronous (the TS `(api) => { api.on(...) }`).
pub fn plug(f: impl FnOnce(&PluginApi) + Send + 'static) -> PluginInit {
    Box::new(move |api| {
        f(&api);
        Box::pin(async { Ok(()) })
    })
}

/// Every event the agent emits from now on (`agent.events.on((e) => events.push(e))`).
pub fn record(agent: &Agent) -> Arc<Mutex<Vec<AgentEvent>>> {
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::default();
    let e = events.clone();
    // Kept subscribed for the agent's life, as the TS never unsubscribed.
    let _ = agent.events().on(move |ev| e.lock().unwrap().push(ev.clone()));
    events
}

pub fn count(events: &Mutex<Vec<AgentEvent>>, type_name: &str) -> usize {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e.type_name() == type_name)
        .count()
}

/// `JSON.stringify(v)`.
pub fn json<T: serde::Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| panic!("json: {e}"))
}

pub fn user(text: &str) -> Message {
    Message::User(sasacode_ai::UserMessage {
        content: vec![UserContent::text(text)],
        timestamp: 0,
    })
}

pub fn temp_dir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .unwrap_or_else(|e| panic!("tempdir: {e}"))
}

pub fn path_str(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}

/// `void promise` in JS: an async function runs synchronously up to its first await before the
/// caller goes on. A Rust future does nothing until polled, so this polls it once here, then lets
/// the runtime drive the rest. Used where a TS test started work from inside an event listener.
pub fn start<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let mut fut = Box::pin(fut);
    match poll_once(&mut fut) {
        Poll::Ready(v) => tokio::spawn(async move { v }),
        Poll::Pending => tokio::spawn(fut),
    }
}

/// One step of `fut`, here and now. The runtime polls it again (with its own waker) once spawned.
pub fn poll_once<F: Future + Unpin>(fut: &mut F) -> Poll<F::Output> {
    let mut cx = Context::from_waker(futures::task::noop_waker_ref());
    std::pin::Pin::new(fut).poll(&mut cx)
}
