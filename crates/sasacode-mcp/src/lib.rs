//! The MCP client (`packages/mcp/src/index.ts`): servers configured in `mcpServers` connect in the
//! background; their tools become tools (`mcp__<server>__<tool>`) and their prompts become slash
//! commands. The JSON-RPC client the TS took from `@modelcontextprotocol/sdk` is in [`protocol`],
//! [`stdio`] and [`streamable_http`] (rust/research/mcp.md).

pub mod protocol;
mod sse;
pub mod stdio;
mod streamable_http;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use sasacode_ai::{ImageContent, UserContent};
use sasacode_plugin_api::hooks::SessionEndEvent;
use sasacode_plugin_api::{
    Command, CommandContext, InjectWhen, Lang, NotifyLevel, PluginApi, SelectOption, SessionEndReason, ToolDef,
    ToolKind, ToolResult, create_private_dir, error_result, hook, sasacode_home, save_output,
};

pub use protocol::{Client, McpError};

use protocol::Transport;
use stdio::{StdioParams, StdioTransport};
use streamable_http::HttpTransport;

/// How a server is reached.
#[derive(Debug, Clone, PartialEq)]
pub enum McpTransportConfig {
    /// A child process speaking MCP on stdio.
    Stdio {
        command: String,
        args: Vec<String>,
        env: IndexMap<String, String>,
        cwd: Option<String>,
    },
    /// A Streamable HTTP endpoint.
    Http {
        url: String,
        headers: IndexMap<String, String>,
    },
}

/// Tool names: `mcp__<server>__<tool>` (prefixed) or the server's own names (plain).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolNames {
    #[default]
    Prefixed,
    Plain,
}

/// One entry of `mcpServers` (`McpServerConfig`).
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerConfig {
    pub transport: McpTransportConfig,
    pub disabled: bool,
    /// Send this server's tool definitions even when tool search defers the rest.
    pub always_load: bool,
    /// Per-call timeout in ms (default 10 minutes).
    pub timeout: Option<f64>,
    /// "plain" registers tools under their own names instead of mcp__<server>__<tool>.
    pub tool_names: ToolNames,
}

/// In a stdio server's `cwd`: the project folder sasacode runs in.
pub const PROJECT_CWD: &str = "${SASACODE_PROJECT}";

/// Servers by name, in config order.
pub type McpServers = IndexMap<String, McpServerConfig>;

/// JS truthiness of a config value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn string_map(v: Option<&Value>) -> IndexMap<String, String> {
    v.and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

impl McpServerConfig {
    /// A stdio server.
    pub fn stdio(command: impl Into<String>, args: Vec<String>) -> Self {
        McpServerConfig {
            transport: McpTransportConfig::Stdio {
                command: command.into(),
                args,
                env: IndexMap::new(),
                cwd: None,
            },
            disabled: false,
            always_load: false,
            timeout: None,
            tool_names: ToolNames::Prefixed,
        }
    }

    /// A Streamable HTTP server.
    pub fn http(url: impl Into<String>, headers: IndexMap<String, String>) -> Self {
        McpServerConfig {
            transport: McpTransportConfig::Http {
                url: url.into(),
                headers,
            },
            disabled: false,
            always_load: false,
            timeout: None,
            tool_names: ToolNames::Prefixed,
        }
    }

    /// Read one entry of `mcpServers` as the TS used it: `command` present means stdio, otherwise
    /// `url`. The CLI checks only that entries are objects, so odd values are read leniently.
    pub fn from_json(v: &Value) -> Self {
        let transport = if v.get("command").is_some() {
            McpTransportConfig::Stdio {
                command: v.get("command").and_then(Value::as_str).unwrap_or_default().to_string(),
                args: v
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
                    .unwrap_or_default(),
                env: string_map(v.get("env")),
                cwd: v.get("cwd").and_then(Value::as_str).map(str::to_string),
            }
        } else {
            McpTransportConfig::Http {
                url: v.get("url").and_then(Value::as_str).unwrap_or_default().to_string(),
                headers: string_map(v.get("headers")),
            }
        };
        McpServerConfig {
            transport,
            disabled: truthy(v.get("disabled")),
            always_load: truthy(v.get("alwaysLoad")),
            timeout: v.get("timeout").and_then(Value::as_f64),
            tool_names: if v.get("toolNames").and_then(Value::as_str) == Some("plain") {
                ToolNames::Plain
            } else {
                ToolNames::Prefixed
            },
        }
    }

    /// `mcpServers` from config: every entry that is an object, in order.
    pub fn servers_from_json(v: &Value) -> McpServers {
        v.as_object()
            .map(|o| {
                o.iter()
                    .filter(|(_, s)| s.is_object())
                    .map(|(k, s)| (k.clone(), Self::from_json(s)))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl<'de> serde::Deserialize<'de> for McpServerConfig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(McpServerConfig::from_json(&Value::deserialize(d)?))
    }
}

const MAX_OUTPUT: usize = 100_000;
/// A server that fell over is reconnected this many times (with growing gaps), then left to `/mcp reconnect`.
const MAX_RECONNECTS: u32 = 5;
const RECONNECT_DELAY_MS: f64 = 2_000.0;
const MAX_RECONNECT_DELAY_MS: f64 = 30_000.0;
/// tools/call without a `timeout` in the config: 10 minutes, a hard limit.
const DEFAULT_CALL_TIMEOUT_MS: f64 = 600_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Connected,
    Failed,
    Closed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Connecting => "connecting",
            Status::Connected => "connected",
            Status::Failed => "failed",
            Status::Closed => "closed",
        }
    }
}

