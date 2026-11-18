//! packages/bundled/test/diagnostics.test.ts. Every transport is an injected `Fetch`: no request
//! leaves the process. `persist` stays off, so nothing is written to a session.

mod support_core;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use futures::future::BoxFuture;
use serde_json::{Value, json};

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_ai::{Fetch, JsonObject, SamplingOptions};
use sasacode_bundled::diagnostics::{DiagnosticFetch, Record, ResponseStats, response_stats, wire_stats};
use sasacode_plugin_api::{
    BeforeRequestEvent, BeforeRequestResult, CommandContext, Lang, NotifyLevel, PermissionMode, PluginApi,
    SelectOption, ShowTextOptions, StopCause, UiBridge, hook,
};
use support_core::*;

const SECRET: &str = "PRIVATE_CANARY_署名_KEY";

fn body() -> String {
    json!({ "system": SECRET, "tools": [{ "description": SECRET }], "messages": [{ "role": "assistant", "content": [{ "type": "thinking", "thinking": SECRET, "signature": SECRET }] }] })
        .to_string()
}

fn event() -> Value {
    json!({ "type": "message_start", "message": { "usage": { "input_tokens": 5, "cache_read_input_tokens": 20 }, "input_transformations": [{ "type": "thinking_dropped", "reason": "prefix_binding_mismatch", "path": SECRET }, { "reason": SECRET }] } })
}

fn stream() -> String {
    let chunk = json!({ "choices": [{ "delta": { "content": SECRET } }], "usage": { "prompt_tokens_details": { "cached_tokens": 2, "cache_write_tokens": 3 }, "cost": 0.01, "key": SECRET } });
    format!(
        "event: message_start\ndata: {}\n\ndata: {chunk}\n\ndata: [DONE]\n\n",
        event()
    )
}

fn recorder() -> (Record, Arc<Mutex<Vec<Value>>>) {
    let records: Arc<Mutex<Vec<Value>>> = Arc::default();
    let r = records.clone();
    let record: Record = Arc::new(move |data: JsonObject| {
        r.lock().unwrap_or_else(|e| e.into_inner()).push(Value::Object(data));
        Ok(())
    });
    (record, records)
}

fn request(method: reqwest::Method, url: &str, body: Option<&str>, auth: Option<&str>) -> reqwest::Request {
    let mut req = reqwest::Request::new(method, reqwest::Url::parse(url).unwrap());
    if let Some(a) = auth {
        req.headers_mut().insert(
            "authorization",
            reqwest::header::HeaderValue::from_bytes(a.as_bytes()).unwrap(),
        );
    }
    if let Some(b) = body {
        *req.body_mut() = Some(b.to_string().into());
    }
    req
}

fn plain(status: u16, headers: &[(&str, &str)], text: &str) -> reqwest::Response {
    let mut b = http::Response::builder().status(status);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    reqwest::Response::from(b.body(text.to_string()).unwrap())
}

#[test]
fn diagnostics_only_extract_counters_and_documented_drop_reasons() {
    let body = body();
    let stats = wire_stats(body.as_bytes());
    assert_eq!(stats.messages, 1);
    assert_eq!(stats.thinking_blocks, 1);
    assert_eq!(stats.signed_blocks, 1);
    assert_eq!(stats.request_bytes, body.len());
    let mut response = ResponseStats::default();
    response_stats(&event(), &mut response);
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({ "usage": { "input_tokens": 5, "cache_read_input_tokens": 20 }, "transformations": 2, "drops": { "prefix_binding_mismatch": 1 } })
    );
    let all = json!({ "stats": stats, "response": response }).to_string();
    assert!(!all.contains(SECRET));
}

/// The SDK side of the second test: 429 first, then the stream in 7-byte chunks.
struct Chunked {
    calls: AtomicUsize,
    seen: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
}

impl Fetch for Chunked {
    fn send(&self, req: reqwest::Request) -> BoxFuture<'static, anyhow::Result<reqwest::Response>> {
        let body = req
            .body()
            .and_then(|b| b.as_bytes())
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.as_bytes().to_vec())
            .unwrap_or_default();
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).push((body, auth));
        let n = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        Box::pin(async move {
            if n == 1 {
                return Ok(plain(429, &[], &format!("rate limited {SECRET}")));
            }
            let bytes = stream().into_bytes();
            let chunks: Vec<Result<Bytes, std::io::Error>> =
                bytes.chunks(7).map(|c| Ok(Bytes::copy_from_slice(c))).collect();
            let res = http::Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                .body(reqwest::Body::wrap_stream(futures::stream::iter(chunks)))?;
            Ok(reqwest::Response::from(res))
        })
    }
}

