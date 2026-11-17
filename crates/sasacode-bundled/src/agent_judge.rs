//! agent-judge (`packages/bundled/src/agent-judge.ts`): the "agent" permission mode: a model
//! decides whether a call the mode would ask the user about is safe to run unasked. The core asks
//! for every call but reads inside the working directory; this permission hook judges the rest.
//! Safe calls run, risky ones and a judge that fails still go to the user, as does a call
//! text-tool-calls picked up from the reply that is not a read. Rules the user wrote are never
//! loosened (the hook only gets `lowest`). The judging model is `judgeModel` in config, else the
//! current one.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use regex::Regex;
use serde_json::Value;

use sasacode_ai::{JsonObject, Message, UserContent, UserMessage, now_ms};
use sasacode_plugin_api::{
    CompleteOptions, Decision, PermissionMode, PermissionResult, PluginApi, VerdictSource, hook,
};

use crate::repair::js::js_string;
use crate::text_tool_calls::asks_first;

fn question(cwd: &str, tool: &str, args: &JsonObject) -> String {
    format!(
        "A coding agent working in {cwd} wants to run this tool call:
tool: {tool}
arguments: {}

Is it safe to run without asking the user? Safe means read-only or confined to the working directory, reversible, and not touching credentials, the network in a harmful way, or system settings.
Answer with one line of JSON: {{\"safe\": true|false, \"reason\": \"<short reason>\"}}",
        sasacode_ai::json::stringify(&Value::Object(args.clone()))
    )
}

/// `/\{[\s\S]*\}/`.
fn verdict_json(text: &str) -> Option<&str> {
    static R: OnceLock<Regex> = OnceLock::new();
    // INFALLIBLE: a constant pattern.
    R.get_or_init(|| Regex::new(r"\{[\s\S]*\}").expect("valid regex"))
        .find(text)
        .map(|m| m.as_str())
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let model = api.settings().get("model").and_then(Value::as_str).map(str::to_string);
        let a = api.clone();
        api.on::<hook::Permission, _, _>(move |ev, _c| {
            let (a, model) = (a.clone(), model.clone());
            async move {
                // Only what the mode itself left to the user; never a rule's or another plugin's decision. A
                // call text-tool-calls picked up from the reply may be an example, which a judge cannot tell.
                if ev.mode != PermissionMode::Agent
                    || ev.verdict.source != VerdictSource::Mode
                    || ev.verdict.decision != Decision::Ask
                    || ev.lowest != Decision::Allow
                    || asks_first(&ev.call, ev.tool.as_ref())
                {
                    return Ok(None);
                }
                let judged: anyhow::Result<PermissionResult> = async {
                    let text = a
                        .agent()
                        .complete(CompleteOptions {
                            model,
                            system: "You review tool calls for safety.".to_string(),
                            max_tokens: Some(2048),
                            messages: vec![Message::User(UserMessage {
                                timestamp: now_ms(),
                                content: vec![UserContent::text(question(
                                    &ev.cwd.to_string_lossy(),
                                    ev.tool.name(),
                                    &ev.args,
                                ))],
                            })],
                            signal: None,
                        })
                        .await?;
                    let m = verdict_json(&text).ok_or_else(|| anyhow::anyhow!("judge gave no verdict"))?;
                    let v: Value = serde_json::from_str(m)?;
                    let safe = v.get("safe") == Some(&Value::Bool(true));
                    let reason = match v.get("reason") {
                        None | Some(Value::Null) => String::new(),
                        Some(r) => js_string(r),
                    };
                    Ok(PermissionResult {
                        decision: Some(if safe { Decision::Allow } else { Decision::Ask }),
                        reason: Some(format!(
                            "agent judged {}: {reason}",
                            if safe { "safe" } else { "risky" }
                        )),
                    })
                }
                .await;
                Ok(Some(judged.unwrap_or_else(|e| PermissionResult {
                    decision: Some(Decision::Ask),
                    reason: Some(format!("judge failed: {e:#}")),
                })))
            }
        });
        Ok(())
    })
}
