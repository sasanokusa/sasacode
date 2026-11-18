//! The MCP client protocol: JSON-RPC requests matched to responses, notifications, timeouts and
//! cancellation over a [`Transport`]. Hand-written to do what `Client` and `Protocol` of the TS SDK
//! (`@modelcontextprotocol/sdk` 1.30.1, `client/index.js`, `shared/protocol.js`) did for sasacode
//! (rust/research/mcp.md): the same handshake, default timeout, error texts and close semantics.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// The version sent in `initialize` (`LATEST_PROTOCOL_VERSION` of the TS SDK).
pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";
/// Versions a server may answer with (`SUPPORTED_PROTOCOL_VERSIONS`).
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 5] = [
    LATEST_PROTOCOL_VERSION,
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];
/// Requests without their own timeout (`DEFAULT_REQUEST_TIMEOUT_MSEC`).
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_millis(60_000);

pub const CONNECTION_CLOSED: i64 = -32000;
pub const REQUEST_TIMEOUT: i64 = -32001;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;

/// What an aborted signal says when it becomes text (`String(signal.reason)` under Bun).
pub(crate) const ABORTED: &str = "AbortError: The operation was aborted.";

/// A JSON-RPC error from the server, or one the protocol raises (timeout, closed connection).
/// Its text is `MCP error <code>: <message>`, as the TS `McpError` wrote it.
#[derive(Debug, Clone, PartialEq)]
pub struct McpError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl McpError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        McpError {
            code,
            message: message.into(),
            data: None,
        }
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MCP error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for McpError {}

/// What a transport hands the client.
#[derive(Debug)]
pub(crate) enum Incoming {
    Message(Value),
    /// The connection is gone (stdio: the process exited; HTTP: close() was called).
    Closed,
}

/// A way to reach a server: stdio child process or Streamable HTTP.
#[async_trait]
pub(crate) trait Transport: Send + Sync {
    /// Begin: messages and the close go to `sink` from now on.
    async fn start(&self, sink: mpsc::UnboundedSender<Incoming>) -> anyhow::Result<()>;
    async fn send(&self, message: Value) -> anyhow::Result<()>;
    async fn close(&self);
    /// HTTP sends the negotiated version in a header on every later request.
    fn set_protocol_version(&self, _version: &str) {}
}

/// One JSON-RPC message as the TS SDK classifies it (`JSONRPCMessageSchema`). Anything else is dropped.
#[derive(Debug)]
pub(crate) enum Classified {
    Request { id: Value, method: String },
    Notification { method: String },
    Response { id: Value, result: Value },
    Error { id: Option<Value>, error: McpError },
}

fn is_id(v: &Value) -> bool {
    v.is_string() || v.as_f64().is_some_and(|n| n.fract() == 0.0)
}

pub(crate) fn classify(v: &Value) -> Option<Classified> {
    let o = v.as_object()?;
    if o.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return None;
    }
    if o.get("params").is_some_and(|p| !p.is_object()) {
        return None;
    }
    if let Some(method) = o.get("method").and_then(Value::as_str) {
        return match o.get("id") {
            Some(id) if is_id(id) => Some(Classified::Request {
                id: id.clone(),
                method: method.to_string(),
            }),
            Some(_) => None,
            None => Some(Classified::Notification {
                method: method.to_string(),
            }),
        };
    }
    if let Some(result) = o.get("result") {
        let id = o.get("id").filter(|id| is_id(id))?;
        return result.is_object().then(|| Classified::Response {
            id: id.clone(),
            result: result.clone(),
        });
    }
    let e = o.get("error")?.as_object()?;
    let code = e.get("code").and_then(Value::as_i64)?;
    let message = e.get("message").and_then(Value::as_str)?.to_string();
    let id = o.get("id").filter(|id| is_id(id)).cloned();
    Some(Classified::Error {
        id,
        error: McpError {
            code,
            message,
            data: e.get("data").cloned(),
        },
    })
}

