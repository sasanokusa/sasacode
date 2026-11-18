//! agent-judge (`packages/bundled/src/agent-judge.ts`): the "agent" permission mode: a model
//! decides whether a call the mode would ask the user about is safe to run unasked. The core asks
//! for every call but reads inside the working directory; this permission hook judges the rest.
//! Safe calls run, risky ones and a judge that fails still go to the user, as does a call
//! text-tool-calls picked up from the reply that is not a read. Rules the user wrote are never
//! loosened (the hook only gets `lowest`). The judging model is `judgeModel` in config, else the
//! current one.

use std::sync::{Arc, Mutex, OnceLock};

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

/// What the judge decided about one call: "allow" runs it unasked, "ask" leaves it to the user
/// (risky, or the judge failed).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Judged {
    pub call_id: String,
    pub tool: String,
    pub verdict: &'static str,
    pub reason: String,
}

type Listener = Arc<dyn Fn(&Judged) + Send + Sync>;

/// Who hears one harness's judge (`--control stdio` reports each verdict as a `judged` line). The
/// TS kept one set per process; here each harness has its own, so runs side by side (tests) do not
/// hear each other.
#[derive(Clone, Default)]
pub struct Verdicts(Arc<Mutex<Vec<(u64, Listener)>>>);

impl Verdicts {
    /// Hear every verdict from now on. Returns the way to stop.
    pub fn on(&self, listener: impl Fn(&Judged) + Send + Sync + 'static) -> Box<dyn FnOnce() + Send> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((id, Arc::new(listener)));
        let all = self.0.clone();
        Box::new(move || all.lock().unwrap_or_else(|e| e.into_inner()).retain(|(i, _)| *i != id))
    }

    fn tell(&self, judged: &Judged) {
        let listeners: Vec<Listener> = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for l in listeners {
            l(judged);
        }
    }
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    with_verdicts(Verdicts::default())(api)
}

/// The plugin, telling `verdicts` what it decides.
pub fn with_verdicts(verdicts: Verdicts) -> impl FnOnce(PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    move |api| plugin_telling(api, verdicts)
}

fn plugin_telling(api: PluginApi, verdicts: Verdicts) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let model = api.settings().get("model").and_then(Value::as_str).map(str::to_string);
        let a = api.clone();
        api.on::<hook::Permission, _, _>(move |ev, _c| {
            let (a, model, verdicts) = (a.clone(), model.clone(), verdicts.clone());
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
                let decided = judged.unwrap_or_else(|e| PermissionResult {
                    decision: Some(Decision::Ask),
                    reason: Some(format!("judge failed: {e:#}")),
                });
                verdicts.tell(&Judged {
                    call_id: ev.call.id.clone(),
                    tool: ev.tool.name().to_string(),
                    verdict: if decided.decision == Some(Decision::Allow) {
                        "allow"
                    } else {
                        "ask"
                    },
                    reason: decided.reason.clone().unwrap_or_default(),
                });
                Ok(Some(decided))
            }
        });
        Ok(())
    })
}
