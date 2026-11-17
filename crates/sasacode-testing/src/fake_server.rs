//! A stand-in for an OpenAI-compatible model server (vLLM, Ollama, a router): chat completions over
//! SSE and /v1/models, driven by a script of replies (`packages/e2e/test/fake-server.ts`). It can
//! misbehave the ways real servers do — cut a stream short, refuse an effort inside a 200 stream, fail
//! with a status — so end-to-end tests exercise the adapters, the HTTP client and the CLI exactly as
//! a real run does.
//!
//! The server runs on its own thread with its own runtime, so it serves the same from synchronous
//! tests, async tests and the `sasacode-fake-model` binary, and `stop` closes open connections.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::future::IntoFuture;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use bytes::Bytes;
use sasacode_ai::JsonObject;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{utf16_cuts, utf16_len};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCallSpec {
    pub name: String,
    #[serde(default)]
    pub args: JsonObject,
}

impl ToolCallSpec {
    pub fn new(name: impl Into<String>, args: Value) -> Self {
        let args = match args {
            Value::Object(o) => o,
            _ => JsonObject::new(),
        };
        ToolCallSpec {
            name: name.into(),
            args,
        }
    }
}

/// How the tool calls are streamed. By default OpenAI's way: one chunk each, with its index, id and
/// name. `NameLater`: the id and type first, the name in the next chunk, then the arguments.
/// `NoIndex`: no index on any chunk, the arguments in two halves, the name repeated on both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolCallShape {
    #[serde(rename = "name-later")]
    NameLater,
    #[serde(rename = "no-index")]
    NoIndex,
}

/// One scripted reply. Fields are read like the TS read them: an empty string or a zero status counts
/// as not set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_shape: Option<ToolCallShape>,
    /// Stop sending after the content: no finish_reason, no usage, no [DONE] (a dropped connection).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cut: bool,
    /// Answer with this HTTP status and an error body instead of a stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// An error event as the first thing in a 200 stream, as vLLM does for an effort it rejects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_error: Option<String>,
}

impl Reply {
    pub fn text(t: impl Into<String>) -> Self {
        Reply {
            text: Some(t.into()),
            ..Reply::default()
        }
    }
    pub fn reasoning(t: impl Into<String>) -> Self {
        Reply {
            reasoning: Some(t.into()),
            ..Reply::default()
        }
    }
    pub fn tool_calls(calls: Vec<ToolCallSpec>) -> Self {
        Reply {
            tool_calls: Some(calls),
            ..Reply::default()
        }
    }
    pub fn status(code: u16) -> Self {
        Reply {
            status: Some(code),
            ..Reply::default()
        }
    }
    pub fn stream_error(message: impl Into<String>) -> Self {
        Reply {
            stream_error: Some(message.into()),
            ..Reply::default()
        }
    }
    pub fn with_shape(mut self, shape: ToolCallShape) -> Self {
        self.tool_call_shape = Some(shape);
        self
    }
    pub fn with_text(mut self, t: impl Into<String>) -> Self {
        self.text = Some(t.into());
        self
    }
    pub fn with_cut(mut self) -> Self {
        self.cut = true;
        self
    }
}

/// A script file: a JSON array of replies, or `{"replies": [...], "models": [...]}`.
#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    pub replies: Vec<Reply>,
    /// What /models lists (`[{"id": "m"}]` unless the file says otherwise).
    pub models: Vec<Value>,
}

impl Script {
    pub fn parse(text: &str) -> anyhow::Result<Script> {
        let value: Value = serde_json::from_str(text)?;
        match value {
            Value::Array(_) => Ok(Script {
                replies: serde_json::from_value(value)?,
                models: default_models(),
            }),
            Value::Object(mut o) => {
                let replies = match o.remove("replies") {
                    Some(r) => serde_json::from_value(r)?,
                    None => anyhow::bail!("the script has no \"replies\""),
                };
                let models = match o.remove("models") {
                    Some(Value::Array(m)) => m,
                    Some(_) => anyhow::bail!("\"models\" must be an array"),
                    None => default_models(),
                };
                Ok(Script { replies, models })
            }
            _ => anyhow::bail!("a script is an array of replies or an object with \"replies\""),
        }
    }
}