/// One server as it stands (`ServerState`).
#[derive(Debug, Clone)]
pub struct ServerState {
    pub name: String,
    pub status: Status,
    pub error: Option<String>,
    pub tools: Vec<String>,
    pub client: Option<Client>,
    pub stderr: String,
    /// Tool names on the model's list, so a tool the server drops can be taken off again.
    pub registered: Vec<String>,
    /// Reconnects since the last successful connection: capped, reset by connecting or `/mcp reconnect`.
    pub attempts: u32,
    /// The pending reconnect, if any.
    timer: Option<tokio::task::AbortHandle>,
}

type StateRef = Arc<Mutex<ServerState>>;

fn lock(m: &Mutex<ServerState>) -> std::sync::MutexGuard<'_, ServerState> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The servers [`connect_servers`] manages, in config order. Reads return snapshots.
#[derive(Clone, Default)]
pub struct ServerStates {
    servers: Arc<IndexMap<String, StateRef>>,
}

impl ServerStates {
    pub fn get(&self, name: &str) -> Option<ServerState> {
        self.servers.get(name).map(|s| lock(s).clone())
    }
    pub fn snapshot(&self) -> Vec<ServerState> {
        self.servers.values().map(|s| lock(s).clone()).collect()
    }
    pub fn names(&self) -> Vec<String> {
        self.servers.keys().cloned().collect()
    }
    pub fn len(&self) -> usize {
        self.servers.len()
    }
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }
}

pub type OnChange = Arc<dyn Fn(&ServerStates) + Send + Sync>;

/// `${VAR}` in env values and headers comes from the environment, so tokens need not sit in config files.
fn expand(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut rest = v;
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let name_len = after
            .char_indices()
            .take_while(|(i, c)| c.is_ascii_alphabetic() || *c == '_' || (*i > 0 && c.is_ascii_digit()))
            .count();
        if name_len > 0 && after[name_len..].starts_with('}') {
            out.push_str(&std::env::var(&after[..name_len]).unwrap_or_default());
            rest = &after[name_len + 1..];
        } else {
            out.push_str("${");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Where a stdio server starts. A `cwd` is taken as written (`${SASACODE_PROJECT}` is the project;
/// a relative one is the process's to resolve, as before). With none the server starts in a folder
/// of its own under `home`, never in the project: `npx` and `uvx` run what the current folder's
/// `node_modules` or `pyproject.toml` name before what the config names.
fn server_cwd(home: &Path, project: &Path, name: &str, cwd: Option<&str>) -> std::io::Result<PathBuf> {
    if let Some(cwd) = cwd {
        return Ok(PathBuf::from(cwd.replace(PROJECT_CWD, &project.to_string_lossy())));
    }
    // One folder per name: every byte but [A-Za-z0-9_-] is written %XX, so no name is `.`, `..`
    // or another server's.
    let mut dir = String::new();
    for b in name.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' => dir.push(b as char),
            _ => dir.push_str(&format!("%{b:02X}")),
        }
    }
    let dir = home
        .join("tmp")
        .join("mcp")
        .join(if dir.is_empty() { "%" } else { &dir });
    create_private_dir(&dir)?;
    Ok(dir)
}

fn expand_all(o: &IndexMap<String, String>) -> Vec<(String, String)> {
    o.iter().map(|(k, v)| (k.clone(), expand(v))).collect()
}

/// MCP tool names become mcp__<server>__<tool>, limited to what every provider accepts.
pub fn tool_name(server: &str, tool: &str) -> String {
    // Per UTF-16 unit, like the JS regex: a character outside the BMP becomes two "_".
    let mut out = String::new();
    for c in format!("mcp__{server}__{tool}").chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else {
            out.push_str(&"_".repeat(c.len_utf16()));
        }
    }
    out.chars().take(64).collect()
}

fn en(api: &PluginApi) -> bool {
    api.ui().lang() == Lang::En
}

/// What every connection shares: the plugin API, the states and their configs.
struct Shared {
    api: PluginApi,
    states: ServerStates,
    configs: IndexMap<String, Arc<McpServerConfig>>,
    on_change: Option<OnChange>,
    exiting: AtomicBool,
    /// `plugins.settings.mcp.reconnectDelayMs` shortens the gaps (the count and the ceiling stay).
    gap: f64,
}

impl Shared {
    fn state(&self, name: &str) -> StateRef {
        // INFALLIBLE: names come from `configs`, which has the same keys as `states`.
        self.states.servers.get(name).cloned().expect("server state")
    }
    fn changed(&self) {
        if let Some(f) = &self.on_change {
            f(&self.states);
        }
    }
}

/// Connect servers in the background and register their tools as they come up. Closes them when
/// sasacode exits (session_end other than a session switch). Other plugins (e.g. browsr) reuse this
/// to wrap a specific MCP server. A server that drops on its own is reconnected, a few times.
/// Call it inside the tokio runtime (plugins load there).
pub fn connect_servers(api: &PluginApi, servers: McpServers, on_change: Option<OnChange>) -> ServerStates {
    start_servers(api, servers, on_change).states.clone()
}

