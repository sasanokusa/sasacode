//! diagnostics (`packages/bundled/src/diagnostics.ts`): API diagnostics that keep counters only,
//! never prompts, headers or signed state. Off until enabled (setting or `/diagnostics on`).

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{Stream, StreamExt};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use sasacode_ai::{DefaultFetch, Fetch, JsonObject, now_ms};
use sasacode_plugin_api::{
    BeforeRequestResult, Command, Lang, NotifyLevel, PluginApi, ShowTextOptions, TextFormat, hook,
};

use crate::repair::js::{js_string, js_trim, truthy};

/// Deliberately has no string-valued provider data: never log prompts, headers or signed state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireStats {
    pub request_bytes: usize,
    pub system_bytes: usize,
    pub tools_bytes: usize,
    pub messages: usize,
    pub thinking_blocks: usize,
    pub signed_blocks: usize,
}

/// `(x ?? []).forEach`-style iteration as the TS spread did it: null is empty, a string yields its
/// characters (which count for nothing here), anything else that is not an array throws.
fn spread(v: Option<&Value>) -> Option<Vec<Value>> {
    match v {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(Value::Array(a)) => Some(a.clone()),
        Some(Value::String(s)) => Some(s.chars().map(|c| Value::String(c.to_string())).collect()),
        Some(_) => None,
    }
}

/// `v.key` in JS: reading a property of null throws (None), of a primitive gives undefined.
fn prop<'a>(v: &'a Value, key: &str) -> Option<Option<&'a Value>> {
    match v {
        Value::Null => None,
        Value::Object(o) => Some(o.get(key)),
        _ => Some(None),
    }
}

fn defined(v: Option<&Value>) -> Option<&Value> {
    v.filter(|v| !v.is_null())
}

pub fn wire_stats(body: &[u8]) -> WireStats {
    let mut stats = WireStats {
        request_bytes: body.len(),
        ..WireStats::default()
    };
    // Unknown wire format: its byte length alone is useful. A step the TS would have thrown on
    // ends here, with what was counted so far.
    let _ = (|| -> Option<()> {
        let j: Value = serde_json::from_slice(body).ok()?;
        let system = match defined(prop(&j, "system")?).or(defined(prop(&j, "instructions")?)) {
            Some(v) => sasacode_ai::json::stringify(v),
            None => match defined(prop(&j, "messages")?) {
                Some(Value::Array(ms)) => {
                    let mut out = Vec::new();
                    for m in ms {
                        if prop(m, "role")? == Some(&Value::String("system".into())) {
                            out.push(m.clone());
                        }
                    }
                    sasacode_ai::json::stringify(&out)
                }
                // `.filter` of something that is not an array throws.
                Some(_) => return None,
                None => sasacode_ai::json::stringify(&""),
            },
        };
        stats.system_bytes = system.len();
        stats.tools_bytes = sasacode_ai::json::stringify(defined(prop(&j, "tools")?).unwrap_or(&json!([]))).len();
        let empty = json!([]);
        let messages = defined(prop(&j, "messages")?)
            .or(defined(prop(&j, "input")?))
            .unwrap_or(&empty);
        let Value::Array(messages) = messages else {
            return Some(());
        };
        stats.messages = messages.len();
        for message in messages {
            let blocks = match prop(message, "content")? {
                Some(Value::Array(a)) => a.clone(),
                _ => Vec::new(),
            };
            let mut all = vec![message.clone()];
            all.extend(blocks);
            all.extend(spread(prop(message, "reasoning_details")?)?);
            for b in &all {
                let ty = prop(b, "type")?;
                let ty_str = defined(ty).map(js_string).unwrap_or_default();
                if ty_str == "thinking" || ty_str == "redacted_thinking" || ty_str.starts_with("reasoning") {
                    stats.thinking_blocks += 1;
                }
                if ["signature", "data", "encrypted_content"]
                    .iter()
                    .any(|k| prop(b, k).flatten().is_some_and(truthy))
                {
                    stats.signed_blocks += 1;
                }
            }
            if [prop(message, "reasoning_content")?, prop(message, "reasoning")?]
                .into_iter()
                .any(|v| v.is_some_and(truthy))
            {
                stats.thinking_blocks += 1;
            }
        }
        Some(())
    })();
    stats
}

