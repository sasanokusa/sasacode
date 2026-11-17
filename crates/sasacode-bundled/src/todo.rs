//! todo (`packages/bundled/src/todo.ts`): a task list the model maintains; kept in the session log
//! and shown in the status line. Models of every size write the list and then forget it: when
//! unfinished items sit through `remindAfter` tool calls without an update, a reminder follows the
//! tool results: a user turn of its own, kept in the history. At the end of a tool's output it was
//! read as output and passed over (17 reminders in the session logs, one update right after).
//!
//! settings: { remindAfter?: number (default 10, 0 = off) }

use std::sync::{Arc, Mutex, MutexGuard};

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use sasacode_ai::AssistantContent;
use sasacode_host::harness_note;
use sasacode_plugin_api::{PermissionRules, PluginApi, ToolDef, ToolKind, ToolResult, TurnEndResult, hook};

use crate::repair::js::num_setting;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Todo {
    content: String,
    status: String,
}

fn mark(status: &str) -> &'static str {
    match status {
        "pending" => "☐",
        "in_progress" => "◐",
        "completed" => "☑",
        _ => "undefined",
    }
}

#[derive(Default)]
struct State {
    todos: Vec<Todo>,
    /// Tool calls of the main agent since the list was last written.
    since_update: f64,
}

fn lock(s: &Mutex<State>) -> MutexGuard<'_, State> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

fn show(api: &PluginApi, todos: &[Todo]) {
    if todos.is_empty() {
        return api.ui().set_status("todos", None);
    }
    let done = todos.iter().filter(|t| t.status == "completed").count();
    let current = todos.iter().find(|t| t.status == "in_progress");
    api.ui().set_status(
        "todos",
        Some(format!(
            "TODO {done}/{}{}",
            todos.len(),
            current.map(|c| format!(": {}", c.content)).unwrap_or_default()
        )),
    );
}

fn parse(v: Option<&Value>) -> Vec<Todo> {
    v.and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let remind_after = num_setting(&api.settings(), "remindAfter", 10.0);
        let state: Arc<Mutex<State>> = Arc::default();

        let (st, a) = (state.clone(), api.clone());
        api.on::<hook::SessionStart, _, _>(move |_e, _c| {
            let todos = parse(a.session().entries("todos").last());
            {
                let mut s = lock(&st);
                s.todos = todos.clone();
                s.since_update = 0.0;
            }
            show(&a, &todos);
            async { Ok(None) }
        });

        let st = state.clone();
        api.on::<hook::ToolResult, _, _>(move |ev, ctx| {
            // Subagents have no list of their own (todo_write is kept from them).
            if ctx.agent.is_none() && remind_after != 0.0 {
                let mut s = lock(&st);
                if ev.call.name == "todo_write" {
                    s.since_update = 0.0;
                } else if s.todos.iter().any(|t| t.status != "completed") {
                    s.since_update += 1.0;
                }
            }
            async { Ok(None) }
        });

        let st = state.clone();
        api.on::<hook::TurnEnd, _, _>(move |ev, ctx| {
            let mut s = lock(&st);
            // After a reply that called tools only: an inject after the final answer would start another turn.
            let out = if ctx.agent.is_some()
                || remind_after == 0.0
                || s.since_update < remind_after
                || !ev.message.content.iter().any(|b| matches!(b, AssistantContent::ToolCall(_)))
            {
                None
            } else {
                let list = s.todos.iter().map(|t| format!("{} {}", mark(&t.status), t.content)).collect::<Vec<_>>().join(", ");
                let calls = s.since_update;
                s.since_update = 0.0;
                Some(TurnEndResult {
                    inject: Some(harness_note(&format!(
                        "Your task list has not been updated for {calls} tool calls and still reads: {list}. If a step is done or another has started, call todo_write now to update it; otherwise carry on."
                    ))),
                    stop: None,
                })
            };
            async move { Ok(out) }
        });

        api.add_permission_rules(PermissionRules {
            allow: vec!["todo_write".into()],
            ..PermissionRules::default()
        })?;
        let (st, a) = (state, api.clone());
        api.register_tool(
            ToolDef::new(
                "todo_write",
                "Replace your task list for the current work. Useful for multi-step tasks: list the steps, keep exactly one in_progress, and mark steps completed as you finish them.",
                json!({
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "content": { "type": "string" },
                                    "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                                },
                                "required": ["content", "status"],
                                "additionalProperties": false,
                            },
                        },
                    },
                    "required": ["todos"],
                    "additionalProperties": false,
                }),
                ToolKind::Other,
                move |args, _ctx| {
                    let (st, a) = (st.clone(), a.clone());
                    async move {
                        let todos = parse(args.get("todos"));
                        lock(&st).todos = todos.clone();
                        a.session().append("todos", serde_json::to_value(&todos)?)?;
                        show(&a, &todos);
                        let list = todos.iter().map(|t| format!("{} {}", mark(&t.status), t.content)).collect::<Vec<_>>().join("\n");
                        let mut details = sasacode_ai::JsonObject::new();
                        details.insert("todos".into(), serde_json::to_value(&todos)?);
                        Ok(ToolResult::text(format!("Task list updated:\n{list}")).with_details(details))
                    }
                },
            )
            .always_load(true)
            .summary(|a| {
                let n = a.get("todos").and_then(Value::as_array).map_or(0, Vec::len);
                format!("{n} items")
            })
            .into_ref(),
        );
        api.ui().register_tool_renderer(
            "todo_write",
            Arc::new(|_call, result, _o| {
                let list = result.and_then(|r| r.details.as_ref()).and_then(|d| d.get("todos"))?;
                let list: Vec<Todo> = serde_json::from_value(list.clone()).ok()?;
                Some(
                    list.iter()
                        .map(|t| {
                            let text = if t.status == "completed" {
                                format!("\x1b[9m{}\x1b[29m", t.content)
                            } else {
                                t.content.clone()
                            };
                            format!("    {} {text}", mark(&t.status))
                        })
                        .collect(),
                )
            }),
        );
        Ok(())
    })
}
