//! packages/bundled/test/auto-reconnect.test.ts: auto-reconnect in a real agent whose provider fails
//! with the given errors in turn (registered under an api name of its own per test).

mod support_net;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use futures::stream::{self, BoxStream, StreamExt};
use serde_json::{Value, json};

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::plugins::PluginHost;
use sasacode_ai::{
    AssistantContent, AssistantMessage, Message, Provider, Request, StopReason, StreamEvent, Usage, register_api,
};
use sasacode_plugin_api::{CommandContext, Lang, PermissionMode, StopCause};
use support_net::*;

/// A provider that fails with each of `errors` in turn, then answers "ok". Keeps the content of
/// the last message of each request (when it is the user's).
struct Flaky {
    api: String,
    errors: Mutex<Vec<String>>,
    calls: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for Flaky {
    fn api(&self) -> &str {
        &self.api
    }
    fn stream(&self, req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        let last = match req.messages.last() {
            Some(Message::User(u)) => serde_json::to_string(&u.content).unwrap_or_default(),
            _ => String::new(),
        };
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).push(last);
        let error = {
            let mut errors = self.errors.lock().unwrap_or_else(|e| e.into_inner());
            (!errors.is_empty()).then(|| errors.remove(0))
        };
        if let Some(error) = error {
            return stream::once(async move { Err(anyhow::anyhow!(error)) }).boxed();
        }
        let message = AssistantMessage {
            content: vec![AssistantContent::text("ok")],
            api: self.api.clone(),
            provider: "t".into(),
            model: "m".into(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            timestamp: 0,
            replay_scope: None,
            error_message: None,
        };
        stream::once(async move { Ok(StreamEvent::Done { message }) }).boxed()
    }
}

struct Started {
    agent: Agent,
    host: Arc<PluginHost>,
    calls: Arc<Mutex<Vec<String>>>,
    status: Arc<Mutex<Vec<String>>>,
    _dir: tempfile::TempDir,
    _server: Server,
}

impl Started {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn status(&self) -> Vec<String> {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    async fn prompt(&self, text: &str) -> StopCause {
        self.agent
            .prompt_text(text)
            .await
            .unwrap_or_else(|e| panic!("prompt: {e:#}"))
    }
    async fn settle(&self, ms: u64) {
        sleep(ms).await;
        self.agent.wait_for_idle().await;
    }
}

/// The endpoint answers, so the network is up: only the model call fails.
async fn start(errors: &[&str], settings: Value) -> Started {
    let server = serve(Router::new().fallback(|| async { "ok" })).await;
    let api = api_name();
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let flaky = Flaky {
        api: api.clone(),
        errors: Mutex::new(errors.iter().map(|e| e.to_string()).collect()),
        calls: calls.clone(),
    };
    register_api(&api, Arc::new(flaky));
    let dir = temp_dir("sasacode-reconnect-");
    let mut o = AgentOptions::new(model(&api, json!({ "baseUrl": server.url() })), s(dir.path()), "");
    o.permissions = Some(policy(PermissionMode::Auto));
    let agent = Agent::new(o);
    let mut all = json!({ "quiet": true });
    if let (Some(all), Some(settings)) = (all.as_object_mut(), settings.as_object()) {
        all.extend(settings.clone());
    }
    let mut map = HashMap::new();
    map.insert("auto-reconnect".to_string(), all);
    let (host, _notes) = host_for(&agent, &s(dir.path()), map, Lang::Ja);
    let status: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = status.clone();
    let weak: Weak<PluginHost> = Arc::downgrade(&host);
    host.set_on_change(Some(Arc::new(move || {
        if let Some(host) = weak.upgrade() {
            seen.lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend(host.status().into_values());
        }
    })));
    assert!(host.load("auto-reconnect", bundled("auto-reconnect")).await);
    Started {
        agent,
        host,
        calls,
        status,
        _dir: dir,
        _server: server,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_error_that_outlasts_the_sdks_retries_waits_then_the_work_goes_on() {
    let _l = lang(Lang::Ja).await;
    let st = start(
        &[
            "503 Upstream model provider is temporarily unavailable.",
            "529 overloaded_error",
        ],
        json!({ "serverRetryMs": 10 }),
    )
    .await;
    assert_eq!(st.prompt("go").await, StopCause::Error);
    // Each retry comes after an online check, and the first one in a process sets up the HTTP client,
    // slow on a loaded machine: wait for the calls rather than a fixed 200 ms.
    until(150, 20, || st.calls().len() >= 3).await;
    st.settle(100).await;
    let calls = st.calls();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(calls[1].contains("サーバーの一時的なエラー"), "{}", calls[1]);
    assert!(
        st.status().first().cloned().unwrap_or_default().contains("1/4"),
        "{:?}",
        st.status()
    );
    let last = st.agent.messages().last().cloned();
    let Some(Message::Assistant(m)) = last else {
        panic!("the last message is not the reply")
    };
    assert_eq!(m.content.first().and_then(|c| c.as_text()), Some("ok"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_error_that_is_the_requests_fault_is_not_retried() {
    let _l = lang(Lang::Ja).await;
    let st = start(&["401 Invalid API key"], json!({ "serverRetryMs": 10 })).await;
    assert_eq!(st.prompt("go").await, StopCause::Error);
    st.settle(100).await;
    assert_eq!(st.calls().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_stop_after_server_retries() {
    let _l = lang(Lang::Ja).await;
    let st = start(&["503 down"; 5], json!({ "serverRetryMs": 5, "serverRetries": 2 })).await;
    st.prompt("go").await;
    until(150, 20, || st.calls().len() >= 3).await;
    for _ in 0..4 {
        st.settle(60).await;
    }
    assert_eq!(st.calls().len(), 3, "{:?}", st.calls());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_user_typing_during_the_wait_cancels_the_retry() {
    let _l = lang(Lang::Ja).await;
    let st = start(&["503 down"], json!({ "serverRetryMs": 300 })).await;
    st.prompt("go").await;
    // Type once the retry is waiting (its status is up): the online check before it can be slow.
    let waiting = || st.status().iter().any(|s| s.contains("再試行"));
    until(100, 20, waiting).await;
    assert!(waiting(), "{:?}", st.status());
    sleep(20).await;
    st.prompt("something else").await;
    st.settle(400).await;
    let calls = st.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(!calls.join(",").contains("サーバーの一時的なエラー"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_error_and_reconnect_at_once_resume_the_work_once_not_twice() {
    let _l = lang(Lang::Ja).await;
    // A port nothing listens on yet: offline until the server comes up there.
    let reserved = reserve_port();
    let port = reserved.port;
    let settings =
        json!({ "checkUrl": format!("http://127.0.0.1:{port}/"), "checkIntervalMs": 30, "checkTimeoutMs": 200 });
    let st = start(&["fetch failed"], settings).await;
    let reconnect = st
        .host
        .commands()
        .into_iter()
        .find(|c| c.command.name == "reconnect")
        .map(|c| c.command);
    let reconnect = reconnect.unwrap_or_else(|| panic!("no /reconnect"));
    let agent = st.agent.clone();
    let run = tokio::spawn(async move { agent.prompt_text("go").await.ok() });
    tokio::task::yield_now().await;
    tokio::spawn((reconnect.run)(CommandContext { args: String::new() }));
    assert_eq!(run.await.ok().flatten(), Some(StopCause::Error));
    sleep(150).await;
    // The first check has found it offline (on a loaded machine it can take longer than the 150 ms).
    until(100, 20, || st.status().iter().any(|s| s.contains("オフライン"))).await;
    let back = reserved.serve_ok();
    let resumed = || {
        st.calls()
            .iter()
            .filter(|c| c.contains("ネットワークが復旧しました"))
            .count()
    };
    until(150, 20, || resumed() > 0).await;
    st.settle(300).await;
    assert_eq!(resumed(), 1, "{:?}", st.calls());
    back.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_error_while_the_first_online_check_is_still_out_is_retried_not_dropped() {
    let _l = lang(Lang::Ja).await;
    let slow = serve(Router::new().fallback(|| async {
        tokio::time::sleep(Duration::from_millis(400)).await;
        "ok"
    }))
    .await;
    let st = start(
        &["fetch failed", "503 down"],
        json!({ "checkUrl": slow.url(), "serverRetryMs": 10 }),
    )
    .await;
    assert_eq!(st.prompt("go").await, StopCause::Error); // starts the monitor; its online check takes 400 ms
    assert_eq!(st.prompt("again").await, StopCause::Error); // 503 meanwhile
    // Two online checks of 400 ms each come before the retry: wait for it rather than a fixed 900 ms.
    until(150, 20, || st.calls().len() >= 3).await;
    st.settle(100).await;
    let calls = st.calls();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(calls[2].contains("サーバーの一時的なエラー"), "{}", calls[2]);
    slow.stop().await;
}
