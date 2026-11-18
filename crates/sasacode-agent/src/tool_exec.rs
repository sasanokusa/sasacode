//! Everything between "the model asked for tools" and "results go back" (`packages/agent/src/tool-exec.ts`):
//! repair, lookup, validation, plugin decisions, permission, parallel execution.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures::FutureExt;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use sasacode_ai::{AssistantContent, AssistantMessage, JsonObject, ToolCall, ToolResultMessage, UserContent, now_ms};
use sasacode_plugin_api::{
    CallTool, Decision, HookName, HookRunner, PermissionRules, ToolContext, ToolRef, ToolResult, Verdict,
    VerdictSource, error_result as err, hook, validate,
};

use crate::events::{AgentEvent, EventBus, RepairOrigin};
use crate::permission::{PermissionCheck, PermissionPolicy};
use crate::session::{RepairOriginal, Repaired, SessionEntry, SessionFile};

/// A tool call put to the user.
#[derive(Clone)]
pub struct ApprovalRequest {
    pub tool: ToolRef,
    pub args: JsonObject,
    pub cwd: String,
    /// Cancelled when the calling run is interrupted: the question is obsolete.
    pub signal: CancellationToken,
    pub call: ToolCall,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalDecision {
    Allow,
    Always,
    Deny,
}

/// `args`: what the user edited the call to (allow / always only); it runs with them, checked again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalAnswer {
    pub decision: ApprovalDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<JsonObject>,
}

impl ApprovalAnswer {
    pub fn new(decision: ApprovalDecision) -> Self {
        ApprovalAnswer {
            decision,
            feedback: None,
            args: None,
        }
    }
}

/// Asks the user about a tool call. An `Err` (the UI went away) means the call does not run.
pub type ApproveFn = Arc<dyn Fn(ApprovalRequest) -> BoxFuture<'static, anyhow::Result<ApprovalAnswer>> + Send + Sync>;
pub type FindTool = Arc<dyn Fn(&str) -> Option<ToolRef> + Send + Sync>;
pub type AllTools = Arc<dyn Fn() -> Vec<ToolRef> + Send + Sync>;
/// Where a finished result goes (the agent stores it); an `Err` (the log could not be written) ends the run.
pub type OnResult = Arc<dyn Fn(ToolResultMessage) -> anyhow::Result<()> + Send + Sync>;
/// The tool calls running now, each with its own token (Agent::abort_call).
pub type RunningCalls = Arc<Mutex<HashMap<String, CancellationToken>>>;

pub struct ToolRunContext {
    pub cwd: String,
    pub hooks: HookRunner,
    pub events: EventBus,
    pub permissions: Arc<PermissionPolicy>,
    pub approve: Option<ApproveFn>,
    pub session: Option<Arc<SessionFile>>,
    pub find_tool: FindTool,
    /// Every tool the model could call, deferred ones included (repair candidates).
    pub all_tools: AllTools,
    /// Where each running call's own token is kept while it runs.
    pub calls: Option<RunningCalls>,
}

impl ToolRunContext {
    fn log(&self, entry: SessionEntry) -> anyhow::Result<()> {
        match &self.session {
            Some(s) => s.append(&entry),
            None => Ok(()),
        }
    }
}

/// The call as it will run, after any tool_call_raw repair.
#[derive(Debug, Clone, PartialEq)]
pub struct Repair {
    pub name: String,
    pub input: JsonObject,
    pub raw_input: Option<String>,
    pub note: String,
}

fn not_run() -> ToolResult {
    err("Interrupted by the user before this tool ran.")
}