const COUNTERS: &[&str] = &[
    "input_tokens",
    "output_tokens",
    "prompt_tokens",
    "completion_tokens",
    "cached_tokens",
    "cache_write_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
    "prompt_cache_hit_tokens",
    "prompt_cache_miss_tokens",
    "reasoning_tokens",
    "cost",
];

const DETAILS: &[&str] = &[
    "input_tokens_details",
    "output_tokens_details",
    "prompt_tokens_details",
    "completion_tokens_details",
];

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ResponseStats {
    pub usage: Map<String, Value>,
    pub transformations: u64,
    pub drops: Map<String, Value>,
}

pub fn response_stats(event: &Value, stats: &mut ResponseStats) {
    fn visit(value: Option<&Value>, prefix: &str, stats: &mut ResponseStats) {
        let Some(Value::Object(o)) = value else { return };
        for (key, v) in o {
            if COUNTERS.contains(&key.as_str())
                && let Some(n) = v.as_f64()
                && n.is_finite()
                && n >= 0.0
            {
                stats.usage.insert(format!("{prefix}{key}"), v.clone());
            } else if DETAILS.contains(&key.as_str()) {
                visit(Some(v), &format!("{key}."), stats);
            }
        }
    }
    let message = event.get("message");
    visit(event.get("usage"), "", stats);
    visit(message.and_then(|m| m.get("usage")), "", stats);
    visit(event.get("response").and_then(|r| r.get("usage")), "", stats);
    // Only count documented drop reasons. Paths and unknown strings may contain user data.
    let transformations =
        defined(message.and_then(|m| m.get("input_transformations"))).or(defined(event.get("input_transformations")));
    if let Some(Value::Array(ts)) = transformations {
        for t in ts {
            stats.transformations += 1;
            for reason in ["prefix_binding_mismatch", "organization_binding_mismatch"] {
                if t.get("reason").and_then(Value::as_str) == Some(reason) {
                    let n = stats.drops.get(reason).and_then(Value::as_u64).unwrap_or(0);
                    stats.drops.insert(reason.to_string(), json!(n + 1));
                }
            }
        }
    }
}

pub type Record = Arc<dyn Fn(JsonObject) -> anyhow::Result<()> + Send + Sync>;

/// Observes the transport actually used by the adapter, including its retries, without consuming ahead.
pub struct DiagnosticFetch {
    inner: Arc<dyn Fetch>,
    record: Record,
    state: Mutex<(u64, Option<String>)>,
}

impl DiagnosticFetch {
    pub fn new(inner: Arc<dyn Fetch>, record: Record) -> Self {
        DiagnosticFetch {
            inner,
            record,
            state: Mutex::new((0, None)),
        }
    }
}

fn save(record: &Record, data: JsonObject) {
    // Diagnostics cannot fail generation.
    let _ = record(data);
}

fn with(meta: &JsonObject, extra: Value) -> JsonObject {
    let mut out = meta.clone();
    if let Value::Object(e) = extra {
        out.extend(e);
    }
    out
}

/// Records "aborted" when the request is dropped before the transport answered.
struct AbortGuard {
    record: Record,
    meta: JsonObject,
    armed: bool,
}

impl Drop for AbortGuard {
    fn drop(&mut self) {
        if self.armed {
            save(&self.record, with(&self.meta, json!({ "outcome": "aborted" })));
        }
    }
}

const MAX_PENDING: usize = 1_048_576;

/// The SSE byte stream passed through untouched, its `data:` lines read for counters.
struct Observed {
    inner: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    pending: Vec<u8>,
    stats: ResponseStats,
    record: Record,
    meta: JsonObject,
    status: u16,
    saved: bool,
}

impl Observed {
    fn line(&mut self, line: &[u8]) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.len() <= MAX_PENDING
            && let Some(data) = line.strip_prefix(b"data:")
            && let Ok(event) = serde_json::from_slice::<Value>(data)
        {
            // [DONE] does not parse.
            response_stats(&event, &mut self.stats);
        }
    }
    fn observe(&mut self, chunk: &[u8]) {
        self.pending.extend_from_slice(chunk);
        let mut start = 0;
        let mut lines = Vec::new();
        while let Some(at) = self.pending[start..].iter().position(|&b| b == b'\n') {
            lines.push(self.pending[start..start + at].to_vec());
            start += at + 1;
        }
        self.pending.drain(..start);
        if self.pending.len() > MAX_PENDING {
            self.pending.clear();
        }
        for l in lines {
            self.line(&l);
        }
    }
    fn finish(&mut self, outcome: &str) {
        if self.saved {
            return;
        }
        self.saved = true;
        let stats = serde_json::to_value(&self.stats).unwrap_or(Value::Null);
        let mut data = with(&self.meta, json!({ "status": self.status, "outcome": outcome }));
        if let Value::Object(s) = stats {
            data.extend(s);
        }
        save(&self.record, data);
    }
}

