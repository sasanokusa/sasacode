//! packages/mcp/test/mcp.test.ts, against the rmcp fixture (tests/fixture): stdio through the
//! examples/mcp-fixture binary, Streamable HTTP in-process.

mod fixture;
mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::{Value, json};

use sasacode_ai::UserContent;
use sasacode_mcp::{McpServerConfig, McpServers, ServerStates, Status, connect_servers, create_mcp_plugin};
use sasacode_plugin_api::{CommandContext, InjectWhen, SessionEndReason, ToolContext, ToolKind, ToolResult};
use support::{FakeHost, until};

/// The stdio fixture, built by `cargo test` as an example of this crate.
fn fixture_bin() -> PathBuf {
    let exe = std::env::current_exe().expect("test binary path");
    // The examples sit in target/<profile>/examples. The test binary is in target/<profile>/deps,
    // or, with the build-dir layout of newer cargo, deeper (target/<profile>/build/<pkg>/<hash>/out).
    let file = format!("mcp-fixture{}", std::env::consts::EXE_SUFFIX);
    exe.ancestors()
        .skip(1)
        .map(|dir| dir.join("examples").join(&file))
        .find(|bin| bin.exists())
        .unwrap_or_else(|| {
            panic!(
                "examples/{file} is missing near {}: `cargo test -p sasacode-mcp` builds it (or cargo build -p sasacode-mcp --example mcp-fixture)",
                exe.display()
            )
        })
}

fn fx(env: &[(&str, &str)]) -> McpServerConfig {
    let mut cfg = McpServerConfig::stdio(fixture_bin().to_string_lossy(), vec![]);
    if let sasacode_mcp::McpTransportConfig::Stdio { env: e, .. } = &mut cfg.transport {
        for (k, v) in env {
            e.insert(k.to_string(), v.to_string());
        }
    }
    cfg
}

fn servers(list: Vec<(&str, McpServerConfig)>) -> McpServers {
    list.into_iter().map(|(n, c)| (n.to_string(), c)).collect()
}

/// A server with no `cwd` starts in a folder under the sasacode home, which a test must not create
/// in the real one: these start where the test runs.
fn pinned(mut servers: McpServers) -> McpServers {
    for s in servers.values_mut() {
        if let sasacode_mcp::McpTransportConfig::Stdio { cwd, .. } = &mut s.transport {
            cwd.get_or_insert_with(|| ".".to_string());
        }
    }
    servers
}

async fn load(servers: McpServers, settings: Value) -> Arc<FakeHost> {
    let host = FakeHost::new(std::env::current_dir().unwrap(), json!({ "mcp": settings }));
    host.load("mcp", create_mcp_plugin(pinned(servers))).await.unwrap();
    host
}

/// The same wiring, with the server states in hand (to drop a connection and watch it return).
async fn load_raw(servers: McpServers, settings: Value) -> (Arc<FakeHost>, ServerStates) {
    let host = FakeHost::new(std::env::current_dir().unwrap(), json!({ "mcp": settings }));
    let slot: Arc<Mutex<Option<ServerStates>>> = Arc::default();
    let s = slot.clone();
    host.load("mcp", move |api| async move {
        *s.lock().unwrap() = Some(connect_servers(&api, pinned(servers), None));
        Ok(())
    })
    .await
    .unwrap();
    let states = slot.lock().unwrap().take().unwrap();
    (host, states)
}

async fn call(host: &FakeHost, tool: &str, args: Value) -> ToolResult {
    let t = host.tool(tool).unwrap_or_else(|| panic!("no tool {tool}"));
    let args = args.as_object().cloned().unwrap_or_default();
    t.execute(args, ToolContext::new(".")).await.unwrap()
}

async fn run(host: &FakeHost, command: &str, args: &str) {
    let c = host.command(command).unwrap_or_else(|| panic!("no command {command}"));
    let _ = (c.run)(CommandContext { args: args.to_string() }).await;
}

