//! The Streamable HTTP transport (`StreamableHTTPClientTransport` of the TS SDK): each message is a
//! POST answered with JSON or an SSE stream; after `notifications/initialized` a GET opens a stream
//! for messages the server sends on its own (e.g. `tools/list_changed`). There is no auth flow, no
//! DELETE on close, and nothing reconnects a session that died: only close() ends the transport.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::protocol::{Classified, Incoming, Transport, classify};
use crate::sse;

/// `DEFAULT_STREAMABLE_HTTP_RECONNECTION_OPTIONS`: for the SSE streams only.
const INITIAL_RECONNECTION_DELAY_MS: f64 = 1000.0;
const MAX_RECONNECTION_DELAY_MS: f64 = 30_000.0;
const RECONNECTION_DELAY_GROW_FACTOR: f64 = 1.5;
const MAX_RETRIES: u32 = 2;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn http_error(message: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("Streamable HTTP error: {message}")
}

struct Inner {
    url: reqwest::Url,
    /// The configured headers; they go on every request and win over ours.
    headers: HeaderMap,
    http: reqwest::Client,
    session_id: Mutex<Option<String>>,
    protocol_version: Mutex<Option<String>>,
    /// Aborts every request and stream (close()).
    abort: CancellationToken,
    sink: Mutex<Option<mpsc::UnboundedSender<Incoming>>>,
    /// A `retry:` the server sent, used for the stream reconnection delay.
    server_retry_ms: Mutex<Option<u64>>,
}

/// How an SSE stream is opened again: from the last event id, answering for the request it served.
#[derive(Clone, Default)]
struct StreamOptions {
    resumption_token: Option<String>,
    replay_message_id: Option<Value>,
}

pub(crate) struct HttpTransport {
    inner: Arc<Inner>,
}

impl HttpTransport {
    pub fn new(url: reqwest::Url, headers: &[(String, String)]) -> anyhow::Result<Self> {
        let mut map = HeaderMap::new();
        for (k, v) in headers {
            let name = HeaderName::from_bytes(k.as_bytes()).map_err(|_| anyhow::anyhow!("Invalid header name: {k}"))?;
            let value = HeaderValue::from_str(v).map_err(|_| anyhow::anyhow!("Invalid header value for {k}"))?;
            map.append(name, value);
        }
        Ok(HttpTransport {
            inner: Arc::new(Inner {
                url,
                headers: map,
                http: sasacode_ai::http::client().clone(),
                session_id: Mutex::new(None),
                protocol_version: Mutex::new(None),
                abort: CancellationToken::new(),
                sink: Mutex::new(None),
                server_retry_ms: Mutex::new(None),
            }),
        })
    }
}

impl Inner {
    fn deliver(&self, message: Value) {
        if let Some(sink) = lock(&self.sink).as_ref() {
            let _ = sink.send(Incoming::Message(message));
        }
    }

    /// `_commonHeaders()`: session and protocol version, then the configured headers over them.
    fn common_headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(id) = lock(&self.session_id)
            .as_deref()
            .and_then(|s| HeaderValue::from_str(s).ok())
        {
            h.insert("mcp-session-id", id);
        }
        if let Some(v) = lock(&self.protocol_version)
            .as_deref()
            .and_then(|s| HeaderValue::from_str(s).ok())
        {
            h.insert("mcp-protocol-version", v);
        }
        for name in self.headers.keys() {
            h.remove(name);
        }
        for (name, value) in &self.headers {
            h.append(name, value.clone());
        }
        h
    }

    async fn post(&self, message: &Value) -> anyhow::Result<reqwest::Response> {
        let mut headers = self.common_headers();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json, text/event-stream"));
        let req = self
            .http
            .post(self.url.clone())
            .headers(headers)
            .body(serde_json::to_vec(message)?)
            .send();
        tokio::select! {
            r = req => Ok(r?),
            _ = self.abort.cancelled() => anyhow::bail!("The operation was aborted."),
        }
    }
}

/// `_startOrAuthSse`: open the GET stream. A 405 means the server offers none, which is fine.
fn start_sse(inner: Arc<Inner>, opts: StreamOptions) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let mut headers = inner.common_headers();
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        if let Some(t) = opts
            .resumption_token
            .as_deref()
            .and_then(|t| HeaderValue::from_str(t).ok())
        {
            headers.insert("last-event-id", t);
        }
        let req = inner.http.get(inner.url.clone()).headers(headers).send();
        let response = tokio::select! {
            r = req => r?,
            _ = inner.abort.cancelled() => anyhow::bail!("The operation was aborted."),
        };
        let status = response.status();
        if !status.is_success() {
            if status.as_u16() == 405 {
                return Ok(());
            }
            return Err(http_error(format!(
                "Failed to open SSE stream: {}",
                status.canonical_reason().unwrap_or_default()
            )));
        }
        tokio::spawn(handle_sse_stream(inner, response, opts, true));
        Ok(())
    })
}