/// `Number(response.id)`: the ids we send are numbers; a server echoing "3" still matches.
fn id_number(id: &Value) -> Option<i64> {
    match id {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

type Pending = oneshot::Sender<Result<Value, McpError>>;
type Callback = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct ToolMeta {
    /// Tools that declared an outputSchema (`_cachedToolOutputValidators`).
    output_schema: HashSet<String>,
    /// Tools whose `execution.taskSupport` is "required".
    task_required: HashSet<String>,
}

struct Inner {
    id: u64,
    client_info: Value,
    next_id: AtomicI64,
    pending: Mutex<HashMap<i64, Pending>>,
    transport: Mutex<Option<Arc<dyn Transport>>>,
    capabilities: Mutex<Option<Value>>,
    on_close: Mutex<Option<Callback>>,
    on_tool_list_changed: Mutex<Option<Callback>>,
    tools: Mutex<ToolMeta>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// An MCP client (`Client` of the TS SDK). Cheap to clone; clones are the same connection.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("id", &self.inner.id).finish()
    }
}

/// A [`Client`] that may be gone.
#[derive(Clone)]
pub(crate) struct WeakClient(Weak<Inner>);

impl WeakClient {
    pub fn upgrade(&self) -> Option<Client> {
        self.0.upgrade().map(|inner| Client { inner })
    }
    pub fn is(&self, client: &Client) -> bool {
        std::ptr::eq(self.0.as_ptr(), Arc::as_ptr(&client.inner))
    }
}

/// Options of one request.
pub(crate) struct RequestOptions {
    pub timeout: Duration,
    pub signal: Option<CancellationToken>,
}

impl Default for RequestOptions {
    fn default() -> Self {
        RequestOptions {
            timeout: DEFAULT_REQUEST_TIMEOUT,
            signal: None,
        }
    }
}

impl Client {
    pub fn new(name: &str, version: &str) -> Client {
        static IDS: AtomicU64 = AtomicU64::new(1);
        Client {
            inner: Arc::new(Inner {
                id: IDS.fetch_add(1, Ordering::Relaxed),
                client_info: json!({ "name": name, "version": version }),
                next_id: AtomicI64::new(0),
                pending: Mutex::new(HashMap::new()),
                transport: Mutex::new(None),
                capabilities: Mutex::new(None),
                on_close: Mutex::new(None),
                on_tool_list_changed: Mutex::new(None),
                tools: Mutex::new(ToolMeta::default()),
            }),
        }
    }

    /// Two handles to the same connection.
    pub fn same(&self, other: &Client) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// A handle that does not keep the connection alive: for callbacks the client itself holds.
    pub(crate) fn downgrade(&self) -> WeakClient {
        WeakClient(Arc::downgrade(&self.inner))
    }

    /// Called once when the connection closes, whoever closed it (`client.onclose`).
    pub(crate) fn set_on_close(&self, f: impl Fn() + Send + Sync + 'static) {
        *lock(&self.inner.on_close) = Some(Arc::new(f));
    }

    /// `setNotificationHandler(ToolListChangedNotificationSchema, f)`.
    pub(crate) fn set_tool_list_changed_handler(&self, f: impl Fn() + Send + Sync + 'static) {
        *lock(&self.inner.on_tool_list_changed) = Some(Arc::new(f));
    }

    /// What the server said it supports in `initialize`.
    pub fn server_capabilities(&self) -> Option<Value> {
        lock(&self.inner.capabilities).clone()
    }

    /// Attach the transport, start it and run the `initialize` handshake.
    pub(crate) async fn connect(&self, transport: Arc<dyn Transport>) -> anyhow::Result<()> {
        *lock(&self.inner.transport) = Some(transport.clone());
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(dispatch(Arc::downgrade(&self.inner), rx));
        transport.start(tx).await?;
        let r = self.initialize(&transport).await;
        if r.is_err() {
            // Disconnect if initialization fails.
            let me = self.clone();
            tokio::spawn(async move { me.close().await });
        }
        r
    }

    async fn initialize(&self, transport: &Arc<dyn Transport>) -> anyhow::Result<()> {
        let params = json!({
            "protocolVersion": LATEST_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": self.inner.client_info,
        });
        let result = self
            .request("initialize", Some(params), RequestOptions::default())
            .await?;
        let version = result.get("protocolVersion").and_then(Value::as_str);
        let capabilities = result.get("capabilities").filter(|c| c.is_object());
        let (Some(version), Some(capabilities)) = (version, capabilities) else {
            anyhow::bail!("Server sent invalid initialize result: {result}");
        };
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&version) {
            anyhow::bail!("Server's protocol version is not supported: {version}");
        }
        *lock(&self.inner.capabilities) = Some(capabilities.clone());
        transport.set_protocol_version(version);
        self.notification("notifications/initialized", None).await
    }

    fn transport(&self) -> Option<Arc<dyn Transport>> {
        lock(&self.inner.transport).clone()
    }

    pub(crate) async fn notification(&self, method: &str, params: Option<Value>) -> anyhow::Result<()> {
        let Some(transport) = self.transport() else {
            anyhow::bail!("Not connected");
        };
        let mut m = Map::new();
        m.insert("method".into(), method.into());
        if let Some(p) = params {
            m.insert("params".into(), p);
        }
        m.insert("jsonrpc".into(), "2.0".into());
        transport.send(Value::Object(m)).await
    }

    /// Send a request and wait for its result, the timeout, or the abort signal. On a timeout or
    /// an abort the server is told with `notifications/cancelled`, and a late answer is ignored.
    pub(crate) async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        opts: RequestOptions,
    ) -> anyhow::Result<Value> {
        let Some(transport) = self.transport() else {
            anyhow::bail!("Not connected");
        };
        if opts.signal.as_ref().is_some_and(CancellationToken::is_cancelled) {
            anyhow::bail!("The operation was aborted.");
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let mut m = Map::new();
        m.insert("method".into(), method.into());
        if let Some(p) = params {
            m.insert("params".into(), p);
        }
        m.insert("jsonrpc".into(), "2.0".into());
        m.insert("id".into(), id.into());
        let (tx, mut rx) = oneshot::channel();
        lock(&self.inner.pending).insert(id, tx);
        // Sent on its own task: a timeout must not cut a message in half on the wire.
        let (etx, mut erx) = oneshot::channel::<anyhow::Error>();
        let sender = transport.clone();
        tokio::spawn(async move {
            if let Err(e) = sender.send(Value::Object(m)).await {
                let _ = etx.send(e);
            }
        });
        let signal = opts.signal.unwrap_or_default();
        tokio::select! {
            r = &mut rx => match r {
                Ok(r) => r.map_err(anyhow::Error::from),
                Err(_) => Err(McpError::new(CONNECTION_CLOSED, "Connection closed").into()),
            },
            Ok(e) = &mut erx => {
                lock(&self.inner.pending).remove(&id);
                Err(e)
            }
            _ = tokio::time::sleep(opts.timeout) => {
                let e = McpError {
                    code: REQUEST_TIMEOUT,
                    message: "Request timed out".into(),
                    data: Some(json!({ "timeout": opts.timeout.as_millis() as u64 })),
                };
                Err(self.cancel(id, &format!("McpError: {e}"), e).into())
            }
            _ = signal.cancelled() => {
                Err(self.cancel(id, ABORTED, McpError::new(REQUEST_TIMEOUT, ABORTED)).into())
            }
        }
    }

    fn cancel(&self, id: i64, reason: &str, error: McpError) -> McpError {
        lock(&self.inner.pending).remove(&id);
        let me = self.clone();
        let params = json!({ "requestId": id, "reason": reason });
        tokio::spawn(async move {
            let _ = me.notification("notifications/cancelled", Some(params)).await;
        });
        error
    }

    pub(crate) async fn list_tools(&self, cursor: Option<&str>) -> anyhow::Result<Value> {
        let params = cursor.map(|c| json!({ "cursor": c }));
        let r = self.request("tools/list", params, RequestOptions::default()).await?;
        let Some(tools) = r.get("tools").and_then(Value::as_array) else {
            anyhow::bail!("invalid tools/list result: no tools array");
        };
        for t in tools {
            let named = t.get("name").is_some_and(Value::is_string);
            let schema = t.get("inputSchema").and_then(|s| s.get("type")).and_then(Value::as_str) == Some("object");
            if !named || !schema {
                anyhow::bail!("invalid tools/list result: a tool needs a name and an inputSchema of type \"object\"");
            }
        }
        // `cacheToolMetadata`: what each listed page says, replacing what the page before said.
        let mut meta = ToolMeta::default();
        for t in tools {
            let name = t.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
            if t.get("outputSchema").is_some_and(|s| !s.is_null()) {
                meta.output_schema.insert(name.clone());
            }
            let support = t
                .get("execution")
                .and_then(|e| e.get("taskSupport"))
                .and_then(Value::as_str);
            if support == Some("required") {
                meta.task_required.insert(name);
            }
        }
        *lock(&self.inner.tools) = meta;
        Ok(r)
    }

    /// `callTool`: the tools/call result, checked against what tools/list said about the tool.
    /// (The TS SDK also validated structuredContent against the outputSchema with Ajv; that is not done here.)
    pub(crate) async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        timeout: Duration,
        signal: Option<CancellationToken>,
    ) -> anyhow::Result<Value> {
        if lock(&self.inner.tools).task_required.contains(name) {
            return Err(McpError::new(
                INVALID_REQUEST,
                format!(
                    "Tool \"{name}\" requires task-based execution. Use client.experimental.tasks.callToolStream() instead."
                ),
            )
            .into());
        }
        let params = json!({ "name": name, "arguments": arguments });
        let r = self
            .request("tools/call", Some(params), RequestOptions { timeout, signal })
            .await?;
        if lock(&self.inner.tools).output_schema.contains(name) {
            let structured = r.get("structuredContent").is_some_and(|s| !s.is_null());
            let is_error = r.get("isError").and_then(Value::as_bool).unwrap_or(false);
            if !structured && !is_error {
                return Err(McpError::new(
                    INVALID_REQUEST,
                    format!("Tool {name} has an output schema but did not return structured content"),
                )
                .into());
            }
        }
        Ok(r)
    }

    pub(crate) async fn list_prompts(&self, cursor: Option<&str>) -> anyhow::Result<Value> {
        let params = cursor.map(|c| json!({ "cursor": c }));
        let r = self.request("prompts/list", params, RequestOptions::default()).await?;
        let ok = r
            .get("prompts")
            .and_then(Value::as_array)
            .is_some_and(|ps| ps.iter().all(|p| p.get("name").is_some_and(Value::is_string)));
        if !ok {
            anyhow::bail!("invalid prompts/list result: prompts need names");
        }
        Ok(r)
    }

    pub(crate) async fn get_prompt(&self, name: &str, arguments: Map<String, Value>) -> anyhow::Result<Value> {
        let params = json!({ "name": name, "arguments": arguments });
        let r = self
            .request("prompts/get", Some(params), RequestOptions::default())
            .await?;
        if !r.get("messages").is_some_and(Value::is_array) {
            anyhow::bail!("invalid prompts/get result: no messages array");
        }
        Ok(r)
    }

    /// Close the connection (`client.close()`): stdio ends the process (stdin closed, then
    /// SIGTERM, then SIGKILL); HTTP stops its requests. The close callback runs when it is gone.
    pub async fn close(&self) {
        if let Some(t) = self.transport() {
            t.close().await;
        }
    }
}