fn text(r: &ToolResult) -> String {
    r.content.iter().filter_map(UserContent::as_text).collect()
}

fn with_prefix(names: Vec<String>, prefix: &str) -> Vec<String> {
    names.into_iter().filter(|n| n.starts_with(prefix)).collect()
}

fn count(notes: &[String], what: &str) -> usize {
    notes.iter().filter(|n| n.contains(what)).count()
}

#[tokio::test]
async fn stdio_server_tools_are_namespaced_callable_and_prompts_become_commands() {
    let host = load(servers(vec![("fx", fx(&[]))]), json!({})).await;
    until(|| host.tool_names().len() == 4 && host.command_names().iter().any(|c| c.starts_with("mcp__fx__review")))
        .await;
    let add = host.tool("mcp__fx__add").unwrap();
    assert_eq!(add.kind(), ToolKind::Other);
    assert!(add.concurrent());
    assert_eq!(
        call(&host, "mcp__fx__add", json!({"a": 2, "b": 3})).await,
        ToolResult {
            content: vec![UserContent::text("5")],
            is_error: None,
            details: None
        }
    );
    assert_eq!(call(&host, "mcp__fx__fail", json!({})).await.is_error, Some(true));
    let big = text(&call(&host, "mcp__fx__big", json!({})).await);
    assert!(
        big.contains("Output truncated at 100000 of 150000"),
        "{}",
        &big[big.len() - 200..]
    );
    // The full output went to a file the message names: check it, then remove it.
    let saved = big
        .rsplit("Full output saved to ")
        .next()
        .unwrap()
        .trim_end_matches(']');
    assert_eq!(std::fs::read_to_string(saved).unwrap(), "x".repeat(150_000));
    let _ = std::fs::remove_file(saved);
    assert_eq!(host.status("mcp:servers").as_deref(), Some("MCP 1/1"));
    host.session_end(None).await;
}

#[tokio::test]
async fn prompts_every_page_is_listed_plain_words_fill_arguments_in_order() {
    let host = load(servers(vec![("fx", fx(&[]))]), json!({})).await;
    until(|| host.command_names().iter().any(|c| c == "mcp__fx__later")).await;
    assert_eq!(
        with_prefix(host.command_names(), "mcp__fx__"),
        vec!["mcp__fx__review", "mcp__fx__greet", "mcp__fx__later"]
    );
    assert_eq!(
        host.command("mcp__fx__greet").unwrap().argument_hint.as_deref(),
        Some("who greeting")
    );
    run(&host, "mcp__fx__greet", "greeting=hi Alice").await;
    until(|| !host.injected().is_empty()).await;
    let (text, when) = host.injected().remove(0);
    assert_eq!(text, r#"args {"greeting":"hi","who":"Alice"}"#);
    assert_eq!(when, InjectWhen::Now);
    host.session_end(None).await;
}

#[tokio::test]
async fn prompts_a_server_that_never_ends_its_list_loses_its_prompts_only() {
    let host = load(
        servers(vec![("fx", fx(&[("FIXTURE_ENDLESS_PROMPTS", "1")]))]),
        json!({}),
    )
    .await;
    until(|| host.notes().iter().any(|n| n.contains("again"))).await;
    let notes = host.notes().join("\n");
    assert!(
        notes.contains("fx: ") && notes.contains("repeats its page cursor \"again\""),
        "{notes}"
    );
    assert!(with_prefix(host.command_names(), "mcp__fx__").is_empty());
    assert_eq!(
        call(&host, "mcp__fx__add", json!({"a": 1, "b": 2})).await.content,
        vec![UserContent::text("3")]
    );
    host.session_end(None).await;
}

/// The TS test's Bun.serve + one transport per session: rmcp's Streamable HTTP server, with a 401
/// for a request that has no session and not the bearer token.
async fn http_fixture() -> u16 {
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
    let service = StreamableHttpService::new(
        || Ok(fixture::FixtureServer::new()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let token = format!("Bearer {}", env!("CARGO_PKG_NAME"));
    let app = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let token = token.clone();
                async move {
                    let session = req.headers().contains_key("mcp-session-id");
                    let auth = req.headers().get("authorization").and_then(|v| v.to_str().ok()) == Some(token.as_str());
                    if !session && !auth {
                        return axum::response::IntoResponse::into_response((
                            axum::http::StatusCode::UNAUTHORIZED,
                            "unauthorized",
                        ));
                    }
                    next.run(req).await
                }
            },
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await });
    port
}