fn start_servers(api: &PluginApi, servers: McpServers, on_change: Option<OnChange>) -> Arc<Shared> {
    let mut states = IndexMap::new();
    let mut configs = IndexMap::new();
    for (name, cfg) in servers {
        if cfg.disabled {
            continue;
        }
        let state = ServerState {
            name: name.clone(),
            status: Status::Connecting,
            error: None,
            tools: Vec::new(),
            client: None,
            stderr: String::new(),
            registered: Vec::new(),
            attempts: 0,
            timer: None,
        };
        states.insert(name.clone(), Arc::new(Mutex::new(state)));
        configs.insert(name, Arc::new(cfg));
    }
    let gap = api
        .settings()
        .get("reconnectDelayMs")
        .and_then(Value::as_f64)
        .unwrap_or(RECONNECT_DELAY_MS);
    let states = ServerStates {
        servers: Arc::new(states),
    };
    let sh = Arc::new(Shared {
        api: api.clone(),
        states: states.clone(),
        configs,
        on_change,
        exiting: AtomicBool::new(false),
        gap,
    });
    for name in sh.configs.keys() {
        // Started now, like the TS promise; headless waits for the first attempt through ready().
        let h = tokio::spawn(open(sh.clone(), name.clone()));
        api.ready(async move {
            let _ = h.await;
        });
    }
    sh.changed();
    // Servers outlive /clear, /resume and /fork: nothing would connect them again for the next session.
    let sh_end = sh.clone();
    api.on::<hook::SessionEnd, _, _>(move |SessionEndEvent { reason, .. }, _| {
        let sh = sh_end.clone();
        async move {
            if reason == Some(SessionEndReason::Switch) {
                return Ok(None);
            }
            sh.exiting.store(true, Ordering::SeqCst);
            let closing: Vec<_> = sh
                .states
                .servers
                .values()
                .map(|s| {
                    let mut st = lock(s);
                    if let Some(t) = st.timer.take() {
                        t.abort();
                    }
                    st.status = Status::Closed; // expected shutdown, not a crash
                    st.client.clone()
                })
                .collect();
            futures::future::join_all(closing.into_iter().flatten().map(|c| async move { c.close().await })).await;
            Ok(None)
        }
    });
    sh
}

fn open(sh: Arc<Shared>, name: String) -> BoxFuture<'static, ()> {
    Box::pin(connect(sh, name))
}

/// The connection was lost or never came up: schedule a reconnect. The gap grows with every
/// attempt; after the last one the server stays down until the user asks (`/mcp reconnect`), so a
/// command that is gone cannot spin retries forever.
fn dropped(sh: &Arc<Shared>, name: &str) {
    if sh.exiting.load(Ordering::SeqCst) {
        return;
    }
    let state = sh.state(name);
    let mut st = lock(&state);
    if st.attempts >= MAX_RECONNECTS {
        drop(st);
        let api = &sh.api;
        let msg = if en(api) {
            format!("MCP server {name}: gave up reconnecting; `/mcp reconnect {name}` tries again")
        } else {
            format!("MCP サーバー {name}: 再接続をあきらめました。`/mcp reconnect {name}` でもう一度試せます")
        };
        api.ui().notify(&msg, NotifyLevel::Warning);
        return;
    }
    let delay = (sh.gap * 2f64.powi(st.attempts as i32)).clamp(0.0, MAX_RECONNECT_DELAY_MS);
    st.attempts += 1;
    // A pending reconnect is background work: it never holds the process open.
    let (sh2, name2) = (sh.clone(), name.to_string());
    let task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(delay as u64)).await;
        open(sh2, name2).await;
    });
    st.timer = Some(task.abort_handle());
}

/// `/mcp reconnect`: start over with a fresh count, replacing the client.
fn reconnect(sh: &Arc<Shared>, name: &str) {
    let state = sh.state(name);
    let client = {
        let mut st = lock(&state);
        if let Some(t) = st.timer.take() {
            t.abort();
        }
        st.attempts = 0;
        st.error = None;
        st.status = Status::Closed; // replacing the client is expected: no crash, no retry
        st.client.clone()
    };
    if let Some(c) = client {
        tokio::spawn(async move { c.close().await });
    }
    tokio::spawn(open(sh.clone(), name.to_string()));
}

/// The MCP adapter is an ordinary plugin: servers connect in the background and register tools as they come up.
pub use sasacode_plugin_api::PluginInit;

/// `createMcpPlugin(servers)`.
pub fn create_mcp_plugin(servers: McpServers) -> PluginInit {
    Box::new(move |api| Box::pin(mcp_plugin(api, servers)))
}

/// The body of [`create_mcp_plugin`]: connect the servers, keep `MCP <up>/<all>` in the status
/// line, and register `/mcp [reconnect [name]]`.
pub async fn mcp_plugin(api: PluginApi, servers: McpServers) -> anyhow::Result<()> {
    let status_api = api.clone();
    let on_change: OnChange = Arc::new(move |all: &ServerStates| {
        if all.is_empty() {
            return;
        }
        let ok = all.snapshot().iter().filter(|s| s.status == Status::Connected).count();
        status_api
            .ui()
            .set_status("servers", Some(format!("MCP {ok}/{}", all.len())));
    });
    let sh = start_servers(&api, servers, Some(on_change));
    let states = sh.states.clone();

    let complete_states = states.clone();
    let run_api = api.clone();
    let description = if en(&api) {
        "MCP servers: status and reconnecting"
    } else {
        "MCP サーバーの接続状態と再接続"
    };
    let command = Command::new("mcp", description, move |CommandContext { args }| {
        let (api, states, sh) = (run_api.clone(), states.clone(), sh.clone());
        async move {
            run_mcp_command(&api, &states, &sh, &args);
            Ok(())
        }
    })
    .hint("[reconnect [name]]")
    .complete(move |prefix: String| {
        let states = complete_states.clone();
        async move {
            std::iter::once("reconnect".to_string())
                .chain(states.names())
                .filter(|w| w.starts_with(&prefix))
                .map(|w| SelectOption::new(w.clone(), w))
                .collect()
        }
    });
    api.register_command(command);
    Ok(())
}