/// `_scheduleReconnection`: try the stream again after a growing delay, at most MAX_RETRIES failures.
fn schedule_reconnection(inner: Arc<Inner>, opts: StreamOptions, attempt: u32) {
    if attempt >= MAX_RETRIES {
        return; // "Maximum reconnection attempts exceeded": reported to onerror, which nobody reads
    }
    let delay = match *lock(&inner.server_retry_ms) {
        Some(ms) => ms as f64,
        None => (INITIAL_RECONNECTION_DELAY_MS * RECONNECTION_DELAY_GROW_FACTOR.powi(attempt as i32))
            .min(MAX_RECONNECTION_DELAY_MS),
    };
    tokio::spawn(async move {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(delay as u64)) => {}
            _ = inner.abort.cancelled() => return,
        }
        if start_sse(inner.clone(), opts.clone()).await.is_err() {
            schedule_reconnection(inner, opts, attempt + 1);
        }
    });
}

/// `_handleSseStream`: deliver each `message` event; a stream that ends before its answer (or the
/// GET stream, whenever it ends) is opened again from the last event id.
async fn handle_sse_stream(inner: Arc<Inner>, response: reqwest::Response, opts: StreamOptions, reconnectable: bool) {
    let mut parser = sse::Parser::default();
    let mut last_event_id: Option<String> = None;
    let mut has_priming_event = false;
    let mut received_response = false;
    let mut body = response.bytes_stream();
    loop {
        let chunk = tokio::select! {
            c = body.next() => c,
            _ = inner.abort.cancelled() => return,
        };
        let Some(Ok(chunk)) = chunk else { break };
        let events = parser.feed(&chunk);
        if let Some(r) = parser.retry {
            *lock(&inner.server_retry_ms) = Some(r);
        }
        for event in events {
            if let Some(id) = event.id.filter(|id| !id.is_empty()) {
                last_event_id = Some(id);
                has_priming_event = true;
            }
            // Events without data are priming events and keep-alives.
            if event.data.is_empty() {
                continue;
            }
            if event.event.as_deref().is_none_or(|e| e == "message") {
                let Ok(mut message) = serde_json::from_str::<Value>(&event.data) else {
                    continue;
                };
                match classify(&message) {
                    None => continue,
                    Some(Classified::Response { .. }) => {
                        received_response = true;
                        if let (Some(id), Some(o)) = (&opts.replay_message_id, message.as_object_mut()) {
                            o.insert("id".into(), id.clone());
                        }
                    }
                    Some(_) => {}
                }
                inner.deliver(message);
            }
        }
    }
    let can_resume = reconnectable || has_priming_event;
    if can_resume && !received_response && !inner.abort.is_cancelled() {
        let next = StreamOptions {
            resumption_token: last_event_id,
            replay_message_id: opts.replay_message_id,
        };
        schedule_reconnection(inner, next, 0);
    }
}

/// The essence of a media type: `text/event-stream; charset=utf-8` → `text/event-stream`.
fn media_type_essence(content_type: Option<&str>) -> Option<String> {
    let ct = content_type?;
    Some(ct.split(';').next().unwrap_or_default().trim().to_ascii_lowercase())
}

#[async_trait]
impl Transport for HttpTransport {
    async fn start(&self, sink: mpsc::UnboundedSender<Incoming>) -> anyhow::Result<()> {
        let mut s = lock(&self.inner.sink);
        if s.is_some() {
            anyhow::bail!("StreamableHTTPClientTransport already started!");
        }
        *s = Some(sink);
        Ok(())
    }

    async fn send(&self, message: Value) -> anyhow::Result<()> {
        let inner = &self.inner;
        let response = inner.post(&message).await?;
        if let Some(id) = response.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *lock(&inner.session_id) = Some(id.to_string());
        }
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_else(|_| "null".into());
            return Err(http_error(format!("Error POSTing to endpoint: {text}")));
        }
        // 202 Accepted: no body. After `initialized`, open the stream for the server's own messages.
        if status.as_u16() == 202 {
            if message.get("method").and_then(Value::as_str) == Some("notifications/initialized")
                && message.get("id").is_none()
            {
                let inner = inner.clone();
                tokio::spawn(async move {
                    let _ = start_sse(inner, StreamOptions::default()).await;
                });
            }
            return Ok(());
        }
        let has_request = message.get("method").is_some() && message.get("id").is_some_and(|id| !id.is_null());
        if !has_request {
            return Ok(());
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        match media_type_essence(content_type.as_deref()).as_deref() {
            Some("text/event-stream") => {
                tokio::spawn(handle_sse_stream(
                    inner.clone(),
                    response,
                    StreamOptions::default(),
                    false,
                ));
                Ok(())
            }
            Some("application/json") => {
                let data: Value = response.json().await?;
                let messages = match data {
                    Value::Array(items) => items,
                    one => vec![one],
                };
                if messages.iter().any(|m| classify(m).is_none()) {
                    anyhow::bail!("invalid JSON-RPC message in the response");
                }
                for m in messages {
                    inner.deliver(m);
                }
                Ok(())
            }
            _ => Err(http_error(format!(
                "Unexpected content type: {}",
                content_type.as_deref().unwrap_or("null")
            ))),
        }
    }

    async fn close(&self) {
        self.inner.abort.cancel();
        let sink = lock(&self.inner.sink).take();
        if let Some(sink) = sink {
            let _ = sink.send(Incoming::Closed);
        }
    }

    fn set_protocol_version(&self, version: &str) {
        *lock(&self.inner.protocol_version) = Some(version.to_string());
    }
}