impl Stream for Observed {
    type Item = reqwest::Result<Bytes>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.poll_next_unpin(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                let rest = std::mem::take(&mut self.pending);
                self.line(&rest);
                self.finish("complete");
                Poll::Ready(None)
            }
            Poll::Ready(Some(Ok(chunk))) => {
                self.observe(&chunk);
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(e))) => {
                self.finish("stream_error");
                Poll::Ready(Some(Err(e)))
            }
        }
    }
}

impl Drop for Observed {
    fn drop(&mut self) {
        // Dropped before its end: the reader cancelled it.
        self.finish("cancelled");
    }
}

impl Fetch for DiagnosticFetch {
    fn send(&self, req: reqwest::Request) -> BoxFuture<'static, anyhow::Result<reqwest::Response>> {
        let body: Vec<u8> = req
            .body()
            .and_then(|b| b.as_bytes())
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        let digest = format!("{:x}", Sha256::digest(&body));
        let meta = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.0 += 1;
            let repeated = s.1.as_deref() == Some(digest.as_str());
            let mut meta = JsonObject::new();
            meta.insert("attempt".into(), json!(s.0));
            meta.insert("repeatedBody".into(), json!(repeated));
            if let Value::Object(w) = serde_json::to_value(wire_stats(&body)).unwrap_or(Value::Null) {
                meta.extend(w);
            }
            s.1 = Some(digest);
            meta
        };
        let (inner, record) = (self.inner.clone(), self.record.clone());
        Box::pin(async move {
            let mut guard = AbortGuard {
                record: record.clone(),
                meta: meta.clone(),
                armed: true,
            };
            let sent = inner.send(req).await;
            guard.armed = false;
            let response = match sent {
                Ok(r) => r,
                Err(e) => {
                    save(&record, with(&meta, json!({ "outcome": "transport_error" })));
                    return Err(e);
                }
            };
            let status = response.status();
            let sse = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("text/event-stream"));
            if !status.is_success() || !sse {
                save(&record, with(&meta, json!({ "status": status.as_u16() })));
                return Ok(response);
            }
            // SSE frames may straddle UTF-8 chunks. Forward their bytes untouched; bound the parser buffer.
            let mut builder = http::Response::builder().status(status).version(response.version());
            if let Some(h) = builder.headers_mut() {
                *h = response.headers().clone();
            }
            let observed = Observed {
                inner: Box::pin(response.bytes_stream()),
                pending: Vec::new(),
                stats: ResponseStats::default(),
                record,
                meta,
                status: status.as_u16(),
                saved: false,
            };
            Ok(reqwest::Response::from(
                builder.body(reqwest::Body::wrap_stream(observed))?,
            ))
        })
    }
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.define_settings(
            &json!({ "type": "object", "properties": { "enabled": { "type": "boolean" }, "persist": { "type": "boolean" } }, "additionalProperties": false }),
            Some(&json!({ "enabled": false, "persist": false })),
        )?;
        let enabled = Arc::new(AtomicBool::new(settings.get("enabled") == Some(&Value::Bool(true))));
        let persist = settings.get("persist") == Some(&Value::Bool(true));
        let records: Arc<Mutex<VecDeque<Value>>> = Arc::default();
        let (rs, a) = (records.clone(), api.clone());
        let record: Arc<dyn Fn(Value) -> anyhow::Result<()> + Send + Sync> = Arc::new(move |data: Value| {
            let mut entry = JsonObject::new();
            entry.insert("at".into(), json!(now_ms()));
            if let Value::Object(d) = data {
                entry.extend(d);
            }
            let entry = Value::Object(entry);
            {
                let mut r = rs.lock().unwrap_or_else(|e| e.into_inner());
                r.push_back(entry.clone());
                if r.len() > 100 {
                    r.pop_front();
                }
            }
            if persist {
                a.session().append("diagnostic", entry)?;
            }
            Ok(())
        });
        let agent_of = |agent: &Option<String>| agent.clone().unwrap_or_else(|| "main".to_string());

        let (on, rec) = (enabled.clone(), record.clone());
        api.on::<hook::BeforeRequest, _, _>(move |ev, ctx| {
            let out = on.load(Ordering::Relaxed).then(|| {
                let (rec, agent) = (rec.clone(), agent_of(&ctx.agent));
                let inner: Arc<dyn Fetch> = ev.fetch.clone().unwrap_or_else(|| Arc::new(DefaultFetch));
                let r: Record = Arc::new(move |data: JsonObject| {
                    let mut d = JsonObject::new();
                    d.insert("kind".into(), json!("request"));
                    d.insert("agent".into(), json!(agent));
                    d.extend(data);
                    rec(Value::Object(d))
                });
                BeforeRequestResult {
                    fetch: Some(Arc::new(DiagnosticFetch::new(inner, r))),
                    ..Default::default()
                }
            });
            async move { Ok(out) }
        });

        let (on, rec) = (enabled.clone(), record.clone());
        api.on::<hook::AssistantMessage, _, _>(move |ev, ctx| {
            let done = if on.load(Ordering::Relaxed) {
                let usage = serde_json::to_value(&ev.message.usage).unwrap_or(Value::Null);
                let usage: JsonObject = usage
                    .as_object()
                    .map(|u| {
                        u.iter()
                            .filter(|(k, v)| {
                                ["input", "output", "cacheRead", "cacheWrite", "cost"].contains(&k.as_str())
                                    && v.as_f64().is_some_and(f64::is_finite)
                            })
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                rec(json!({
                    "kind": "assistant",
                    "agent": agent_of(&ctx.agent),
                    "stopReason": ev.message.stop_reason.as_str(),
                    "usage": usage,
                }))
            } else {
                Ok(())
            };
            async move { done.map(|()| None) }
        });

        let (on, rec) = (enabled.clone(), record);
        api.on::<hook::ToolResult, _, _>(move |ev, ctx| {
            let done = if on.load(Ordering::Relaxed) {
                let mut d = json!({ "kind": "tool", "agent": agent_of(&ctx.agent), "ran": ev.ran, "isError": ev.result.is_error() });
                if let Some(Value::Bool(changed)) = ev.result.details.as_ref().and_then(|d| d.get("changed")) {
                    d["changed"] = json!(changed);
                }
                rec(d)
            } else {
                Ok(())
            };
            async move { done.map(|()| None) }
        });

        let rs = records.clone();
        api.on::<hook::SessionEnd, _, _>(move |_e, _c| {
            rs.lock().unwrap_or_else(|e| e.into_inner()).clear();
            async { Ok(None) }
        });

        let description = if api.ui().lang() == Lang::Ja {
            "API 診断: on / off / show / clear（内容や秘密値は記録しない）"
        } else {
            "API diagnostics: on / off / show / clear (no contents or secrets)"
        };
        let (on, rs, a) = (enabled, records, api.clone());
        api.register_command(Command::new("diagnostics", description, move |c| {
            let (on, rs, a) = (on.clone(), rs.clone(), a.clone());
            async move {
                match js_trim(&c.args) {
                    "on" => on.store(true, Ordering::Relaxed),
                    "off" => on.store(false, Ordering::Relaxed),
                    "clear" => rs.lock().unwrap_or_else(|e| e.into_inner()).clear(),
                    _ => {
                        let list: Vec<Value> = rs.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect();
                        let text = sasacode_ai::json::stringify_pretty(
                            &json!({ "enabled": on.load(Ordering::Relaxed), "records": list }),
                        );
                        a.ui()
                            .show_text(ShowTextOptions {
                                title: "API diagnostics".into(),
                                text,
                                format: TextFormat::Text,
                            })
                            .await?;
                        return Ok(());
                    }
                }
                let n = rs.lock().unwrap_or_else(|e| e.into_inner()).len();
                a.ui().notify(
                    &format!(
                        "diagnostics {}; {n} records",
                        if on.load(Ordering::Relaxed) { "on" } else { "off" }
                    ),
                    NotifyLevel::Info,
                );
                Ok(())
            }
        }));
        Ok(())
    })
}