fn js_split_whitespace(s: &str) -> Vec<&str> {
    s.split(|c: char| is_js_space(c)).filter(|w| !w.is_empty()).collect()
}

fn run_mcp_command(api: &PluginApi, states: &ServerStates, sh: &Arc<Shared>, args: &str) {
    let en = en(api);
    if states.is_empty() {
        api.ui().notify(
            if en {
                "No MCP servers are configured (mcpServers in config.json)"
            } else {
                "MCP サーバーは設定されていません（config.json の mcpServers）"
            },
            NotifyLevel::Info,
        );
        return;
    }
    let words = js_split_whitespace(args);
    let (sub, target) = (words.first().copied().unwrap_or(""), words.get(1).copied());
    if sub == "reconnect" {
        let picked = target.and_then(|t| states.get(t));
        if let (Some(target), None) = (target, &picked) {
            let known = states.names().join(", ");
            let msg = if en {
                format!("No MCP server named {target} ({known})")
            } else {
                format!("その名前の MCP サーバーはありません: {target}（{known}）")
            };
            api.ui().notify(&msg, NotifyLevel::Warning);
            return;
        }
        let now: Vec<String> = match picked {
            Some(p) => vec![p.name],
            None => states.names(),
        };
        for name in &now {
            reconnect(sh, name);
        }
        let names = now.join(", ");
        api.ui().notify(
            &if en {
                format!("Reconnecting: {names}")
            } else {
                format!("再接続します: {names}")
            },
            NotifyLevel::Info,
        );
        return;
    }
    if !sub.is_empty() {
        api.ui().notify(
            if en {
                "usage: /mcp [reconnect [name]]"
            } else {
                "使い方: /mcp [reconnect [name]]"
            },
            NotifyLevel::Warning,
        );
        return;
    }
    let lines: Vec<String> = states
        .snapshot()
        .iter()
        .map(|s| {
            let dot = if s.status == Status::Connected { "●" } else { "○" };
            let error = s
                .error
                .as_deref()
                .filter(|e| !e.is_empty())
                .map(|e| format!(" ({e})"))
                .unwrap_or_default();
            let tools = if s.tools.is_empty() {
                String::new()
            } else {
                format!(" · {} tools", s.tools.len())
            };
            format!("{dot} {}: {}{error}{tools}", s.name, s.status.as_str())
        })
        .collect();
    api.ui().notify(&lines.join("\n"), NotifyLevel::Info);
}

/// JS `\s`.
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// `[\x00-\x08\x0b-\x1f\x7f-\x9f]` removed: stderr is shown in notices, and the server's escape
/// sequences must not reach the terminal.
fn strip_controls(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(*c, '\u{0}'..='\u{8}' | '\u{B}'..='\u{1F}' | '\u{7F}'..='\u{9F}'))
        .collect()
}

/// The last `n` UTF-16 units of `s` (`s.slice(-n)`), never splitting a character.
fn last_utf16(s: &str, n: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices().rev() {
        units += c.len_utf16();
        if units > n {
            return &s[i + c.len_utf8()..];
        }
    }
    s
}

/// The first `n` UTF-16 units of `s` (`s.slice(0, n)`), never splitting a character.
fn first_utf16(s: &str, n: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > n {
            return &s[..i];
        }
    }
    s
}

/// The message of an error the way the TS showed it (`e.message`).
fn message(e: &anyhow::Error) -> String {
    e.to_string()
}