/// `${VAR}` in a header comes from the environment. Tests cannot set variables (set_var is unsafe in
/// edition 2024), so the token is one cargo sets for every test run.
fn http_config(port: u16, with_token: bool) -> McpServerConfig {
    assert!(std::env::var("CARGO_PKG_NAME").is_ok(), "run through cargo test");
    let mut headers = IndexMap::new();
    if with_token {
        headers.insert("authorization".to_string(), "Bearer ${CARGO_PKG_NAME}".to_string());
    }
    McpServerConfig::http(format!("http://127.0.0.1:{port}/mcp"), headers)
}

#[tokio::test]
async fn streamable_http_server_with_headers_from_the_environment() {
    let port = http_fixture().await;
    let host = load(servers(vec![("web", http_config(port, true))]), json!({})).await;
    until(|| host.tool_names().len() == 4).await;
    assert_eq!(
        call(&host, "mcp__web__add", json!({"a": 40, "b": 2})).await.content,
        vec![UserContent::text("42")]
    );
    host.session_end(None).await;
}

// Not in the TS tests: the GET stream brings tools/list_changed over HTTP, and a 401 is a failed start.
#[tokio::test]
async fn streamable_http_list_changed_arrives_and_a_401_is_a_failed_start() {
    let port = http_fixture().await;
    let host = load(servers(vec![("web", http_config(port, true))]), json!({})).await;
    until(|| host.tool_names().len() == 4).await;
    call(&host, "mcp__web__toggle", json!({})).await;
    until(|| host.tool_names().iter().any(|t| t == "mcp__web__late")).await;
    assert!(host.tool_names().iter().any(|t| t == "mcp__web__late"));
    host.session_end(None).await;

    let host = load(servers(vec![("web", http_config(port, false))]), json!({})).await;
    until(|| !host.notes().is_empty()).await;
    assert_eq!(
        host.notes()[0],
        "MCP サーバー web を起動できませんでした: Streamable HTTP error: Error POSTing to endpoint: unauthorized"
    );
    host.session_end(None).await;
}

#[tokio::test]
async fn servers_stay_connected_across_a_session_switch_and_close_on_exit() {
    let host = load(servers(vec![("fx", fx(&[]))]), json!({})).await;
    until(|| host.tool_names().len() == 4).await;
    host.session_end(Some(SessionEndReason::Switch)).await;
    assert_eq!(
        call(&host, "mcp__fx__add", json!({"a": 1, "b": 2})).await.content,
        vec![UserContent::text("3")]
    );
    host.session_end(Some(SessionEndReason::Exit)).await;
    let r = call(&host, "mcp__fx__add", json!({"a": 1, "b": 2})).await;
    assert_eq!(r.is_error, Some(true));
    assert_eq!(text(&r), "MCP server fx is not connected (closed).");
}

#[tokio::test]
async fn a_server_that_cannot_start_is_reported_without_stopping_anything() {
    let host = load(
        servers(vec![(
            "broken",
            McpServerConfig::stdio("definitely-not-a-command-xyz", vec![]),
        )]),
        json!({}),
    )
    .await;
    until(|| !host.notes().is_empty()).await;
    assert!(
        host.notes()[0].contains("MCP サーバー broken を起動できませんでした"),
        "{:?}",
        host.notes()
    );
    assert_eq!(
        host.notes()[0],
        "MCP サーバー broken を起動できませんでした: spawn definitely-not-a-command-xyz ENOENT"
    );
    assert!(host.tool_names().is_empty());
    assert_eq!(host.status("mcp:servers").as_deref(), Some("MCP 0/1"));
    host.session_end(None).await;
}