/// Let tool_call_raw hooks fix names and arguments before anything checks them. The message is
/// rewritten to the repaired call (the model imitates its own history), unless it carries
/// provider-signed reasoning, where editing an earlier turn is rejected by the API. What the model
/// really produced is kept in the session as a tool_repair entry.
pub async fn repair_calls(
    ctx: &ToolRunContext,
    message: &mut AssistantMessage,
) -> anyhow::Result<HashMap<String, Repair>> {
    let mut repairs = HashMap::new();
    // A call cut off by max_tokens must never be "completed" into something runnable.
    if message.stop_reason == sasacode_ai::StopReason::MaxTokens || !ctx.hooks.has(HookName::ToolCallRaw) {
        return Ok(repairs);
    }
    let rewrite = !message
        .content
        .iter()
        .any(|b| matches!(b, AssistantContent::Thinking(t) if t.signature.as_deref().is_some_and(|s| !s.is_empty())));
    for i in 0..message.content.len() {
        let AssistantContent::ToolCall(call) = &message.content[i] else {
            continue;
        };
        let call = call.clone();
        let mut name = call.name.clone();
        let mut input = call.input.clone();
        let mut raw_input = call.raw_input.clone();
        // JS compared the input object by identity: "changed" is "a handler returned one".
        let mut input_replaced = false;
        let mut notes: Vec<String> = Vec::new();
        let event = sasacode_plugin_api::ToolCallRawEvent {
            call: call.clone(),
            name: name.clone(),
            input: input.clone(),
            raw_input: raw_input.clone(),
            tools: (ctx.all_tools)(),
            stop_reason: message.stop_reason,
        };
        ctx.hooks
            .run::<hook::ToolCallRaw>(event, |r, ev, _| {
                if let Some(n) = r.name.filter(|n| !n.is_empty()) {
                    name = n.clone();
                    ev.name = n;
                }
                if let Some(inp) = r.input {
                    input = inp.clone();
                    ev.input = inp;
                    input_replaced = true;
                    raw_input = None;
                    ev.raw_input = None;
                }
                if let Some(n) = r.note.filter(|n| !n.is_empty()) {
                    notes.push(n);
                }
                true
            })
            .await;
        // A note alone (no change) still reaches the model with the result (#85); the stored call stays as it was.
        let changed = name != call.name || input_replaced || raw_input != call.raw_input;
        if !changed && notes.is_empty() {
            continue;
        }
        let note = if !notes.is_empty() {
            notes.join("; ")
        } else if name != call.name {
            format!("tool \"{}\" → \"{name}\"", call.name)
        } else {
            "arguments repaired".to_string()
        };
        let original = RepairOrigin {
            name: call.name.clone(),
            input: call.input.clone(),
            raw_input: call.raw_input.clone(),
        };
        ctx.log(SessionEntry::ToolRepair {
            call_id: call.id.clone(),
            original: RepairOriginal {
                name: call.name.clone(),
                input: Value::Object(call.input.clone()),
                raw_input: call.raw_input.clone(),
            },
            repaired: Repaired {
                name: name.clone(),
                input: Value::Object(input.clone()),
            },
            note: note.clone(),
        })?;
        ctx.events.emit(AgentEvent::ToolRepaired {
            call: call.clone(),
            from: original,
            note: note.clone(),
        });
        repairs.insert(
            call.id.clone(),
            Repair {
                name: name.clone(),
                input: input.clone(),
                raw_input: raw_input.clone(),
                note,
            },
        );
        if rewrite
            && changed
            && let AssistantContent::ToolCall(stored) = &mut message.content[i]
        {
            stored.name = name;
            stored.input = input;
            stored.raw_input = None;
        }
    }
    Ok(repairs)
}

pub struct ExecOptions {
    pub truncated: bool,
    pub signal: CancellationToken,
    pub repairs: HashMap<String, Repair>,
    pub on_result: OnResult,
    pub ancestors: Vec<String>,
    pub parent_call_id: Option<String>,
    pub on_tool_result: Option<Arc<dyn Fn(ToolResult) + Send + Sync>>,
}

struct Job {
    call: ToolCall,
    name: String,
    /// Told to the model before the result: what was repaired, or edited.
    note: Mutex<Option<String>>,
}

/// What every finish of one execute_tools shares: results are stored here and flushed in call order.
struct Exec {
    jobs: Vec<Job>,
    flush: Mutex<Flush>,
    on_result: OnResult,
    on_tool_result: Option<Arc<dyn Fn(ToolResult) + Send + Sync>>,
    parent_call_id: Option<String>,
}

struct Flush {
    messages: Vec<Option<ToolResultMessage>>,
    flushed: usize,
}