/// One connection attempt for `name`. On failure the caller's reconnect schedule takes over.
async fn connect(sh: Arc<Shared>, name: String) {
    let api = sh.api.clone();
    let state = sh.state(&name);
    // INFALLIBLE: `name` is a key of `configs` (see Shared::state).
    let cfg = sh.configs.get(&name).cloned().expect("server config");
    let client = Client::new("sasacode", api.version());
    lock(&state).status = Status::Connecting;
    let result: anyhow::Result<()> = async {
        let transport: Arc<dyn Transport> = match &cfg.transport {
            McpTransportConfig::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                let mut child_env = stdio::default_environment();
                child_env.extend(expand_all(env));
                let cwd = server_cwd(&sasacode_home(), &api.cwd(), &name, cwd.as_deref())
                    .map_err(|e| anyhow::anyhow!("cannot prepare the working directory of {name}: {e}"))?;
                let params = StdioParams {
                    command: command.clone(),
                    args: args.clone(),
                    env: child_env,
                    cwd,
                };
                let st = state.clone();
                // Shown in notices: the server's escape sequences must not reach the terminal.
                let on_stderr = Arc::new(move |d: &str| {
                    let mut s = lock(&st);
                    let joined = format!("{}{}", s.stderr, strip_controls(d));
                    s.stderr = last_utf16(&joined, 2000).to_string();
                });
                Arc::new(StdioTransport::new(params, on_stderr))
            }
            McpTransportConfig::Http { url, headers } => {
                let url = expand(url.as_str());
                let parsed =
                    reqwest::Url::parse(&url).map_err(|_| anyhow::anyhow!("\"{url}\" cannot be parsed as a URL."))?;
                Arc::new(HttpTransport::new(parsed, &expand_all(headers))?)
            }
        };
        let (st, sh2, me) = (state.clone(), sh.clone(), client.downgrade());
        let server = name.clone();
        client.set_on_close(move || {
            {
                let mut s = lock(&st);
                // A connection that was replaced (/mcp reconnect, a reconnect after a failure) may take its
                // time to close: its end says nothing about the client of the moment (#101).
                if !s.client.as_ref().is_some_and(|c| me.is(c)) {
                    return;
                }
                // A close we asked for (exit, /mcp reconnect) has already moved the state on.
                if s.status != Status::Connected {
                    return;
                }
                s.status = Status::Closed;
                s.error = Some("server exited".into());
            }
            let api = &sh2.api;
            let msg = if en(api) {
                format!("MCP server {server} disconnected")
            } else {
                format!("MCP サーバー {server} の接続が切れました")
            };
            api.ui().notify(&msg, NotifyLevel::Warning);
            sh2.changed();
            dropped(&sh2, &server);
        });
        client.connect(transport).await?;
        {
            let mut s = lock(&state);
            s.client = Some(client.clone());
            s.status = Status::Connected;
        }
        register_tools(&api, &name, &cfg, &client, &state).await?;
        let caps = client.server_capabilities().unwrap_or(Value::Null);
        if truthy(caps.get("tools").and_then(|t| t.get("listChanged"))) {
            let (api2, name2, cfg2, client2, state2) = (
                api.clone(),
                name.clone(),
                cfg.clone(),
                client.downgrade(),
                state.clone(),
            );
            client.set_tool_list_changed_handler(move || {
                // The list changed in some way: sync it, so a tool the server dropped leaves the model's list.
                let Some(client) = client2.upgrade() else { return };
                let (api, name, cfg, state) = (api2.clone(), name2.clone(), cfg2.clone(), state2.clone());
                tokio::spawn(async move {
                    if let Err(e) = register_tools(&api, &name, &cfg, &client, &state).await {
                        let msg = if en(&api) {
                            format!("MCP server {name}: tool list update failed: {}", message(&e))
                        } else {
                            format!("MCP サーバー {name}: ツール一覧を更新できませんでした: {}", message(&e))
                        };
                        api.ui().notify(&msg, NotifyLevel::Warning);
                    }
                });
            });
        }
        if truthy(caps.get("prompts")) {
            register_prompts(&api, &name, &client).await;
        }
        // Counted as up only now: a server that connects and then fails its tools/list must still run
        // out of reconnects (#101).
        lock(&state).attempts = 0;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        let first = {
            let mut s = lock(&state);
            s.status = Status::Failed;
            let last_line = s
                .stderr
                .trim_matches(is_js_space)
                .split('\n')
                .next_back()
                .unwrap_or_default()
                .to_string();
            let tail = if s.stderr.is_empty() {
                String::new()
            } else {
                format!(" — {last_line}")
            };
            s.error = Some(format!("{}{tail}", message(&e)));
            s.attempts == 0
        };
        // Its process would otherwise keep running beside the next attempt's (#101).
        let c = client.clone();
        tokio::spawn(async move { c.close().await });
        // Repeats are the reconnect loop's own business; one notice is enough to explain a dead server.
        if first {
            let error = lock(&state).error.clone().unwrap_or_default();
            let msg = if en(&api) {
                format!("MCP server {name} failed to start: {error}")
            } else {
                format!("MCP サーバー {name} を起動できませんでした: {error}")
            };
            api.ui().notify(&msg, NotifyLevel::Warning);
        }
        dropped(&sh, &name);
    }
    sh.changed();
}