#[tokio::test]
async fn a_tool_the_server_drops_is_taken_off_the_models_list() {
    let host = load(servers(vec![("fx", fx(&[]))]), json!({})).await;
    until(|| host.tool_names().len() == 4).await;
    call(&host, "mcp__fx__toggle", json!({})).await;
    until(|| host.tool_names().iter().any(|t| t == "mcp__fx__late")).await;
    assert!(host.tool_names().iter().any(|t| t == "mcp__fx__late"));
    call(&host, "mcp__fx__toggle", json!({})).await;
    until(|| !host.tool_names().iter().any(|t| t == "mcp__fx__late")).await;
    let mut names = with_prefix(host.tool_names(), "mcp__fx__");
    names.sort();
    assert_eq!(
        names,
        vec!["mcp__fx__add", "mcp__fx__big", "mcp__fx__fail", "mcp__fx__toggle"]
    );
    host.session_end(None).await;
}

#[tokio::test]
async fn a_server_that_drops_is_reconnected_by_itself_and_its_tools_work_again() {
    let (host, states) = load_raw(servers(vec![("fx", fx(&[]))]), json!({ "reconnectDelayMs": 150 })).await;
    let fx = || states.get("fx").unwrap();
    until(|| fx().status == Status::Connected && host.tool_names().len() == 4).await;
    // As far as sasacode can tell, the server went away.
    let client = fx().client.unwrap();
    tokio::spawn(async move { client.close().await });
    until(|| fx().status == Status::Closed).await;
    assert_eq!(fx().error.as_deref(), Some("server exited"));
    // The count goes back to 0 only once the tools are listed again (#101), a moment after "connected".
    until(|| fx().status == Status::Connected && fx().attempts == 0).await;
    assert_eq!(fx().attempts, 0); // until() gives up quietly: this is what fails if the count never comes back
    assert_eq!(fx().status, Status::Connected);
    assert!(host.notes().contains(&"MCP サーバー fx の接続が切れました".to_string()));
    assert_eq!(
        call(&host, "mcp__fx__add", json!({"a": 1, "b": 2})).await.content,
        vec![UserContent::text("3")]
    );
    host.session_end(None).await;
}

#[tokio::test]
async fn a_server_that_never_comes_up_retries_a_few_times_then_mcp_reconnect_starts_over() {
    let broken = McpServerConfig::stdio("definitely-not-a-command-xyz", vec![]);
    let host = load(servers(vec![("broken", broken)]), json!({ "reconnectDelayMs": 10 })).await;
    until(|| count(&host.notes(), "再接続をあきらめました") > 0).await;
    assert_eq!(count(&host.notes(), "起動できませんでした"), 1); // the retries stay quiet
    run(&host, "mcp", "reconnect broken").await;
    assert!(host.notes().last().unwrap().contains("再接続します: broken"));
    until(|| count(&host.notes(), "再接続をあきらめました") == 2).await; // the count starts over
    assert_eq!(count(&host.notes(), "再接続をあきらめました"), 2);
    run(&host, "mcp", "").await;
    assert!(
        host.notes().last().unwrap().contains("broken: failed"),
        "{:?}",
        host.notes().last()
    );
    host.session_end(None).await;
}

/// Starts and ends of fixture processes, from FIXTURE_LOG.
fn processes(log: &Path) -> (usize, usize) {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    (
        lines.iter().filter(|l| l.starts_with("start")).count(),
        lines.iter().filter(|l| l.starts_with("exit")).count(),
    )
}