#[tokio::test]
async fn wire_bytes_headers_and_split_utf8_sse_pass_through_retries_are_distinguished() {
    let inner = Arc::new(Chunked {
        calls: AtomicUsize::new(0),
        seen: Mutex::default(),
    });
    let (record, records) = recorder();
    let observed = DiagnosticFetch::new(inner.clone(), record);
    let body = body();
    let first = observed
        .send(request(
            reqwest::Method::POST,
            &format!("https://user:{SECRET}@example.test?key={SECRET}"),
            Some(&body),
            Some(SECRET),
        ))
        .await
        .unwrap();
    assert_eq!(first.status().as_u16(), 429);
    let second = observed
        .send(request(
            reqwest::Method::POST,
            "https://example.test",
            Some(&body),
            Some(SECRET),
        ))
        .await
        .unwrap();
    assert_eq!(second.text().await.unwrap(), stream());
    for (b, auth) in inner.seen.lock().unwrap().iter() {
        assert_eq!(b, body.as_bytes());
        assert_eq!(auth, SECRET.as_bytes());
    }
    let records = records.lock().unwrap().clone();
    assert_eq!(records[0]["status"], 429);
    assert_eq!(records[0]["attempt"], 1);
    assert_eq!(records[0]["repeatedBody"], false);
    assert_eq!(records[1]["status"], 200);
    assert_eq!(records[1]["attempt"], 2);
    assert_eq!(records[1]["repeatedBody"], true);
    assert_eq!(records[1]["outcome"], "complete");
    assert_eq!(records[1]["transformations"], 2);
    let usage = &records[1]["usage"];
    assert_eq!(usage["prompt_tokens_details.cached_tokens"], 2);
    assert_eq!(usage["prompt_tokens_details.cache_write_tokens"], 3);
    assert_eq!(usage["cost"], 0.01);
    assert!(!Value::Array(records).to_string().contains(SECRET));
}

/// A body that never ends and says when the reader cancelled it.
struct Endless(Arc<AtomicBool>);

impl Stream for Endless {
    type Item = Result<Bytes, std::io::Error>;
    fn poll_next(self: std::pin::Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

impl Drop for Endless {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

struct EndlessFetch(Arc<AtomicBool>);

impl Fetch for EndlessFetch {
    fn send(&self, _req: reqwest::Request) -> BoxFuture<'static, anyhow::Result<reqwest::Response>> {
        let flag = self.0.clone();
        Box::pin(async move {
            let res = http::Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                .body(reqwest::Body::wrap_stream(Endless(flag)))?;
            Ok(reqwest::Response::from(res))
        })
    }
}

struct Throwing;

impl Fetch for Throwing {
    fn send(&self, _req: reqwest::Request) -> BoxFuture<'static, anyhow::Result<reqwest::Response>> {
        Box::pin(async { Err(anyhow::anyhow!("{SECRET}")) })
    }
}

#[tokio::test]
async fn cancellation_and_recorder_failure_preserve_transport_semantics() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let (record, records) = recorder();
    let res = DiagnosticFetch::new(Arc::new(EndlessFetch(cancelled.clone())), record)
        .send(request(
            reqwest::Method::POST,
            "https://example.test",
            Some(&body()),
            None,
        ))
        .await
        .unwrap();
    // `res.body.cancel()`: the reader lets go of the body.
    drop(res);
    assert!(cancelled.load(Ordering::Relaxed));
    assert_eq!(records.lock().unwrap()[0]["outcome"], "cancelled");
    // A reqwest::Request carries no AbortSignal: the TS's already-aborted signal has no counterpart.
    let failing: Record = Arc::new(|_| anyhow::bail!("observer failed"));
    let e = DiagnosticFetch::new(Arc::new(Throwing), failing)
        .send(request(reqwest::Method::GET, "https://example.test", None, None))
        .await
        .unwrap_err();
    assert_eq!(e.to_string(), SECRET);
}

struct Ok200;

impl Fetch for Ok200 {
    fn send(&self, _req: reqwest::Request) -> BoxFuture<'static, anyhow::Result<reqwest::Response>> {
        Box::pin(async { Ok(plain(200, &[], "ok")) })
    }
}

async fn wrapped_by(agent: &Agent, transport: Arc<dyn Fetch>) -> Option<Arc<dyn Fetch>> {
    let event = BeforeRequestEvent {
        messages: Vec::new(),
        model: agent.model(),
        sampling: SamplingOptions::default(),
        fetch: Some(transport),
    };
    let mut wrapped = None;
    agent
        .hooks()
        .run::<hook::BeforeRequest>(event, |r: BeforeRequestResult, _, _| {
            wrapped = r.fetch;
            true
        })
        .await;
    wrapped
}

async fn command(host: &PluginHost, name: &str, args: &str) {
    let c = host.commands().into_iter().find(|c| c.command.name == name).unwrap();
    (c.command.run)(CommandContext { args: args.into() }).await.unwrap();
}