/// `[{"id": "m"}]`: the model `fake/m` the default config uses.
pub fn default_models() -> Vec<Value> {
    vec![json!({ "id": "m" })]
}

struct Shared {
    queue: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Value>>,
    models: Vec<Value>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A running fake server. Dropping it stops it.
pub struct FakeServer {
    url: String,
    shared: Arc<Shared>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for FakeServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeServer")
            .field("url", &self.url)
            .field("left", &self.left())
            .finish()
    }
}

impl FakeServer {
    /// Starts on 127.0.0.1 with a free port; /models lists `[{"id": "m"}]`.
    pub fn start(script: Vec<Reply>) -> std::io::Result<FakeServer> {
        Self::with_models(script, default_models())
    }

    /// Starts with these entries for /models, each listed as `{"object":"model","owned_by":"fake",...entry}`.
    pub fn with_models(script: Vec<Reply>, models: Vec<Value>) -> std::io::Result<FakeServer> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let listener = {
            let _guard = runtime.enter();
            tokio::net::TcpListener::from_std(listener)?
        };
        let shared = Arc::new(Shared {
            queue: Mutex::new(script.into()),
            requests: Mutex::new(Vec::new()),
            models,
        });
        let app = Router::new().fallback(handle).with_state(shared.clone());
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("fake-model-server".into())
            .spawn(move || {
                runtime.block_on(async move {
                    tokio::select! {
                        _ = axum::serve(listener, app).into_future() => {}
                        _ = stopped => {}
                    }
                });
                // Like Bun's `server.stop(true)`: connections still open are closed, not waited for.
                runtime.shutdown_background();
            })?;
        Ok(FakeServer {
            url: format!("http://127.0.0.1:{port}/v1"),
            shared,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// Starts with a parsed script file.
    pub fn from_script(script: Script) -> std::io::Result<FakeServer> {
        Self::with_models(script.replies, script.models)
    }

    /// The base URL, `http://127.0.0.1:<port>/v1`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Chat request bodies in order, parsed.
    pub fn requests(&self) -> Vec<Value> {
        lock(&self.shared.requests).clone()
    }

    /// Replies not used yet.
    pub fn left(&self) -> usize {
        lock(&self.shared.queue).len()
    }

    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn handle(State(shared): State<Arc<Shared>>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    if path.ends_with("/models") {
        let data: Vec<Value> = shared.models.iter().map(listed_model).collect();
        return json_response(StatusCode::OK, &json!({ "object": "list", "data": data }));
    }
    if !path.ends_with("/chat/completions") {
        return plain_response(StatusCode::NOT_FOUND, "not found".to_string());
    }
    let body = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            return plain_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not read the request: {e}"),
            );
        }
    };
    let body: Value = match serde_json::from_slice(&body) {
        Ok(v) => js_value(&v),
        Err(e) => {
            return plain_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the request is not JSON: {e}"),
            );
        }
    };
    let model = body.get("model").cloned();
    let served = {
        let mut requests = lock(&shared.requests);
        requests.push(body);
        requests.len()
    };
    let Some(reply) = lock(&shared.queue).pop_front() else {
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({ "error": { "message": "the fake server has no more replies" } }),
        );
    };
    if let Some(status) = reply.status.filter(|&s| s != 0) {
        let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        return json_response(
            code,
            &json!({ "error": { "message": format!("fake failure {status}") } }),
        );
    }
    let (events, done) = stream_events(model.as_ref(), &reply, served);
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(sse_body(events, done))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// `{ object: "model", owned_by: "fake", ...m }`: a key the entry repeats keeps its place.
fn listed_model(m: &Value) -> Value {
    let mut o = JsonObject::new();
    o.insert("object".into(), json!("model"));
    o.insert("owned_by".into(), json!("fake"));
    if let Value::Object(entry) = m {
        for (k, v) in entry {
            o.insert(k.clone(), v.clone());
        }
    }
    Value::Object(o)
}

