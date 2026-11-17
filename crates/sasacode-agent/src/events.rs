//! What the agent tells front ends (`packages/agent/src/events.ts`), and the bus it tells them on.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Value, json};

use sasacode_ai::{AssistantMessage, JsonObject, Message, StreamEvent, ToolCall, Usage};
use sasacode_plugin_api::{SpendSource, StopCause, Stopped, ToolResult};

/// What a repaired call looked like before the repair.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairOrigin {
    pub name: String,
    pub input: JsonObject,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    AgentStart,
    TurnStart {
        turn: u32,
    },
    MessageStart {
        message: Arc<AssistantMessage>,
    },
    MessageUpdate {
        message: Arc<AssistantMessage>,
        event: StreamEvent,
    },
    MessageEnd {
        message: Message,
    },
    #[serde(rename_all = "camelCase")]
    ToolStart {
        #[serde(serialize_with = "sasacode_ai::tagged_call")]
        call: ToolCall,
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ToolUpdate {
        #[serde(serialize_with = "sasacode_ai::tagged_call")]
        call: ToolCall,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ToolEnd {
        #[serde(serialize_with = "sasacode_ai::tagged_call")]
        call: ToolCall,
        result: ToolResult,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
    },
    TurnEnd {
        turn: u32,
    },
    Usage {
        t: i64,
        model: String,
        usage: Usage,
        source: SpendSource,
    },
    #[serde(rename_all = "camelCase")]
    ContextLimit {
        tokens: i64,
        context_window: u64,
    },
    Error {
        error: String,
    },
    PluginError {
        plugin: String,
        hook: String,
        error: String,
    },
    MessagesReplaced,
    /// A response a plugin asked to regenerate (assistant_message → retry), or a cut reply asked
    /// for again; UIs should drop it.
    MessageDiscarded {
        #[serde(serialize_with = "with_role")]
        message: AssistantMessage,
    },
    ToolRepaired {
        #[serde(serialize_with = "sasacode_ai::tagged_call")]
        call: ToolCall,
        from: RepairOrigin,
        note: String,
    },
    AgentEnd {
        cause: StopCause,
        #[serde(skip_serializing_if = "Option::is_none")]
        stopped: Option<Stopped>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl AgentEvent {
    pub fn type_name(&self) -> &'static str {
        match self {
            AgentEvent::AgentStart => "agent_start",
            AgentEvent::TurnStart { .. } => "turn_start",
            AgentEvent::MessageStart { .. } => "message_start",
            AgentEvent::MessageUpdate { .. } => "message_update",
            AgentEvent::MessageEnd { .. } => "message_end",
            AgentEvent::ToolStart { .. } => "tool_start",
            AgentEvent::ToolUpdate { .. } => "tool_update",
            AgentEvent::ToolEnd { .. } => "tool_end",
            AgentEvent::TurnEnd { .. } => "turn_end",
            AgentEvent::Usage { .. } => "usage",
            AgentEvent::ContextLimit { .. } => "context_limit",
            AgentEvent::Error { .. } => "error",
            AgentEvent::PluginError { .. } => "plugin_error",
            AgentEvent::MessagesReplaced => "messages_replaced",
            AgentEvent::MessageDiscarded { .. } => "message_discarded",
            AgentEvent::ToolRepaired { .. } => "tool_repaired",
            AgentEvent::AgentEnd { .. } => "agent_end",
        }
    }

    /// The event as `--output jsonl` writes it: message_update carries only the delta (no
    /// partial message), message_start only its type (`toJson` in packages/cli/src/headless.ts).
    pub fn to_jsonl(&self) -> Value {
        match self {
            AgentEvent::MessageStart { .. } => json!({ "type": "message_start" }),
            AgentEvent::MessageUpdate { event, .. } => {
                let mut ev = serde_json::to_value(event).unwrap_or(Value::Null);
                if let Value::Object(o) = &mut ev {
                    o.shift_remove("partial");
                }
                json!({ "type": "message_update", "event": ev })
            }
            other => serde_json::to_value(other).unwrap_or(Value::Null),
        }
    }
}

/// `#[serde(serialize_with = "with_role")]`: an assistant message written as a message, `role`
/// first, as the TS events carried it.
fn with_role<S: serde::Serializer>(m: &AssistantMessage, s: S) -> Result<S::Ok, S::Error> {
    #[derive(Serialize)]
    #[serde(tag = "role", rename_all = "snake_case")]
    enum Tagged<'a> {
        Assistant(&'a AssistantMessage),
    }
    Tagged::Assistant(m).serialize(s)
}

pub type Listener = Arc<dyn Fn(&AgentEvent) + Send + Sync>;

/// Synchronous listeners, called in the order they subscribed. A broken listener does not stop
/// the loop.
#[derive(Clone, Default)]
pub struct EventBus {
    listeners: Arc<Mutex<Vec<(u64, Listener)>>>,
    next: Arc<AtomicU64>,
}

/// Unsubscribes when [`Subscription::off`] is called (dropping it keeps the listener).
pub struct Subscription {
    bus: EventBus,
    id: u64,
}

impl Subscription {
    pub fn off(self) {
        self.bus
            .listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(i, _)| *i != self.id);
    }
}

impl EventBus {
    pub fn on(&self, listener: impl Fn(&AgentEvent) + Send + Sync + 'static) -> Subscription {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((id, Arc::new(listener)));
        Subscription { bus: self.clone(), id }
    }

    pub fn emit(&self, event: AgentEvent) {
        let listeners: Vec<Listener> = self
            .listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for l in listeners {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| l(&event)));
            if r.is_err() {
                eprintln!("event listener failed on {}", event.type_name());
            }
        }
    }
}
