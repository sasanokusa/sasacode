//! thinking-budget: caps runaway thinking blocks during streaming on local models (or when
//! explicitly configured via `maxChars` / `cloudMaxChars`), stops generation cleanly, and asks the
//! model to act with a tool or answer concisely.
//!
//! settings: { maxChars?: number, localMaxChars?: number (default 12000), cloudMaxChars?: number (default 0), maxStopsPerRun?: number (default 2) }

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use serde_json::Value;

use sasacode_ai::ModelInfo;
use sasacode_plugin_api::{AssistantMessageResult, DeltaKind, PluginApi, StreamDeltaResult, hook};

use crate::repair::js::num_setting;
use crate::text_tool_calls::local_server;

const INJECT: &str = "[thinking-budget] Your thinking exceeded the length limit and was stopped. Keep your reasoning brief and immediately call a tool or give your final answer.";

#[derive(Default)]
struct AgentState {
    stops: u32,
    is_local: Option<bool>,
}

fn lock(m: &Mutex<HashMap<String, AgentState>>) -> std::sync::MutexGuard<'_, HashMap<String, AgentState>> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn is_local_model(m: &ModelInfo) -> bool {
    m.server.is_some() || local_server(m.base_url.as_deref())
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let explicit_max = settings
            .get("maxChars")
            .and_then(Value::as_f64)
            .map(|n| n.max(0.0) as usize);
        let local_max = num_setting(&settings, "localMaxChars", 12_000.0).max(0.0) as usize;
        let cloud_max = num_setting(&settings, "cloudMaxChars", 0.0).max(0.0) as usize;
        let max_stops = num_setting(&settings, "maxStopsPerRun", 2.0).max(0.0) as u32;

        let states: Arc<Mutex<HashMap<String, AgentState>>> = Arc::default();

        let st = states.clone();
        api.on::<hook::UserPrompt, _, _>(move |_e, ctx| {
            let key = ctx.agent.unwrap_or_default();
            lock(&st).entry(key).or_default().stops = 0;
            async { Ok(None) }
        });

        let st = states.clone();
        api.on::<hook::BeforeRequest, _, _>(move |ev, ctx| {
            let key = ctx.agent.unwrap_or_default();
            lock(&st).entry(key).or_default().is_local = Some(is_local_model(&ev.model));
            async { Ok(None) }
        });

        let st = states.clone();
        let model_api = api.clone();
        api.on_sync::<hook::StreamDelta, _>(move |ev, ctx| {
            if ev.kind != DeltaKind::Thinking {
                return Ok(None);
            }
            let key = ctx.agent.clone().unwrap_or_default();
            let (stops, cached_local) = {
                let map = lock(&st);
                let entry = map.get(&key);
                (entry.map_or(0, |e| e.stops), entry.and_then(|e| e.is_local))
            };
            if stops >= max_stops {
                return Ok(None);
            }
            let limit = match explicit_max {
                Some(v) => v,
                None => {
                    let local = cached_local.unwrap_or_else(|| {
                        let l = is_local_model(&model_api.agent().model());
                        lock(&st).entry(key).or_default().is_local = Some(l);
                        l
                    });
                    if local { local_max } else { cloud_max }
                }
            };
            if limit == 0 || ev.text.len() <= limit || ev.text.chars().count() <= limit {
                return Ok(None);
            }
            Ok(Some(StreamDeltaResult {
                stop: Some(format!("thinking exceeded {limit} chars")),
            }))
        });

        let st = states;
        let name = api.name().to_string();
        api.on::<hook::AssistantMessage, _, _>(move |ev, ctx| {
            let st = st.clone();
            let name = name.clone();
            async move {
                if ev.stopped.as_ref().map(|s| s.plugin.as_str()) != Some(name.as_str()) {
                    return Ok(None);
                }
                let key = ctx.agent.unwrap_or_default();
                let stops = {
                    let mut map = lock(&st);
                    let entry = map.entry(key).or_default();
                    entry.stops += 1;
                    entry.stops
                };
                Ok(Some(AssistantMessageResult {
                    message: None,
                    retry: false,
                    inject: (stops <= max_stops).then(|| INJECT.to_string()),
                }))
            }
        });

        Ok(())
    })
}