/// Handles what the transport delivers, in order (`Protocol.connect`'s onmessage/onclose).
async fn dispatch(client: Weak<Inner>, mut rx: mpsc::UnboundedReceiver<Incoming>) {
    while let Some(incoming) = rx.recv().await {
        let Some(inner) = client.upgrade() else { return };
        match incoming {
            Incoming::Closed => {
                on_closed(&inner);
                return;
            }
            Incoming::Message(v) => match classify(&v) {
                Some(Classified::Response { id, result }) => respond(&inner, &id, Ok(result)),
                Some(Classified::Error { id: Some(id), error }) => respond(&inner, &id, Err(error)),
                Some(Classified::Error { id: None, .. }) | None => {}
                Some(Classified::Request { id, method }) => {
                    // The SDK answers ping; this client offers nothing else (no sampling, roots, elicitation).
                    let reply = if method == "ping" {
                        json!({ "result": {}, "jsonrpc": "2.0", "id": id })
                    } else {
                        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": METHOD_NOT_FOUND, "message": "Method not found" } })
                    };
                    if let Some(t) = lock(&inner.transport).clone() {
                        tokio::spawn(async move {
                            let _ = t.send(reply).await;
                        });
                    }
                }
                Some(Classified::Notification { method }) => {
                    if method == "notifications/tools/list_changed" {
                        let handler = lock(&inner.on_tool_list_changed).clone();
                        if let Some(h) = handler {
                            h();
                        }
                    }
                }
            },
        }
    }
}