/// What Bun's `Response.json` sends.
fn json_response(status: StatusCode, body: &Value) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json;charset=utf-8")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn plain_response(status: StatusCode, text: String) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain;charset=utf-8")
        .body(Body::from(text))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// One `chat.completion.chunk`. `model` is the request's (left out when it sent none, as
/// `JSON.stringify` drops an undefined field).
fn chunk(model: Option<&Value>, delta: Value, finish: Option<&str>) -> Value {
    let mut o = JsonObject::new();
    o.insert("id".into(), json!("c"));
    o.insert("object".into(), json!("chat.completion.chunk"));
    o.insert("created".into(), json!(0));
    if let Some(m) = model {
        o.insert("model".into(), m.clone());
    }
    o.insert(
        "choices".into(),
        json!([{ "index": 0, "delta": delta, "finish_reason": finish }]),
    );
    Value::Object(o)
}

/// The events of a reply, and whether `data: [DONE]` follows them. `served` numbers the request
/// (1-based) and makes the tool-call ids: the TS drew them at random, here they repeat from run to
/// run so recordings compare, and stay unique per reply.
fn stream_events(model: Option<&Value>, r: &Reply, served: usize) -> (Vec<Value>, bool) {
    let mut events = Vec::new();
    if let Some(message) = r.stream_error.as_deref().filter(|s| !s.is_empty()) {
        events.push(json!({ "error": { "message": message, "type": "BadRequestError", "code": 400 } }));
        return (events, false);
    }
    if let Some(reasoning) = r.reasoning.as_deref().filter(|s| !s.is_empty()) {
        for part in split(reasoning) {
            events.push(chunk(model, json!({ "reasoning_content": part }), None));
        }
    }
    if let Some(text) = r.text.as_deref().filter(|s| !s.is_empty()) {
        for part in split(text) {
            events.push(chunk(model, json!({ "content": part }), None));
        }
    }
    let calls = r.tool_calls.as_deref().unwrap_or_default();
    for (i, c) in calls.iter().enumerate() {
        let id = format!("call_{i}_{}", id_suffix(served));
        let args = js_value(&Value::Object(c.args.clone())).to_string();
        let parts = match r.tool_call_shape {
            Some(ToolCallShape::NameLater) => vec![
                json!({ "index": i, "id": id, "type": "function", "function": { "arguments": "" } }),
                json!({ "index": i, "function": { "name": c.name } }),
                json!({ "index": i, "function": { "arguments": args } }),
            ],
            Some(ToolCallShape::NoIndex) => {
                let half = utf16_cuts(&args, &[utf16_len(&args).div_ceil(2)])[0];
                vec![
                    json!({ "id": id, "type": "function", "function": { "name": c.name, "arguments": &args[..half] } }),
                    json!({ "function": { "name": c.name, "arguments": &args[half..] } }),
                ]
            }
            None => vec![
                json!({ "index": i, "id": id, "type": "function", "function": { "name": c.name, "arguments": args } }),
            ],
        };
        for part in parts {
            events.push(chunk(model, json!({ "tool_calls": [part] }), None));
        }
    }
    if !r.cut {
        events.push(chunk(
            model,
            json!({}),
            Some(if calls.is_empty() { "stop" } else { "tool_calls" }),
        ));
        let mut usage = JsonObject::new();
        usage.insert("id".into(), json!("c"));
        usage.insert("object".into(), json!("chat.completion.chunk"));
        usage.insert("created".into(), json!(0));
        if let Some(m) = model {
            usage.insert("model".into(), m.clone());
        }
        usage.insert("choices".into(), json!([]));
        usage.insert(
            "usage".into(),
            json!({ "prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120 }),
        );
        events.push(Value::Object(usage));
    }
    (events, !r.cut)
}

/// A value as JS holds it after `JSON.parse`: a whole number written `1.0` is the number 1, so it
/// compares equal to `1` and prints as `1` again.
fn js_value(v: &Value) -> Value {
    match v {
        Value::Number(n) if n.is_f64() => n.as_f64().map_or(Value::Null, sasacode_ai::json::js_number),
        Value::Array(a) => Value::Array(a.iter().map(js_value).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), js_value(v))).collect()),
        other => other.clone(),
    }
}