#[tokio::test]
async fn a_server_with_prompts_only_is_not_asked_for_tools() {
    let host = load(servers(vec![("docs", fx(&[("FIXTURE_NO_TOOLS", "1")]))]), json!({})).await;
    until(|| host.command_names().iter().any(|c| c == "mcp__docs__later") || !host.notes().is_empty()).await;
    assert_eq!(host.notes(), Vec::<String>::new());
    assert_eq!(
        with_prefix(host.command_names(), "mcp__docs__"),
        vec!["mcp__docs__review", "mcp__docs__greet", "mcp__docs__later"]
    );
    assert_eq!(host.status("mcp:servers").as_deref(), Some("MCP 1/1"));
    assert!(with_prefix(host.tool_names(), "mcp__docs__").is_empty());
    host.session_end(None).await;
}

#[tokio::test]
async fn a_server_whose_tools_list_fails_runs_out_of_reconnects_and_no_process_is_left_behind() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("processes.log");
    let cfg = fx(&[("FIXTURE_TOOLS_FAIL", "1"), ("FIXTURE_LOG", &log.to_string_lossy())]);
    let host = load(servers(vec![("docs", cfg)]), json!({ "reconnectDelayMs": 10 })).await;
    until(|| count(&host.notes(), "再接続をあきらめました") > 0).await;
    assert_eq!(count(&host.notes(), "再接続をあきらめました"), 1);
    assert_eq!(count(&host.notes(), "起動できませんでした"), 1); // the retries stay quiet
    assert!(
        host.notes()[0].ends_with("MCP error -32603: tools are broken"),
        "{:?}",
        host.notes()
    );
    until(|| processes(&log).1 == 6).await;
    assert_eq!(processes(&log), (6, 6)); // the first try and 5 reconnects, each closed
    host.session_end(None).await;
}

#[tokio::test]
async fn mcp_reconnect_the_old_process_taking_its_time_is_not_taken_for_the_new_connection_dropping() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("processes.log");
    let cfg = fx(&[("FIXTURE_SLOW_EXIT", "600"), ("FIXTURE_LOG", &log.to_string_lossy())]);
    let host = load(servers(vec![("slow", cfg)]), json!({ "reconnectDelayMs": 10 })).await;
    until(|| host.status("mcp:servers").as_deref() == Some("MCP 1/1")).await;
    run(&host, "mcp", "reconnect slow").await;
    until(|| processes(&log).1 == 1).await; // the first process is gone...
    tokio::time::sleep(std::time::Duration::from_millis(300)).await; // ...and its close has reached the client
    assert_eq!(count(&host.notes(), "接続が切れました"), 0);
    let mut last = String::new();
    for _ in 0..100 {
        run(&host, "mcp", "").await;
        last = host.notes().last().cloned().unwrap_or_default();
        if last.contains("● slow: connected") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(last, "● slow: connected · 4 tools");
    assert_eq!(processes(&log).0, 2);
    host.session_end(None).await;
}

// Not in the TS tests: /mcp with no servers, a wrong name, and a wrong subcommand.
#[tokio::test]
async fn mcp_command_messages() {
    let host = load(McpServers::new(), json!({})).await;
    run(&host, "mcp", "").await;
    assert_eq!(
        host.notes(),
        vec!["MCP サーバーは設定されていません（config.json の mcpServers）".to_string()]
    );
    assert_eq!(host.status("mcp:servers"), None);
    let broken = McpServerConfig::stdio("definitely-not-a-command-xyz", vec![]);
    let host = load(servers(vec![("broken", broken)]), json!({})).await;
    run(&host, "mcp", "reconnect nope").await;
    assert_eq!(
        host.notes().last().unwrap(),
        "その名前の MCP サーバーはありません: nope（broken）"
    );
    run(&host, "mcp", "status").await;
    assert_eq!(host.notes().last().unwrap(), "使い方: /mcp [reconnect [name]]");
    let complete = host.command("mcp").unwrap().complete.unwrap();
    let values: Vec<String> = complete("re".into()).await.into_iter().map(|o| o.value).collect();
    assert_eq!(values, vec!["reconnect"]);
    host.session_end(None).await;
}
