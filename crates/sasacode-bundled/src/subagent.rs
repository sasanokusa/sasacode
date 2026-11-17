//! subagent (`packages/bundled/src/subagent.ts`). `task`: hand a self-contained job to a fresh
//! agent loop. In the foreground the caller waits for the report; with `background` it keeps
//! working and the report arrives as a message when the run ends. task_status / task_send /
//! task_stop let the caller watch, steer and stop runs; `/task` does the same for the user.
//! Interrupting the caller (Esc) stops its background runs too.

use std::sync::{Arc, Mutex, MutexGuard};

use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use sasacode_ai::now_ms;
use sasacode_host::t;
use sasacode_plugin_api::{
    Command, InjectWhen, NotifyLevel, OnUpdate, PermissionRules, PluginApi, SelectOption, StopCause, SubagentControl,
    SubagentOptions, ToolDef, ToolKind, ToolResult, hook,
};

use crate::repair::js::{js_space, js_string, js_trim, truthy};

/// A subagent may not steer other subagents, nor touch the caller's TODO list or goal.
pub const SUBAGENT_EXCLUDED: &[&str] = &[
    "task",
    "task_status",
    "task_send",
    "task_stop",
    "todo_write",
    "set_goal",
];

#[derive(Clone, Default)]
struct Run {
    id: String,
    description: String,
    background: bool,
    started: i64,
    ended: Option<i64>,
    /// Tool calls so far, newest last.
    trail: Vec<String>,
    status: &'static str,
    /// Stopped because the caller was interrupted (Esc): the report waits for the next message.
    interrupted: bool,
    /// Its session ended (/new, /resume, exit): its report belongs to that conversation, not to the next one.
    orphaned: bool,
    report: Option<String>,
    control: Option<SubagentControl>,
}

const TRAIL: usize = 12;

/// `Math.round`.
fn round(x: f64) -> f64 {
    (x + 0.5).floor()
}

fn ago(ms: i64) -> String {
    let ms = ms as f64;
    if ms < 60_000.0 {
        format!("{}s", round(ms / 1000.0))
    } else {
        format!("{}m{}s", (ms / 60_000.0).floor(), round((ms % 60_000.0) / 1000.0))
    }
}

fn tail(trail: &[String]) -> &[String] {
    &trail[trail.len().saturating_sub(TRAIL)..]
}