async fn finish(ctx: &ToolRunContext, ex: &Exec, i: usize, result: ToolResult, did_run: bool) -> anyhow::Result<()> {
    let job = &ex.jobs[i];
    let mut result = result;
    ctx.hooks
        .run::<hook::ToolResult>(
            sasacode_plugin_api::ToolResultEvent {
                call: job.call.clone(),
                result: result.clone(),
                ran: did_run,
            },
            |r, ev, _| {
                if let Some(res) = r.result {
                    result = res.clone();
                    ev.result = res;
                }
                true
            },
        )
        .await;
    if did_run || ex.parent_call_id.is_some() {
        ctx.events.emit(AgentEvent::ToolEnd {
            call: job.call.clone(),
            result: result.clone(),
            parent_call_id: ex.parent_call_id.clone(),
        });
    }
    if let Some(f) = &ex.on_tool_result {
        f(result.clone());
    }
    // Tell the model briefly what was fixed (or edited), so it can produce the right form next time.
    let mut content: Vec<UserContent> = Vec::new();
    if let Some(note) = job.note.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        content.push(UserContent::text(note));
    }
    content.extend(result.content.iter().cloned());
    let message = ToolResultMessage {
        tool_call_id: job.call.id.clone(),
        tool_name: job.name.clone(),
        content,
        is_error: result.is_error(),
        timestamp: now_ms(),
    };
    // Store and drain in one critical section: two finishes may race.
    let mut f = ex.flush.lock().unwrap_or_else(|e| e.into_inner());
    f.messages[i] = Some(message);
    while f.flushed < f.messages.len() {
        let k = f.flushed;
        let Some(m) = f.messages[k].take() else { break };
        f.flushed += 1;
        (ex.on_result)(m)?;
    }
    Ok(())
}

/// Runs the calls of one response. Results are passed to `on_result` in call order, each as soon
/// as it and those before it are final, so a crash loses at most the calls in flight. A call is
/// approved right before it runs, never all at once: what the user is asked about is what happens.
pub fn execute_tools(
    ctx: Arc<ToolRunContext>,
    calls: Vec<ToolCall>,
    opts: ExecOptions,
) -> BoxFuture<'static, anyhow::Result<()>> {
    async move {
        let mut jobs = Vec::with_capacity(calls.len());
        let mut inputs = Vec::with_capacity(calls.len());
        let mut raw_inputs = Vec::with_capacity(calls.len());
        for call in calls {
            let fixed = opts.repairs.get(&call.id);
            inputs.push(fixed.map(|f| f.input.clone()).unwrap_or_else(|| call.input.clone()));
            raw_inputs.push(match fixed {
                Some(f) => f.raw_input.clone(),
                None => call.raw_input.clone(),
            });
            jobs.push(Job {
                name: fixed.map(|f| f.name.clone()).unwrap_or_else(|| call.name.clone()),
                note: Mutex::new(fixed.map(|f| format!("[harness repaired this call: {}]", f.note))),
                call,
            });
        }
        let n = jobs.len();
        let ex = Arc::new(Exec {
            jobs,
            flush: Mutex::new(Flush {
                messages: (0..n).map(|_| None).collect(),
                flushed: 0,
            }),
            on_result: opts.on_result.clone(),
            on_tool_result: opts.on_tool_result.clone(),
            parent_call_id: opts.parent_call_id.clone(),
        });

        // Validation runs one call at a time, in order; permission prompts do not (below).
        let mut tools: Vec<Option<ToolRef>> = vec![None; n];
        for i in 0..n {
            let name = &ex.jobs[i].name;
            let rejected = if opts.ancestors.contains(name) {
                let mut chain = opts.ancestors.clone();
                chain.push(name.clone());
                Some(err(format!("Nested tool cycle: {}", chain.join(" -> "))))
            } else if opts.ancestors.len() >= 8 {
                Some(err("Nested tool depth exceeds 8."))
            } else {
                match preflight(
                    &ctx,
                    name,
                    &inputs[i],
                    raw_inputs[i].as_deref(),
                    opts.truncated,
                    &opts.signal,
                ) {
                    Ok(tool) => {
                        tools[i] = Some(tool);
                        None
                    }
                    Err(r) => Some(r),
                }
            };
            if let Some(r) = rejected {
                finish(&ctx, &ex, i, r, false).await?;
            }
        }

        // "Approve → run" one call at a time for tools that cannot run in parallel, so the user sees
        // the previous call's result before deciding on the next one. Concurrency-safe tools run as
        // soon as they are approved, together with the ones already in flight.
        let mut running: Vec<tokio::task::JoinHandle<anyhow::Result<()>>> = Vec::new();
        for i in 0..n {
            let Some(tool) = tools[i].clone() else { continue };
            if !tool.concurrent() && !running.is_empty() {
                join_all(std::mem::take(&mut running)).await?;
            }
            if opts.signal.is_cancelled() {
                finish(&ctx, &ex, i, not_run(), false).await?;
                continue;
            }
            // Approvals stay one at a time, in order: only the runs overlap.
            let call = ex.jobs[i].call.clone();
            let (args, denied, edited) = authorize(&ctx, &tool, &call, inputs[i].clone(), &opts.signal).await?;
            if edited {
                // The stored call keeps what the model asked for: say what ran instead.
                let told = format!(
                    "[the user edited the arguments; the call ran with {}]",
                    sasacode_ai::json::stringify(&Value::Object(args.clone()))
                );
                let mut note = ex.jobs[i].note.lock().unwrap_or_else(|e| e.into_inner());
                *note = Some(note.take().map_or(told.clone(), |n| format!("{n}\n{told}")));
            }
            let concurrent = tool.concurrent();
            let run = {
                let (ctx, ex, signal) = (ctx.clone(), ex.clone(), opts.signal.clone());
                let (ancestors, parent) = (opts.ancestors.clone(), opts.parent_call_id.clone());
                async move {
                    if let Some(d) = denied {
                        return finish(&ctx, &ex, i, d, false).await;
                    }
                    // A call must not start once the user has interrupted.
                    if signal.is_cancelled() {
                        return finish(&ctx, &ex, i, not_run(), false).await;
                    }
                    let result = run_tool(&ctx, &tool, &call, args, &signal, &ancestors, parent).await?;
                    finish(&ctx, &ex, i, result, true).await
                }
            };
            if concurrent {
                running.push(tokio::spawn(run));
            } else {
                run.await?;
            }
        }
        join_all(running).await
    }
    .boxed()
}