/// A list call followed through its pages, stopping a server that keeps sending a next cursor (#49).
async fn all_pages<F, Fut>(key: &str, mut list: F) -> anyhow::Result<Vec<Value>>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Value>>,
{
    let mut items = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut cursor: Option<String> = None;
    loop {
        if seen.len() >= 100 {
            anyhow::bail!("the list did not end after 100 pages");
        }
        let page = list(cursor.take()).await?;
        items.extend(page.get(key).and_then(Value::as_array).cloned().unwrap_or_default());
        let next = page.get("nextCursor").and_then(Value::as_str).map(str::to_string);
        if let Some(n) = next.as_ref().filter(|n| seen.contains(*n)) {
            anyhow::bail!("the list repeats its page cursor {}", Value::String(n.clone()));
        }
        // `while (cursor)`: an empty cursor ends the list too.
        match next.filter(|n| !n.is_empty()) {
            Some(n) => {
                seen.insert(n.clone());
                cursor = Some(n);
            }
            None => return Ok(items),
        }
    }
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Brings the model's tool list in line with what the server offers right now (also on list changes).
async fn register_tools(
    api: &PluginApi,
    server: &str,
    cfg: &Arc<McpServerConfig>,
    client: &Client,
    state: &StateRef,
) -> anyhow::Result<()> {
    // A server without tools (prompts or resources only) answers tools/list with "Method not found" (#101).
    let has_tools = truthy(client.server_capabilities().as_ref().and_then(|c| c.get("tools")));
    let tools = if has_tools {
        all_pages("tools", |cursor| {
            let client = client.clone();
            async move { client.list_tools(cursor.as_deref()).await }
        })
        .await?
    } else {
        Vec::new()
    };
    let plain = cfg.tool_names == ToolNames::Plain;
    let tool_names: Vec<String> = tools
        .iter()
        .map(|t| str_field(t, "name").unwrap_or_default().to_string())
        .collect();
    // A tool the server no longer offers must come off the model's list: it would only keep failing.
    let names: Vec<String> = tool_names
        .iter()
        .map(|t| if plain { t.clone() } else { tool_name(server, t) })
        .collect();
    let gone: Vec<String> = {
        let mut s = lock(state);
        s.tools = tool_names.clone();
        let gone = s.registered.iter().filter(|g| !names.contains(g)).cloned().collect();
        s.registered = names.clone();
        gone
    };
    for g in gone {
        api.unregister_tool(&g);
    }
    let timeout = Duration::from_millis(cfg.timeout.unwrap_or(DEFAULT_CALL_TIMEOUT_MS).max(0.0) as u64);
    for (t, (tname, registered_name)) in tools.iter().zip(tool_names.into_iter().zip(names)) {
        let description = str_field(t, "description")
            .or_else(|| str_field(t, "title"))
            .unwrap_or(&tname)
            .to_string();
        let read_only = t.get("annotations").and_then(|a| a.get("readOnlyHint")) == Some(&Value::Bool(true));
        let (state, server, call_name) = (state.clone(), server.to_string(), tname.clone());
        let def = ToolDef::new(
            &registered_name,
            &description,
            t.get("inputSchema").cloned().unwrap_or(Value::Null),
            // Server annotations are hints from outside our trust boundary, so MCP tools always go through approval.
            ToolKind::Other,
            move |args, ctx| {
                let (state, server, call_name) = (state.clone(), server.clone(), call_name.clone());
                async move {
                    // The client of the moment: after a reconnect, a tool taken before it was registered again still works.
                    let (current, status, error) = {
                        let s = lock(&state);
                        (s.client.clone(), s.status, s.error.clone())
                    };
                    let Some(current) = current.filter(|_| status == Status::Connected) else {
                        let why = error.unwrap_or_else(|| status.as_str().to_string());
                        return Ok(error_result(format!("MCP server {server} is not connected ({why}).")));
                    };
                    let r = current
                        .call_tool(&call_name, Value::Object(args), timeout, Some(ctx.signal.clone()))
                        .await;
                    Ok(match r.and_then(|r| convert_result(&r, &server, &call_name)) {
                        Ok(result) => result,
                        Err(e) => error_result(format!("MCP {server}/{call_name} failed: {}", message(&e))),
                    })
                }
            },
        )
        .concurrent(read_only)
        .always_load(cfg.always_load);
        api.register_tool(def.into_ref());
    }
    Ok(())
}

/// `args.match(/(?:[^\s"]+|"[^"]*")+/g)`: words, where a quoted part may hold spaces.
fn prompt_words(args: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let start = i;
        let mut j = i;
        loop {
            let rest = &args[j..];
            match rest.chars().next() {
                Some('"') => match rest[1..].find('"') {
                    Some(close) => j += close + 2,
                    None => break,
                },
                Some(c) if !is_js_space(c) => j += c.len_utf8(),
                _ => break,
            }
        }
        if j > start {
            out.push(&args[start..j]);
            i = j;
        } else {
            i += args[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

/// `w.replace(/^"|"$/g, "")`.
fn strip_quotes(w: &str) -> &str {
    let w = w.strip_prefix('"').unwrap_or(w);
    w.strip_suffix('"').unwrap_or(w)
}

/// "key=value" pairs, or plain words filling the arguments in order. Only the plain words count for
/// the order: "greeting=hi Alice" gives who=Alice.
fn prompt_arguments(args: &str, arg_names: &[String]) -> Map<String, Value> {
    let mut values = Map::new();
    let mut next = 0;
    for w in prompt_words(args) {
        // `/^(\w+)=(.*)$/s` with ASCII \w.
        let key_len = w
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        if key_len > 0 && w[key_len..].starts_with('=') {
            values.insert(w[..key_len].to_string(), strip_quotes(&w[key_len + 1..]).into());
        } else if let Some(name) = arg_names.get(next).filter(|n| !n.is_empty()) {
            values.insert(name.clone(), strip_quotes(w).into());
            next += 1;
        }
    }
    values
}

async fn register_prompts(api: &PluginApi, server: &str, client: &Client) {
    let prompts = all_pages("prompts", |cursor| {
        let client = client.clone();
        async move { client.list_prompts(cursor.as_deref()).await }
    })
    .await;
    let prompts = match prompts {
        Ok(p) => p,
        Err(e) => {
            // The tools work without them: give up on the prompts only.
            let why = message(&e);
            let msg = if en(api) {
                format!("MCP server {server}: prompts not loaded: {why}")
            } else {
                format!("MCP サーバー {server}: プロンプトを読み込めませんでした: {why}")
            };
            api.ui().notify(&msg, NotifyLevel::Warning);
            return;
        }
    };
    for p in prompts {
        let pname = str_field(&p, "name").unwrap_or_default().to_string();
        let arg_names: Vec<String> = p
            .get("arguments")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|a| str_field(a, "name").unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        let description = str_field(&p, "description")
            .map(str::to_string)
            .unwrap_or_else(|| format!("MCP prompt from {server}"));
        let (run_api, client, run_name, names) = (api.clone(), client.clone(), pname.clone(), arg_names.clone());
        let command = Command::new(
            &tool_name(server, &pname),
            &description,
            move |CommandContext { args }| {
                let (api, client, name) = (run_api.clone(), client.clone(), run_name.clone());
                let values = prompt_arguments(&args, &names);
                async move {
                    let r = client.get_prompt(&name, values).await?;
                    let texts: Vec<String> = r
                        .get("messages")
                        .and_then(Value::as_array)
                        .map(|ms| {
                            ms.iter()
                                .filter_map(|m| {
                                    let c = m.get("content")?;
                                    match str_field(c, "type") {
                                        Some("text") => str_field(c, "text").map(str::to_string),
                                        Some("resource") => {
                                            c.get("resource").and_then(|r| str_field(r, "text")).map(str::to_string)
                                        }
                                        _ => None,
                                    }
                                })
                                .filter(|t| !t.is_empty())
                                .collect()
                        })
                        .unwrap_or_default();
                    api.session().inject_text(&texts.join("\n\n"), InjectWhen::Now);
                    Ok(())
                }
            },
        )
        .hint(&arg_names.join(" "));
        api.register_command(command);
    }
}

/// A tools/call result as sasacode content: text and images as they are, resources as their text
/// (or a placeholder), other kinds as a placeholder. Text past 100 000 characters (UTF-16 units)
/// is cut, and the whole output is saved to a file the model is told about (A4).
pub fn convert_result(r: &Value, server: &str, tool: &str) -> anyhow::Result<ToolResult> {
    let mut content: Vec<UserContent> = Vec::new();
    let empty = Vec::new();
    for c in r.get("content").and_then(Value::as_array).unwrap_or(&empty) {
        let kind = str_field(c, "type").unwrap_or_default();
        match kind {
            "text" => content.push(UserContent::text(str_field(c, "text").unwrap_or_default())),
            "image" => content.push(UserContent::Image(ImageContent {
                media_type: str_field(c, "mimeType").unwrap_or_default().to_string(),
                data: str_field(c, "data").unwrap_or_default().into(),
            })),
            "resource" => {
                let res = c.get("resource").unwrap_or(&Value::Null);
                let text = match str_field(res, "text") {
                    Some(t) => t.to_string(),
                    None => format!(
                        "[resource {} ({})]",
                        str_field(res, "uri").unwrap_or_default(),
                        str_field(res, "mimeType").unwrap_or("binary")
                    ),
                };
                content.push(UserContent::text(text));
            }
            "resource_link" => content.push(UserContent::text(format!(
                "[resource link {} {}]",
                str_field(c, "name").unwrap_or_default(),
                str_field(c, "uri").unwrap_or_default()
            ))),
            _ => content.push(UserContent::text(format!("[{kind} content omitted]"))),
        }
    }
    if content.is_empty()
        && let Some(s) = r.get("structuredContent")
    {
        content.push(UserContent::text(sasacode_ai::json::stringify(s)));
    }
    if content.is_empty()
        && let Some(s) = r.get("toolResult")
    {
        content.push(UserContent::text(sasacode_ai::json::stringify(s)));
    }
    let is_error = r.get("isError").and_then(Value::as_bool);
    // A4: cut only at an explicit limit, and say where the rest is.
    let total: usize = content
        .iter()
        .filter_map(UserContent::as_text)
        .map(|t| t.encode_utf16().count())
        .sum();
    if total > MAX_OUTPUT {
        let full: Vec<&str> = content.iter().map(|c| c.as_text().unwrap_or_default()).collect();
        let full = full.join("\n");
        let prefix: String = format!("mcp-{server}-{tool}")
            .chars()
            .flat_map(|c| {
                let keep = c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-');
                let n = if keep { 1 } else { c.len_utf16() };
                std::iter::repeat_n(if keep { c } else { '_' }, n)
            })
            .collect();
        let file = save_output(&prefix, &full)?;
        let mut out = vec![UserContent::text(format!(
            "{}\n[Output truncated at {MAX_OUTPUT} of {total} characters. Full output saved to {}]",
            first_utf16(&full, MAX_OUTPUT),
            file.display()
        ))];
        out.extend(content.into_iter().filter(|c| matches!(c, UserContent::Image(_))));
        return Ok(ToolResult {
            content: out,
            is_error,
            details: None,
        });
    }
    if content.is_empty() {
        content.push(UserContent::text("(no content)"));
    }
    Ok(ToolResult {
        content,
        is_error,
        details: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // mcp.test.ts: "names and results are normalized"
    #[test]
    fn names_and_results_are_normalized() {
        assert_eq!(tool_name("my server", "do.thing"), "mcp__my_server__do_thing");
        let r = convert_result(
            &json!({"content": [{"type": "resource", "resource": {"uri": "file:///a", "text": "hello"}}]}),
            "s",
            "t",
        )
        .unwrap();
        assert_eq!(r.content, vec![UserContent::text("hello")]);
        let r = convert_result(&json!({"content": [], "structuredContent": {"ok": 1}}), "s", "t").unwrap();
        assert_eq!(r.content, vec![UserContent::text("{\"ok\":1}")]);
    }

    #[test]
    fn names_count_utf16_units_and_stop_at_64() {
        assert_eq!(tool_name("s", "😀"), "mcp__s____");
        assert_eq!(tool_name("server", &"x".repeat(80)).len(), 64);
    }

    #[test]
    fn result_kinds_and_is_error() {
        let r = convert_result(
            &json!({"content": [
                {"type": "image", "data": "AAA", "mimeType": "image/png"},
                {"type": "audio", "data": "AAA", "mimeType": "audio/wav"},
                {"type": "resource", "resource": {"uri": "file:///b", "blob": "AA"}},
                {"type": "resource_link", "uri": "file:///c"}
            ], "isError": true}),
            "s",
            "t",
        )
        .unwrap();
        assert_eq!(
            r.content,
            vec![
                UserContent::Image(ImageContent {
                    media_type: "image/png".into(),
                    data: "AAA".into()
                }),
                UserContent::text("[audio content omitted]"),
                UserContent::text("[resource file:///b (binary)]"),
                UserContent::text("[resource link  file:///c]"),
            ]
        );
        assert_eq!(r.is_error, Some(true));
        let r = convert_result(&json!({}), "s", "t").unwrap();
        assert_eq!((r.content, r.is_error), (vec![UserContent::text("(no content)")], None));
        let r = convert_result(&json!({"toolResult": [1, 2]}), "s", "t").unwrap();
        assert_eq!(r.content, vec![UserContent::text("[1,2]")]);
    }

    #[test]
    fn env_references_expand_and_others_stay() {
        assert_eq!(expand("a ${NO_SUCH_VAR_FOR_SASACODE_TEST} b"), "a  b");
        assert_eq!(expand("$HOME ${1X} ${} ${A-B}"), "$HOME ${1X} ${} ${A-B}");
        let path = std::env::var("PATH").unwrap_or_default();
        assert_eq!(expand("p=${PATH}!"), format!("p={path}!"));
    }

    #[test]
    fn prompt_words_and_arguments() {
        assert_eq!(prompt_words("a  \"b c\"d e\"f"), vec!["a", "\"b c\"d", "e", "f"]);
        let names = vec!["who".to_string(), "greeting".to_string()];
        let v = prompt_arguments("greeting=hi Alice", &names);
        assert_eq!(serde_json::to_string(&v).unwrap(), r#"{"greeting":"hi","who":"Alice"}"#);
        let v = prompt_arguments("\"Bob Smith\" greeting=\"good day\"", &names);
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"{"who":"Bob Smith","greeting":"good day"}"#
        );
        // \w is ASCII: a Japanese key is a plain word.
        let v = prompt_arguments("名前=x", &names);
        assert_eq!(serde_json::to_string(&v).unwrap(), r#"{"who":"名前=x"}"#);
    }

    #[test]
    fn stderr_is_cleaned_and_kept_short() {
        assert_eq!(strip_controls("a\u{1b}[31mb\tc\r\n\u{85}"), "a[31mb\tc\n");
        assert_eq!(last_utf16("ab😀", 2), "😀");
        assert_eq!(last_utf16("ab😀", 3), "b😀");
        assert_eq!(first_utf16("a😀b", 2), "a");
    }
}

#[cfg(test)]
mod cwd_tests {
    use super::*;

    #[test]
    fn a_server_without_a_cwd_starts_in_a_folder_of_its_own_outside_the_project() {
        let (home, project) = (tempfile::tempdir().unwrap(), Path::new("/p"));
        let dir = server_cwd(home.path(), project, "git hub/é", None).unwrap();
        assert_eq!(dir, home.path().join("tmp/mcp/git%20hub%2F%C3%A9"));
        assert!(dir.is_dir());
        let (dot, dotdot, empty) = (
            server_cwd(home.path(), project, ".", None).unwrap(),
            server_cwd(home.path(), project, "..", None).unwrap(),
            server_cwd(home.path(), project, "", None).unwrap(),
        );
        assert_eq!(dot, home.path().join("tmp/mcp/%2E"));
        assert_eq!(dotdot, home.path().join("tmp/mcp/%2E%2E"));
        assert_eq!(empty, home.path().join("tmp/mcp/%"));
        assert_eq!(
            server_cwd(home.path(), project, "a_b-C9", None).unwrap(),
            home.path().join("tmp/mcp/a_b-C9")
        );
    }

    #[test]
    fn a_cwd_is_kept_and_only_the_project_name_is_replaced() {
        let (home, project) = (Path::new("/nonexistent-home"), Path::new("/p/q"));
        let cwd = |c: &str| server_cwd(home, project, "s", Some(c)).unwrap();
        assert_eq!(cwd(PROJECT_CWD), PathBuf::from("/p/q"));
        assert_eq!(cwd("${SASACODE_PROJECT}/sub"), PathBuf::from("/p/q/sub"));
        assert_eq!(cwd("/abs/dir"), PathBuf::from("/abs/dir"));
        // Relative as before (the process resolves it); another variable is not expanded.
        assert_eq!(cwd("rel/dir"), PathBuf::from("rel/dir"));
        assert_eq!(cwd("${HOME}/x"), PathBuf::from("${HOME}/x"));
    }
}