fn respond(inner: &Inner, id: &Value, r: Result<Value, McpError>) {
    let Some(id) = id_number(id) else { return };
    let pending = lock(&inner.pending).remove(&id);
    if let Some(tx) = pending {
        let _ = tx.send(r);
    }
}

/// `Protocol._onclose`: forget the transport, tell the owner, fail what is still waiting.
fn on_closed(inner: &Inner) {
    let pending: Vec<Pending> = lock(&inner.pending).drain().map(|(_, tx)| tx).collect();
    *lock(&inner.transport) = None;
    let on_close = lock(&inner.on_close).clone();
    if let Some(f) = on_close {
        f();
    }
    for tx in pending {
        let _ = tx.send(Err(McpError::new(CONNECTION_CLOSED, "Connection closed")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_classified_like_the_sdk_schema() {
        assert!(matches!(
            classify(&json!({"jsonrpc": "2.0", "id": 1, "result": {}})),
            Some(Classified::Response { .. })
        ));
        assert!(matches!(
            classify(&json!({"jsonrpc": "2.0", "id": "1", "method": "ping"})),
            Some(Classified::Request { .. })
        ));
        assert!(matches!(
            classify(&json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})),
            Some(Classified::Notification { .. })
        ));
        let Some(Classified::Error { error, .. }) =
            classify(&json!({"jsonrpc": "2.0", "id": 2, "error": {"code": -32603, "message": "tools are broken"}}))
        else {
            panic!("not an error response");
        };
        assert_eq!(error.to_string(), "MCP error -32603: tools are broken");
        assert!(classify(&json!({"id": 1, "result": {}})).is_none());
        assert!(classify(&json!({"jsonrpc": "2.0", "id": 1, "result": 3})).is_none());
        assert_eq!(id_number(&json!("7")), Some(7));
    }
}