/// What the caller gets back: the report, and when it did not finish, why and how far it got.
pub fn report_of(text: &str, cause: StopCause, error: Option<&str>, trail: &[String]) -> String {
    if cause == StopCause::Done {
        return if text.is_empty() {
            "(the subagent finished without a report)".to_string()
        } else {
            text.to_string()
        };
    }
    let why = if cause == StopCause::Error {
        format!("error: {}", error.unwrap_or("unknown"))
    } else {
        cause.as_str().to_string()
    };
    [
        format!("[subagent did not finish: {why}]"),
        if text.is_empty() {
            "It wrote no message.".to_string()
        } else {
            format!("Its last message:\n{text}")
        },
        if trail.is_empty() {
            "It made no tool calls.".to_string()
        } else {
            format!(
                "Its last tool calls:\n{}",
                tail(trail)
                    .iter()
                    .map(|t| format!("- {t}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        },
    ]
    .join("\n\n")
}

fn line(r: &Run) -> String {
    format!(
        "{} [{}{}] {} — {}, {} tool calls{}",
        r.id,
        r.status,
        if r.background { ", background" } else { "" },
        r.description,
        ago(r.ended.unwrap_or_else(now_ms) - r.started),
        r.trail.len(),
        r.trail.last().map(|l| format!(", last: {l}")).unwrap_or_default()
    )
}

#[derive(Default)]
struct State {
    runs: IndexMap<String, Run>,
    seq: u64,
}

type Shared = Arc<Mutex<State>>;

fn lock(s: &Shared) -> MutexGuard<'_, State> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

fn find(state: &Shared, id: &str) -> anyhow::Result<Run> {
    let s = lock(state);
    match s.runs.get(id) {
        Some(r) => Ok(r.clone()),
        None => {
            let known = s.runs.keys().cloned().collect::<Vec<_>>().join(", ");
            anyhow::bail!(
                "no task {id} (known: {})",
                if known.is_empty() { "none" } else { &known }
            )
        }
    }
}

struct Args {
    description: String,
    prompt: String,
    model: Option<String>,
}

/// Starts a run; the future ends with the finished run.
fn start(
    api: &PluginApi,
    state: &Shared,
    args: Args,
    background: bool,
    signal: Option<CancellationToken>,
    on_update: Option<OnUpdate>,
) -> (String, BoxFuture<'static, Run>) {
    let id = {
        let mut s = lock(state);
        s.seq += 1;
        let id = format!("t{}", s.seq);
        s.runs.insert(
            id.clone(),
            Run {
                id: id.clone(),
                description: args.description,
                background,
                started: now_ms(),
                ended: None,
                trail: Vec::new(),
                status: "running",
                interrupted: false,
                orphaned: false,
                report: None,
                control: None,
            },
        );
        id
    };
    let (s1, s2, s3) = (state.clone(), state.clone(), state.clone());
    let (i1, i2, i3) = (id.clone(), id.clone(), id.clone());
    let options = SubagentOptions {
        prompt: format!(
            "{}\n\nWhen done, reply with a concise report of what you found or did; it is all the caller will see. The caller may send you messages while you work; follow them.",
            args.prompt
        ),
        model: args.model,
        exclude_tools: SUBAGENT_EXCLUDED.iter().map(|s| s.to_string()).collect(),
        signal,
        on_start: Some(Arc::new(move |c| {
            if let Some(r) = lock(&s1).runs.get_mut(&i1) {
                r.control = Some(c);
            }
        })),
        on_progress: Some(Arc::new(move |t: String| {
            if let Some(r) = lock(&s2).runs.get_mut(&i2) {
                r.trail.push(t.clone());
            }
            if let Some(u) = &on_update {
                u(format!("{t}\n"));
            }
        })),
        ..SubagentOptions::default()
    };
    let api = api.clone();
    let done = Box::pin(async move {
        let (text, cause, error) = match api.agent().run(options).await {
            Ok(r) => (r.text, r.cause, r.error),
            Err(e) => (String::new(), StopCause::Error, Some(format!("{e:#}"))),
        };
        let mut s = lock(&s3);
        // Runs are never taken out of the table.
        let run = s.runs.entry(i3.clone()).or_insert_with(|| Run {
            id: i3,
            ..Run::default()
        });
        run.ended = Some(now_ms());
        run.status = match cause {
            StopCause::Done => "done",
            StopCause::Aborted => "stopped",
            _ => "failed",
        };
        run.report = Some(report_of(&text, cause, error.as_deref(), &run.trail));
        run.control = None;
        run.clone()
    }) as BoxFuture<'static, Run>;
    (id, done)
}

fn id_of(args: &sasacode_ai::JsonObject) -> String {
    args.get("id")
        .filter(|v| !v.is_null())
        .map(js_string)
        .unwrap_or_default()
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let state: Shared = Arc::default();

        // The subagent's own tool calls go through permissions, so starting or watching one needs no approval.
        api.add_permission_rules(PermissionRules {
            allow: ["task", "task_status", "task_send", "task_stop"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ..PermissionRules::default()
        })?;

        let (st, a) = (state.clone(), api.clone());
        api.register_tool(
            ToolDef::new(
                "task",
                "Run a subagent with a fresh context on a self-contained task (research, a broad search, an independent change). It has the same tools except task, todo_write and set_goal. Several task calls in one turn run in parallel. By default you wait for its report; with background: true you get its id at once, keep working, and its report arrives as a message when it ends (watch it with task_status, steer it with task_send, stop it with task_stop).",
                json!({
                    "type": "object",
                    "properties": {
                        "description": { "type": "string", "description": "Short label (3-6 words)" },
                        "prompt": { "type": "string", "description": "Complete instructions; the subagent sees nothing else" },
                        "model": { "type": "string", "description": "Optional \"<provider>/<model>\"" },
                        "background": { "type": "boolean", "description": "Return at once and deliver the report later" },
                    },
                    "required": ["description", "prompt"],
                    "additionalProperties": false,
                }),
                ToolKind::Other,
                move |args, ctx| {
                    let (st, a) = (st.clone(), a.clone());
                    async move {
                        let parsed = Args {
                            description: args.get("description").map(js_string).unwrap_or_default(),
                            prompt: args.get("prompt").map(js_string).unwrap_or_default(),
                            model: args.get("model").and_then(Value::as_str).map(str::to_string),
                        };
                        if !args.get("background").is_some_and(truthy) {
                            let (_, done) = start(&a, &st, parsed, false, Some(ctx.signal.clone()), ctx.on_update.clone());
                            let run = done.await;
                            let mut r = ToolResult::text(run.report.unwrap_or_default());
                            r.is_error = Some(run.status == "failed");
                            return Ok(r);
                        }
                        // Not tied to this turn's signal, which ends with the turn: agent_end below stops it on Esc.
                        let (id, done) = start(&a, &st, parsed, true, None, None);
                        let a2 = a.clone();
                        tokio::spawn(async move {
                            let r = done.await;
                            a2.ui().notify(
                                &t!(
                                    "task {id}「{description}」が終了しました（{status}）",
                                    id = r.id,
                                    description = r.description,
                                    status = r.status
                                ),
                                if r.status == "done" { NotifyLevel::Info } else { NotifyLevel::Warning },
                            );
                            // After an interruption the user decides what comes next: the report goes with their next message.
                            if !r.orphaned {
                                a2.session().inject_text(
                                    &format!(
                                        "[task {} \"{}\" ended: {}]\n\n{}",
                                        r.id,
                                        r.description,
                                        r.status,
                                        r.report.as_deref().unwrap_or_default()
                                    ),
                                    if r.interrupted { InjectWhen::Next } else { InjectWhen::Now },
                                );
                            }
                        });
                        Ok(ToolResult::text(format!(
                            "Started {id} in the background. Its report will arrive as a message; use task_status / task_send / task_stop with id \"{id}\"."
                        )))
                    }
                },
            )
            .concurrent(true)
            .always_load(true)
            .summary(|a| {
                format!(
                    "{}{}",
                    a.get("description").map(js_string).unwrap_or_else(|| "undefined".into()),
                    if a.get("background").is_some_and(truthy) { " (background)" } else { "" }
                )
            })
            .into_ref(),
        );

        // Esc on the caller stops what it started in the background (a subagent's own end does not).
        let st = state.clone();
        api.on::<hook::AgentEnd, _, _>(move |e, wh| {
            if wh.agent.is_none() && e.cause == StopCause::Aborted {
                let stops: Vec<SubagentControl> = lock(&st)
                    .runs
                    .values_mut()
                    .filter(|r| r.background && r.control.is_some())
                    .filter_map(|r| {
                        r.interrupted = true;
                        r.control.clone()
                    })
                    .collect();
                for c in stops {
                    (c.stop)();
                }
            }
            async { Ok(None) }
        });

        // A switch to another conversation stops them too: their reports started the new one's model unasked (#122).
        let st = state.clone();
        api.on::<hook::SessionEnd, _, _>(move |_e, _c| {
            let stops: Vec<SubagentControl> = lock(&st)
                .runs
                .values_mut()
                .filter(|r| r.background && r.control.is_some())
                .filter_map(|r| {
                    r.orphaned = true;
                    r.control.clone()
                })
                .collect();
            for c in stops {
                (c.stop)();
            }
            async { Ok(None) }
        });

        let st = state.clone();
        api.register_tool(
            ToolDef::new(
                "task_status",
                "List subagent runs, or show one run's progress (its recent tool calls, and its report once it has ended).",
                json!({ "type": "object", "properties": { "id": { "type": "string", "description": "Omit to list all runs" } }, "additionalProperties": false }),
                ToolKind::Read,
                move |args, _ctx| {
                    let st = st.clone();
                    async move {
                        let id = id_of(&args);
                        if id.is_empty() {
                            let list = lock(&st).runs.values().map(line).collect::<Vec<_>>().join("\n");
                            return Ok(ToolResult::text(if list.is_empty() { "no subagent runs".to_string() } else { list }));
                        }
                        let r = find(&st, &id)?;
                        let text = [
                            line(&r),
                            if r.trail.is_empty() {
                                String::new()
                            } else {
                                format!(
                                    "Recent tool calls:\n{}",
                                    tail(&r.trail).iter().map(|t| format!("- {t}")).collect::<Vec<_>>().join("\n")
                                )
                            },
                            r.report.as_ref().filter(|x| !x.is_empty()).map(|x| format!("Report:\n{x}")).unwrap_or_default(),
                        ];
                        Ok(ToolResult::text(text.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n")))
                    }
                },
            )
            .concurrent(true)
            .summary(|a| a.get("id").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "all".into()))
            .into_ref(),
        );

        let st = state.clone();
        api.register_tool(
            ToolDef::new(
                "task_send",
                "Send a message to a running background subagent (a correction, extra information, or \"wrap up now\"). It sees it at its next turn.",
                json!({
                    "type": "object",
                    "properties": { "id": { "type": "string", "description": "Task id, e.g. \"t1\"" }, "message": { "type": "string" } },
                    "required": ["id", "message"],
                    "additionalProperties": false,
                }),
                ToolKind::Other,
                move |args, _ctx| {
                    let st = st.clone();
                    async move {
                        let r = find(&st, &id_of(&args))?;
                        let Some(c) = r.control else {
                            anyhow::bail!("{} has already ended ({}); start a new task instead", r.id, r.status);
                        };
                        let message = args.get("message").map(js_string).unwrap_or_default();
                        (c.send)(format!("[message from the caller]\n{message}"));
                        Ok(ToolResult::text(format!("sent to {}", r.id)))
                    }
                },
            )
            .summary(id_of)
            .into_ref(),
        );

        let st = state.clone();
        api.register_tool(
            ToolDef::new(
                "task_stop",
                "Stop a running subagent. Its partial progress is reported as usual.",
                json!({
                    "type": "object",
                    "properties": { "id": { "type": "string", "description": "Task id, e.g. \"t1\"" } },
                    "required": ["id"],
                    "additionalProperties": false,
                }),
                ToolKind::Other,
                move |args, _ctx| {
                    let st = st.clone();
                    async move {
                        let r = find(&st, &id_of(&args))?;
                        let Some(c) = r.control else {
                            return Ok(ToolResult::text(format!("{} has already ended ({})", r.id, r.status)));
                        };
                        (c.stop)();
                        Ok(ToolResult::text(format!("stopping {}", r.id)))
                    }
                },
            )
            .summary(id_of)
            .into_ref(),
        );

        let (st, st2, a) = (state.clone(), state, api.clone());
        api.register_command(
            Command::new(
                "task",
                &t!("サブエージェントの一覧・詳細・指示・停止"),
                move |c| {
                    let (st, a) = (st.clone(), a.clone());
                    async move {
                        let words: Vec<String> = js_trim(&c.args)
                            .split(js_space)
                            .filter(|w| !w.is_empty())
                            .map(str::to_string)
                            .collect();
                        let (sub, id, rest) = (words.first(), words.get(1), words.get(2..).unwrap_or_default());
                        let ui = a.ui();
                        let outcome: anyhow::Result<()> = (|| {
                            match sub.map(String::as_str) {
                                None => {
                                    let s = lock(&st);
                                    let text = if s.runs.is_empty() {
                                        t!("サブエージェントはまだ動いていません")
                                    } else {
                                        s.runs.values().map(line).collect::<Vec<_>>().join("\n")
                                    };
                                    drop(s);
                                    ui.notify(&text, NotifyLevel::Info);
                                }
                                Some("stop") => {
                                    let r = find(&st, id.map_or("", String::as_str))?;
                                    match r.control {
                                        Some(c) => {
                                            (c.stop)();
                                            ui.notify(&t!("{id} を停止します", id = r.id), NotifyLevel::Info);
                                        }
                                        None => ui.notify(
                                            &t!("{id} は既に終了しています ({status})", id = r.id, status = r.status),
                                            NotifyLevel::Info,
                                        ),
                                    }
                                }
                                Some("send") => {
                                    let r = find(&st, id.map_or("", String::as_str))?;
                                    match r.control {
                                        None => ui.notify(
                                            &t!("{id} は既に終了しています ({status})", id = r.id, status = r.status),
                                            NotifyLevel::Warning,
                                        ),
                                        Some(_) if rest.is_empty() => ui.notify(
                                            &t!("送る内容がありません: /task send <id> <message>"),
                                            NotifyLevel::Warning,
                                        ),
                                        Some(c) => {
                                            (c.send)(format!("[message from the user]\n{}", rest.join(" ")));
                                            ui.notify(&t!("{id} に送りました", id = r.id), NotifyLevel::Info);
                                        }
                                    }
                                }
                                Some(other) => {
                                    let r = find(&st, other)?;
                                    let mut parts = vec![line(&r)];
                                    parts.extend(tail(&r.trail).iter().map(|t| format!("  {t}")));
                                    if let Some(rep) = r.report.as_ref().filter(|x| !x.is_empty()) {
                                        parts.push(format!("\n{rep}"));
                                    }
                                    ui.notify(&parts.join("\n"), NotifyLevel::Info);
                                }
                            }
                            Ok(())
                        })();
                        if let Err(e) = outcome {
                            ui.notify(&e.to_string(), NotifyLevel::Warning);
                        }
                        Ok(())
                    }
                },
            )
            .hint("[id] | send <id> <message> | stop <id>")
            .complete(move |prefix| {
                let s = lock(&st2);
                let mut values: Vec<String> = s.runs.keys().cloned().collect();
                values.extend(["send".to_string(), "stop".to_string()]);
                let out: Vec<SelectOption> = values
                    .into_iter()
                    .filter(|v| v.starts_with(&prefix))
                    .map(|v| {
                        let d = s.runs.get(&v).map(|r| r.description.clone()).unwrap_or_default();
                        SelectOption::new(v.clone(), v).describe(d)
                    })
                    .collect();
                async move { out }
            }),
        );
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_rounds_like_math_round() {
        assert_eq!(ago(1499), "1s");
        assert_eq!(ago(1500), "2s");
        assert_eq!(ago(61_500), "1m2s");
    }
}