async fn join_all(handles: Vec<tokio::task::JoinHandle<anyhow::Result<()>>>) -> anyhow::Result<()> {
    let mut first: anyhow::Result<()> = Ok(());
    for r in futures::future::join_all(handles).await {
        let r = r.map_err(|e| anyhow::anyhow!("tool task failed: {e}")).and_then(|r| r);
        if first.is_ok() {
            first = r;
        }
    }
    first
}

/// Checks that need no user: an error result when the call cannot run.
fn preflight(
    ctx: &ToolRunContext,
    name: &str,
    input: &JsonObject,
    raw_input: Option<&str>,
    truncated: bool,
    signal: &CancellationToken,
) -> Result<ToolRef, ToolResult> {
    if signal.is_cancelled() {
        return Err(not_run());
    }
    if truncated {
        return Err(err(
            "The response hit max_tokens before this tool call was complete, so it was not run. Retry with smaller input.",
        ));
    }
    let Some(tool) = (ctx.find_tool)(name) else {
        let names: Vec<String> = (ctx.all_tools)().iter().map(|t| t.name().to_string()).collect();
        return Err(err(format!("Unknown tool \"{name}\". Available: {}", names.join(", "))));
    };
    if let Some(raw) = raw_input {
        return Err(err(format!(
            "Tool input was not valid JSON, so it was not run: {}",
            sasacode_ai::json::stringify(&json!({ "INVALID_JSON": raw }))
        )));
    }
    let problems = validate(&tool.parameters(), &Value::Object(input.clone()), "input");
    if !problems.is_empty() {
        return Err(err(format!("Invalid arguments: {}", problems.join("; "))));
    }
    Ok(tool)
}

