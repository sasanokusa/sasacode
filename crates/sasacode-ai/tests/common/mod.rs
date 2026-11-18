//! A fake transport for the adapter tests (the TS tests' `fetch` stubs): it records what each request
//! sent and answers with a recorded stream, an error body, or a body that breaks midway.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use futures::future::BoxFuture;
use sasacode_ai::provider::Fetch;
use sasacode_ai::{AssistantMessage, Message, ModelInfo, Provider, Request, StreamEvent, ToolSpec};
use serde_json::{Value, json};

/// What one request sent.
#[derive(Debug, Clone)]
pub struct Sent {
    pub url: String,
    pub headers: reqwest::header::HeaderMap,
    pub raw: String,
    pub body: Value,
}

impl Sent {
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
    }
}

/// How the fake answers.
pub enum Reply {
    /// 200 with an event stream.
    Sse(String),
    /// A status with a JSON (or any) body.
    Status(u16, String),
    /// A status with extra response headers.
    WithHeaders(u16, String, Vec<(&'static str, String)>),
    /// 200 whose body sends `first`, then fails with `error`.
    Breaking(String, Box<dyn std::error::Error + Send + Sync>),
    /// 200 whose body sends `first`, then nothing more, without ending.
    Stalling(String),
    /// Never answers.
    Hang,
    /// The transport fails (connection refused and the like).
    Fail(String),
}

type Handler = dyn Fn(&Sent, usize) -> Reply + Send + Sync;

pub struct FakeFetch {
    pub sent: Arc<Mutex<Vec<Sent>>>,
    handler: Box<Handler>,
}

impl Fetch for FakeFetch {
    fn send(&self, req: reqwest::Request) -> BoxFuture<'static, anyhow::Result<reqwest::Response>> {
        let raw = req
            .body()
            .and_then(|b| b.as_bytes())
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default();
        let sent = Sent {
            url: req.url().to_string(),
            headers: req.headers().clone(),
            body: serde_json::from_str(&raw).unwrap_or(Value::Null),
            raw,
        };
        let n = {
            let mut all = self.sent.lock().unwrap();
            all.push(sent.clone());
            all.len()
        };
        let reply = (self.handler)(&sent, n);
        Box::pin(async move {
            let builder = http::Response::builder();
            let resp = match reply {
                Reply::Sse(body) => builder
                    .status(200)
                    .header("content-type", "text/event-stream")
                    .body(reqwest::Body::from(body))?,
                Reply::Status(status, body) => builder
                    .status(status)
                    .header("content-type", "application/json")
                    .body(reqwest::Body::from(body))?,
                Reply::WithHeaders(status, body, headers) => {
                    let mut b = builder.status(status);
                    for (k, v) in headers {
                        b = b.header(k, v);
                    }
                    b.body(reqwest::Body::from(body))?
                }
                Reply::Breaking(first, error) => {
                    let chunks = futures::stream::iter(vec![Ok(bytes::Bytes::from(first)), Err(error)]);
                    // The error comes a little after the first chunk, as from a socket.
                    let delayed = chunks.then(|c| async move {
                        if c.is_err() {
                            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        }
                        c
                    });
                    builder
                        .status(200)
                        .header("content-type", "text/event-stream")
                        .body(reqwest::Body::wrap_stream(delayed))?
                }
                Reply::Stalling(first) => {
                    let chunks = futures::stream::iter(vec![Ok::<_, std::io::Error>(bytes::Bytes::from(first))])
                        .chain(futures::stream::pending());
                    builder
                        .status(200)
                        .header("content-type", "text/event-stream")
                        .body(reqwest::Body::wrap_stream(chunks))?
                }
                Reply::Hang => futures::future::pending().await,
                Reply::Fail(message) => {
                    anyhow::bail!(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, message))
                }
            };
            Ok(reqwest::Response::from(resp))
        })
    }
}

/// A fake transport answering with `handler(sent, n)` (n: 1 for the first request), and what was sent.
pub fn fake(
    handler: impl Fn(&Sent, usize) -> Reply + Send + Sync + 'static,
) -> (Arc<dyn Fetch>, Arc<Mutex<Vec<Sent>>>) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    (
        Arc::new(FakeFetch {
            sent: sent.clone(),
            handler: Box::new(handler),
        }),
        sent,
    )
}

/// Always the same answer.
pub fn fixed(reply: impl Fn() -> Reply + Send + Sync + 'static) -> (Arc<dyn Fetch>, Arc<Mutex<Vec<Sent>>>) {
    fake(move |_, _| reply())
}

/// The bodies sent so far.
pub fn bodies(sent: &Arc<Mutex<Vec<Sent>>>) -> Vec<Value> {
    sent.lock().unwrap().iter().map(|s| s.body.clone()).collect()
}

/// An event stream: `data:` lines (plus `[DONE]`), or named events (`event: <type>`) without `[DONE]`.
pub fn sse(events: &[Value], named: bool) -> String {
    let mut out: String = events
        .iter()
        .map(|e| {
            let name = if named {
                format!("event: {}\n", e["type"].as_str().unwrap_or_default())
            } else {
                String::new()
            };
            format!("{name}data: {e}\n\n")
        })
        .collect();
    if !named {
        out.push_str("data: [DONE]\n\n");
    }
    out
}

pub fn model(v: Value) -> ModelInfo {
    serde_json::from_value(v).expect("a model")
}

pub fn message(v: Value) -> Message {
    serde_json::from_value(v).expect("a message")
}

pub fn assistant(v: Value) -> AssistantMessage {
    serde_json::from_value(v).expect("an assistant message")
}

pub fn user(text: &str) -> Message {
    message(json!({ "role": "user", "content": [{ "type": "text", "text": text }], "timestamp": 0 }))
}

pub fn tool_spec(v: Value) -> ToolSpec {
    serde_json::from_value(v).expect("a tool")
}

/// A request with `maxRetries: 0`, no tools, one user message.
pub fn request(model: ModelInfo, fetch: Arc<dyn Fetch>) -> Request {
    Request {
        model,
        api_key: None,
        system: String::new(),
        messages: vec![user("x")],
        tools: vec![],
        thinking: None,
        max_tokens: None,
        max_retries: Some(0),
        sampling: None,
        signal: Default::default(),
        fetch: Some(fetch),
    }
}

/// Every event, the last one `done`.
pub async fn run(provider: &dyn Provider, req: Request) -> anyhow::Result<(AssistantMessage, Vec<StreamEvent>)> {
    let mut stream = provider.stream(req);
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev?);
    }
    match events.last() {
        Some(StreamEvent::Done { message }) => Ok((message.clone(), events)),
        _ => anyhow::bail!("no done event"),
    }
}

/// The reply's content as JSON, for comparing with the TS expectations.
pub fn content(m: &AssistantMessage) -> Value {
    serde_json::to_value(&m.content).unwrap()
}

/// The provider registered for `api`.
pub fn provider(api: &str) -> Arc<dyn Provider> {
    sasacode_ai::get_provider(api).unwrap()
}

pub fn chat_chunk(delta: Value, finish: Option<&str>) -> Value {
    json!({ "id": "1", "object": "chat.completion.chunk", "created": 0, "model": "m",
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] })
}