#[tokio::test]
async fn the_plugin_is_disabled_by_default_and_composes_an_existing_fetch_only_when_enabled() {
    let dir = temp_dir("sasacode-diagnostics-");
    let h = load_plugin("diagnostics", bundled("diagnostics"), &path_str(dir.path()), json!({})).await;
    let transport: Arc<dyn Fetch> = Arc::new(Ok200);
    assert!(wrapped_by(&h.agent, transport.clone()).await.is_none());
    command(&h.host, "diagnostics", "on").await;
    let wrapped = wrapped_by(&h.agent, transport).await.unwrap();
    let res = wrapped
        .send(request(reqwest::Method::GET, "https://example.test", None, None))
        .await
        .unwrap();
    assert_eq!(res.text().await.unwrap(), "ok");
    command(&h.host, "diagnostics", "off").await;
    assert!(
        h.notices
            .lock()
            .unwrap()
            .join(",")
            .contains("diagnostics off; 1 records")
    );
}

/// A front end that keeps what show_text showed.
struct ShowUi(Arc<Mutex<String>>);

#[async_trait]
impl UiBridge for ShowUi {
    fn interactive(&self) -> bool {
        false
    }
    fn notify(&self, _message: &str, _level: NotifyLevel) {}
    async fn show_text(&self, options: ShowTextOptions) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = options.text;
    }
    async fn confirm(&self, _t: &str, _m: Option<&str>) -> bool {
        false
    }
    async fn select(&self, _t: &str, _o: &[SelectOption]) -> Option<String> {
        None
    }
}

/// The openai-chat server: 429 with retry-after 0, then a streamed reply.
struct Server(Arc<AtomicUsize>);

impl Fetch for Server {
    fn send(&self, _req: reqwest::Request) -> BoxFuture<'static, anyhow::Result<reqwest::Response>> {
        let n = self.0.fetch_add(1, Ordering::Relaxed) + 1;
        Box::pin(async move {
            Ok(if n == 1 {
                plain(
                    429,
                    &[("content-type", "application/json"), ("retry-after", "0")],
                    &json!({ "error": { "message": SECRET } }).to_string(),
                )
            } else {
                plain(
                    200,
                    &[("content-type", "text/event-stream")],
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n",
                )
            })
        })
    }
}

#[tokio::test]
async fn agent_fetch_hook_observes_actual_sdk_429_retries_and_final_usage_without_secrets() {
    let dir = temp_dir("sasacode-diagnostics-");
    let cwd = path_str(dir.path());
    let mut m = model("openai-chat", 10_000, 1000);
    m.id = "test".into();
    m.provider = "test".into();
    m.base_url = Some("https://mock.invalid/v1".into());
    let calls = Arc::new(AtomicUsize::new(0));
    let transport: Arc<dyn Fetch> = Arc::new(Server(calls.clone()));
    // A transport plugin registered ahead of diagnostics is composed instead of replaced.
    let mut o = AgentOptions::new(m, cwd.clone(), SECRET);
    o.max_retries = Some(1);
    o.permissions = Some(policy(PermissionMode::Auto));
    let agent = Agent::new(o);
    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd,
        settings: Some(Arc::new(|_: &str| json!({ "enabled": true }))),
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: Lang::Ja,
    });
    let shown: Arc<Mutex<String>> = Arc::default();
    host.set_ui(Some(Arc::new(ShowUi(shown.clone()))));
    let t = transport.clone();
    assert!(
        host.load(
            "transport",
            move |api: PluginApi| -> BoxFuture<'static, anyhow::Result<()>> {
                Box::pin(async move {
                    api.on::<hook::BeforeRequest, _, _>(move |_ev, _c| {
                        let fetch = Some(t.clone());
                        async move {
                            Ok(Some(BeforeRequestResult {
                                fetch,
                                ..Default::default()
                            }))
                        }
                    });
                    Ok(())
                })
            }
        )
        .await
    );
    assert!(host.load("diagnostics", bundled("diagnostics")).await);
    assert_eq!(agent.prompt_text(SECRET).await.unwrap(), StopCause::Done);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    command(&host, "diagnostics", "show").await;
    let shown = shown.lock().unwrap().clone();
    let parsed: Value = serde_json::from_str(&shown).unwrap();
    let records = parsed["records"].as_array().unwrap();
    let requests: Vec<&Value> = records.iter().filter(|r| r["kind"] == "request").collect();
    assert_eq!(
        requests.iter().map(|r| r["status"].clone()).collect::<Vec<_>>(),
        vec![json!(429), json!(200)]
    );
    let ok = records.iter().find(|r| r["status"] == 200).unwrap();
    assert_eq!(ok["attempt"], 2);
    assert_eq!(ok["repeatedBody"], true);
    assert_eq!(ok["usage"]["prompt_tokens"], 10);
    assert_eq!(ok["usage"]["completion_tokens"], 2);
    assert!(!shown.contains(SECRET));
}