/// A tool_call plugin's decision replaces the permission mode's default, but never the user's
/// explicit deny/ask rules: the stricter of the two wins. Then permission hooks may change the
/// verdict within limits (see lowest_for).
async fn decide(
    ctx: &ToolRunContext,
    check: &PermissionCheck,
    call: &ToolCall,
    forced: Option<(Decision, String)>,
) -> Verdict {
    let policy = ctx.permissions.check(check);
    let mut v = policy.clone();
    if let Some((decision, reason)) = forced
        && !(policy.source != VerdictSource::Mode && policy.decision.rank() > decision.rank())
    {
        v = Verdict {
            decision,
            reason,
            source: VerdictSource::Plugin,
        };
    }
    if ctx.hooks.has(HookName::Permission) {
        let lowest = lowest_for(&v);
        let source = v.source;
        let event = sasacode_plugin_api::PermissionEvent {
            call: call.clone(),
            tool: check.tool.clone(),
            args: check.args.clone(),
            cwd: PathBuf::from(&check.cwd),
            mode: ctx.permissions.mode(),
            verdict: v.clone(),
            lowest,
        };
        ctx.hooks
            .run::<hook::Permission>(event, |r, ev, _| {
                let Some(d) = r.decision else { return true };
                let decision = if d.rank() < lowest.rank() { lowest } else { d };
                v = Verdict {
                    decision,
                    reason: r.reason.unwrap_or_else(|| v.reason.clone()),
                    source,
                };
                ev.verdict = v.clone();
                true
            })
            .await;
    }
    v
}

/// How far a permission hook may loosen a verdict: a mode default all the way; a softDeny rule
/// only as far as asking the user; the user's rules and other plugins' decisions not at all.
fn lowest_for(v: &Verdict) -> Decision {
    match v.source {
        VerdictSource::Mode => Decision::Allow,
        VerdictSource::Soft => Decision::Ask,
        _ => v.decision,
    }
}

/// Plugins see the call first (tool_call hook), then the core policy and the user decide.
async fn authorize(
    ctx: &ToolRunContext,
    tool: &ToolRef,
    call: &ToolCall,
    input: JsonObject,
    signal: &CancellationToken,
) -> anyhow::Result<(JsonObject, Option<ToolResult>, bool)> {
    let denied = |args: JsonObject, r: ToolResult| Ok((args, Some(r), false));
    let mut args = input;
    let mut forced: Option<(Decision, String)> = None;
    let event = sasacode_plugin_api::ToolCallEvent {
        call: call.clone(),
        tool: tool.clone(),
        args: args.clone(),
    };
    ctx.hooks
        .run::<hook::ToolCall>(event, |r, ev, _| {
            if let Some(a) = r.args {
                args = a.clone();
                ev.args = a;
            }
            if let Some(d) = r.decision
                && forced.as_ref().is_none_or(|(f, _)| d.rank() > f.rank())
            {
                forced = Some((d, r.reason.unwrap_or_else(|| "plugin".into())));
            }
            forced.as_ref().is_none_or(|(f, _)| *f != Decision::Deny)
        })
        .await;
    // A hook may rewrite args: validate the values that will actually be authorized and executed.
    let problems = validate(&tool.parameters(), &Value::Object(args.clone()), "input");
    if !problems.is_empty() {
        let e = err(format!("Invalid arguments after tool_call: {}", problems.join("; ")));
        return denied(args, e);
    }
    let check = PermissionCheck {
        tool: tool.clone(),
        args: args.clone(),
        cwd: ctx.cwd.clone(),
    };
    if signal.is_cancelled() {
        return denied(args, not_run());
    }
    let verdict = decide(ctx, &check, call, forced).await;
    if signal.is_cancelled() {
        return denied(args, not_run());
    }
    match verdict.decision {
        Decision::Allow => return Ok((args, None, false)),
        Decision::Deny => return denied(args, err(format!("Permission denied ({}).", verdict.reason))),
        Decision::Ask => {}
    }
    let Some(approve) = ctx.approve.clone() else {
        let e = err(format!(
            "This call needs user approval ({}), but no user is available to approve it (non-interactive run). It was not executed.",
            verdict.reason
        ));
        return denied(args, e);
    };
    // Esc while the question is open answers it: the call does not run. A failed question (the UI
    // went away) is not run either, like every other failure here a result.
    let request = ApprovalRequest {
        tool: tool.clone(),
        args: args.clone(),
        cwd: ctx.cwd.clone(),
        signal: signal.clone(),
        call: call.clone(),
        reason: verdict.reason.clone(),
    };
    let answer = tokio::select! {
        a = approve(request) => Some(a),
        _ = signal.cancelled() => None,
    };
    let answer = match answer {
        Some(Err(e)) => {
            let e = err(format!(
                "The approval could not be asked ({e:#}), so the call was not run."
            ));
            return denied(args, e);
        }
        Some(Ok(a)) if !signal.is_cancelled() => a,
        _ => return denied(args, not_run()),
    };
    if answer.decision == ApprovalDecision::Deny {
        let feedback = answer
            .feedback
            .filter(|f| !f.is_empty())
            .map(|f| format!(" User feedback: {f}"))
            .unwrap_or_default();
        return denied(args, err(format!("The user denied this tool call.{feedback}")));
    }
    // Arguments the user edited are checked as the model's are: plugins see them again (a checkpoint
    // of the files they name, the guards), and their denials and the deny rules still hold (softDeny
    // asks: it was asked). Sent back unchanged it is a plain allow: seen twice, the loop guard would
    // take it for a repeat.
    let mut edited: Option<PermissionCheck> = None;
    if let Some(mut next) = answer
        .args
        .filter(|a| !same_json(&Value::Object(a.clone()), &Value::Object(args.clone())))
    {
        let mut refused: Option<String> = None;
        let event = sasacode_plugin_api::ToolCallEvent {
            call: call.clone(),
            tool: tool.clone(),
            args: next.clone(),
        };
        ctx.hooks
            .run::<hook::ToolCall>(event, |r, ev, _| {
                if let Some(a) = r.args {
                    next = a.clone();
                    ev.args = a;
                }
                if r.decision == Some(Decision::Deny) {
                    refused = Some(r.reason.unwrap_or_else(|| "plugin".into()));
                }
                refused.is_none()
            })
            .await;
        let problems = match refused {
            Some(r) => vec![r],
            None => validate(&tool.parameters(), &Value::Object(next.clone()), "input"),
        };
        let checked = PermissionCheck {
            args: next,
            ..check.clone()
        };
        let verdict = problems.is_empty().then(|| ctx.permissions.check(&checked));
        let reason = match &verdict {
            None => Some(problems.join("; ")),
            Some(v) if v.decision == Decision::Deny && v.source == VerdictSource::Rule => Some(v.reason.clone()),
            Some(_) => None,
        };
        if let Some(reason) = reason {
            return denied(
                args,
                err(format!("The user's edited arguments were refused ({reason}).")),
            );
        }
        edited = Some(checked);
    }
    if answer.decision == ApprovalDecision::Always {
        ctx.permissions.add_rules(&PermissionRules {
            allow: PermissionPolicy::rules_for(edited.as_ref().unwrap_or(&check)),
            ..Default::default()
        })?;
    }
    Ok(match edited {
        Some(e) => (e.args, None, true),
        None => (args, None, false),
    })
}