/// Six base-36 characters, like the TS's `Math.random().toString(36).slice(2, 8)`.
fn id_suffix(n: usize) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut n = n;
    let mut out = [b'0'; 6];
    for slot in out.iter_mut().rev() {
        *slot = DIGITS[n % 36];
        n /= 36;
    }
    out.iter().map(|&b| b as char).collect()
}

/// A few pieces, so the client really parses a stream of deltas.
fn split(s: &str) -> Vec<&str> {
    let n = utf16_len(s).div_ceil(3).max(1);
    let cuts = utf16_cuts(s, &[n, 2 * n]);
    [&s[..cuts[0]], &s[cuts[0]..cuts[1]], &s[cuts[1]..]]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect()
}

/// `data: <event>\n\n` for each event with a millisecond after each, then `data: [DONE]\n\n` when
/// `done`, then the end of the body (a clean end even when cut: the client sees the stream stop).
fn sse_body(events: Vec<Value>, done: bool) -> Body {
    let frames: VecDeque<(Bytes, bool)> = events
        .into_iter()
        .map(|e| (Bytes::from(format!("data: {e}\n\n")), true))
        .chain(done.then(|| (Bytes::from_static(b"data: [DONE]\n\n"), false)))
        .collect();
    let stream = futures::stream::unfold((frames, false), |(mut frames, pause)| async move {
        if pause {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let (bytes, pause_after) = frames.pop_front()?;
        Some((Ok::<_, Infallible>(bytes), (frames, pause_after)))
    });
    Body::from_stream(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_six_base36_characters() {
        assert_eq!(id_suffix(1), "000001");
        assert_eq!(id_suffix(36), "000010");
        assert_eq!(id_suffix(36usize.pow(6) + 5), "000005");
    }

    #[test]
    fn text_is_split_in_three_by_utf16_units() {
        assert_eq!(split("abcdefg"), vec!["abc", "def", "g"]);
        assert_eq!(split("hi"), vec!["h", "i"]);
        assert_eq!(split("日本語のテキスト"), vec!["日本語", "のテキ", "スト"]);
    }

    #[test]
    fn whole_numbers_read_as_js_reads_them() {
        let v: Value = serde_json::from_str(r#"{"a":1.0,"b":[2.5,3.0],"c":"1.0"}"#).unwrap();
        assert_eq!(js_value(&v).to_string(), r#"{"a":1,"b":[2.5,3],"c":"1.0"}"#);
        assert_eq!(js_value(&v)["a"], json!(1));
    }

    #[test]
    fn scripts_are_an_array_or_an_object() {
        let s = Script::parse(r#"[{"text":"hi"},{"status":500}]"#).unwrap();
        assert_eq!(s.replies, vec![Reply::text("hi"), Reply::status(500)]);
        assert_eq!(s.models, default_models());
        let s = Script::parse(r#"{"replies":[{"toolCalls":[{"name":"read","args":{"path":"a"}}],"toolCallShape":"no-index","cut":true}],"models":[]}"#).unwrap();
        let expected = Reply::tool_calls(vec![ToolCallSpec::new("read", json!({ "path": "a" }))])
            .with_shape(ToolCallShape::NoIndex)
            .with_cut();
        assert_eq!(s.replies, vec![expected]);
        assert!(s.models.is_empty());
        assert!(Script::parse(r#"{"models":[]}"#).is_err());
    }
}