/// `isDeepStrictEqual` for JSON: numbers by value, objects whatever their keys' order.
fn same_json(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same_json(x, y)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same_json(v, w)))
        }
        _ => a == b,
    }
}

fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_string()
    }
}

async fn run_tool(
    ctx: &Arc<ToolRunContext>,
    tool: &ToolRef,
    call: &ToolCall,
    args: JsonObject,
    signal: &CancellationToken,
    ancestors: &[String],
    parent_call_id: Option<String>,
) -> anyhow::Result<ToolResult> {
    let summary = std::panic::catch_unwind(AssertUnwindSafe(|| tool.summary(&args)))
        .ok()
        .flatten()
        .unwrap_or_default();
    if let Some(parent) = &parent_call_id {
        ctx.log(SessionEntry::Custom {
            plugin: "core".into(),
            kind: "nested_tool_execute".into(),
            data: json!({ "parentCallId": parent, "call": call.tagged(), "args": args }),
        })?;
    }
    ctx.events.emit(AgentEvent::ToolStart {
        call: call.clone(),
        summary,
        parent_call_id: parent_call_id.clone(),
    });
    // Its own token, so this call alone can be stopped (Agent::abort_call); stopping the run stops
    // it too, also when that happened already (a tool_start listener can do that).
    let own = signal.child_token();
    if let Some(calls) = &ctx.calls {
        calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(call.id.clone(), own.clone());
    }

    // Calls from one execution are serialized, including when the caller joins several. Each child
    // has its own queue, so ordinary nested delegation never deadlocks. As with a JS promise, a call
    // is queued and starts when it is made, awaited or not; the tool's end waits for all of them.
    let queue: Arc<Mutex<Queue>> = Arc::new(Mutex::new(Queue {
        active: true,
        tail: None,
    }));
    let call_tool: CallTool = {
        let (ctx, queue, signal) = (ctx.clone(), queue.clone(), own.clone());
        let mut chain: Vec<String> = ancestors.to_vec();
        chain.push(tool.name().to_string());
        let parent_id = call.id.clone();
        Arc::new(move |name: String, input: JsonObject| {
            let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
            if !q.active {
                return futures::future::ready(err("Tool context is no longer active.")).boxed();
            }
            let prev = q.tail.take();
            let work = nested_call(
                ctx.clone(),
                signal.clone(),
                chain.clone(),
                parent_id.clone(),
                name,
                input,
            );
            let job: Tail = async move {
                if let Some(p) = prev {
                    p.await;
                }
                work.await
            }
            .boxed()
            .shared();
            q.tail = Some(job.clone());
            drop(q);
            tokio::spawn(job.clone());
            job.boxed()
        })
    };
    let on_update = {
        let (events, call, parent) = (ctx.events.clone(), call.clone(), parent_call_id.clone());
        Arc::new(move |text: String| {
            events.emit(AgentEvent::ToolUpdate {
                call: call.clone(),
                text,
                parent_call_id: parent.clone(),
            })
        })
    };
    let tctx = ToolContext {
        cwd: PathBuf::from(&ctx.cwd),
        signal: own.clone(),
        on_update: Some(on_update),
        call_tool: Some(call_tool),
    };
    let result = match AssertUnwindSafe(tool.execute(args, tctx)).catch_unwind().await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => err(format!("Tool failed: {e:#}")),
        Err(p) => err(format!("Tool failed: {}", panic_message(&*p))),
    };
    // Even a plugin forgetting to await cannot leave children running after its tool has finished.
    let tail = {
        let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
        q.active = false;
        q.tail.take()
    };
    if let Some(t) = tail {
        t.await;
    }
    if let Some(calls) = &ctx.calls {
        calls.lock().unwrap_or_else(|e| e.into_inner()).remove(&call.id);
    }
    if !own.is_cancelled() || signal.is_cancelled() {
        return Ok(result);
    }
    let mut content = vec![UserContent::text(
        "[The user stopped this tool call; the rest of the run goes on.]",
    )];
    content.extend(result.content);
    Ok(ToolResult {
        content,
        is_error: Some(true),
        ..result
    })
}

type Tail = futures::future::Shared<BoxFuture<'static, ToolResult>>;

/// The nested calls of one tool execution: the last one queued, which waits for those before it.
struct Queue {
    active: bool,
    tail: Option<Tail>,
}

/// One programmatic call from a tool (`ctx.call_tool`): validated, authorized and run like a model's.
async fn nested_call(
    ctx: Arc<ToolRunContext>,
    signal: CancellationToken,
    chain: Vec<String>,
    parent_id: String,
    name: String,
    input: JsonObject,
) -> ToolResult {
    let child = ToolCall {
        id: format!("nested-{}", uuid::Uuid::new_v4()),
        name,
        input,
        raw_input: None,
    };
    let got: Arc<Mutex<Option<ToolResult>>> = Arc::new(Mutex::new(None));
    let work = async {
        ctx.log(SessionEntry::Custom {
            plugin: "core".into(),
            kind: "nested_tool_call".into(),
            data: json!({ "parentCallId": parent_id, "call": child.tagged() }),
        })?;
        // Programmatic calls are already structured; tool_call_raw (model-output repair) is skipped.
        let sink = got.clone();
        execute_tools(
            ctx.clone(),
            vec![child.clone()],
            ExecOptions {
                truncated: false,
                signal: signal.clone(),
                repairs: HashMap::new(),
                on_result: Arc::new(|_| Ok(())),
                ancestors: chain.clone(),
                parent_call_id: Some(parent_id.clone()),
                on_tool_result: Some(Arc::new(move |r| {
                    *sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                })),
            },
        )
        .await?;
        let result = got
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(|| err("Nested tool did not return a result."));
        ctx.log(SessionEntry::Custom {
            plugin: "core".into(),
            kind: "nested_tool_result".into(),
            data: json!({ "parentCallId": parent_id, "call": child.tagged(), "result": result }),
        })?;
        anyhow::Ok(result)
    };
    match AssertUnwindSafe(work).catch_unwind().await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => err(format!("Nested tool failed: {e:#}")),
        Err(p) => err(format!("Nested tool failed: {}", panic_message(&*p))),
    }
}
