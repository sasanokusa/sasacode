//! What the SDKs did out of sight and the port does by hand: retries before the response headers,
//! backoff and the servers' retry hints, the time limit, aborts, the error messages, the header merge,
//! and a connection that breaks mid-reply. The TS had no tests of its own for these (they were the
//! SDKs'); the expectations come from the SDK sources (rust/research/llm-sdks.md).

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use futures::StreamExt;
use sasacode_ai::transport::is_cut_reply;
use sasacode_ai::{INCOMPLETE_STREAM, ModelInfo, Request, StopReason, StreamEvent};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

fn chat_model() -> ModelInfo {
    model(
        json!({ "id": "m", "provider": "p", "api": "openai-chat", "baseUrl": "http://x/v1", "contextWindow": 1000, "maxOutput": 100 }),
    )
}

fn claude_model() -> ModelInfo {
    model(
        json!({ "id": "claude-x", "provider": "anthropic", "api": "anthropic", "baseUrl": "http://x", "contextWindow": 1000, "maxOutput": 100 }),
    )
}

fn responses_model() -> ModelInfo {
    model(
        json!({ "id": "r", "provider": "openai", "api": "openai-responses", "baseUrl": "http://x/v1", "contextWindow": 1000, "maxOutput": 100 }),
    )
}

fn req(m: ModelInfo, fetch: Arc<dyn sasacode_ai::provider::Fetch>, retries: u32) -> Request {
    Request {
        api_key: Some("k".into()),
        max_retries: Some(retries),
        ..request(m, fetch)
    }
}

/// When each request was sent, on tokio's clock (paused in these tests: sleeps pass at once, exactly).
type Times = Arc<Mutex<Vec<Instant>>>;

fn timed(
    reply: impl Fn(usize) -> Reply + Send + Sync + 'static,
) -> (Arc<dyn sasacode_ai::provider::Fetch>, Times, Arc<Mutex<Vec<Sent>>>) {
    let times: Times = Arc::default();
    let at = times.clone();
    let (fetch, sent) = fake(move |_, n| {
        at.lock().unwrap().push(Instant::now());
        reply(n)
    });
    (fetch, times, sent)
}

/// The waits between attempts, in ms.
fn gaps(times: &Times) -> Vec<u128> {
    let t = times.lock().unwrap();
    t.windows(2).map(|w| (w[1] - w[0]).as_millis()).collect()
}

fn api_error(status: &str) -> String {
    json!({ "type": "error", "error": { "type": "api_error", "message": status } }).to_string()
}

#[tokio::test(start_paused = true)]
async fn every_attempt_is_made_max_retries_plus_one_times_and_numbered_the_last_error_is_the_status_and_the_body() {
    let (fetch, _, sent) = timed(|_| Reply::Status(500, json!({ "error": { "message": "down" } }).to_string()));
    let err = run(provider("openai-chat").as_ref(), req(chat_model(), fetch, 2))
        .await
        .unwrap_err();
    // openai: the status and `error.message`.
    assert_eq!(err.to_string(), "500 down");
    let counts: Vec<String> = sent
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.header("x-stainless-retry-count").unwrap())
        .collect();
    assert_eq!(counts, ["0", "1", "2"]);

    let (fetch, _, sent) = timed(|_| Reply::Status(529, api_error("overloaded")));
    let err = run(provider("anthropic").as_ref(), req(claude_model(), fetch, 1))
        .await
        .unwrap_err();
    // anthropic: the status and the whole body.
    assert_eq!(err.to_string(), format!("529 {}", api_error("overloaded")));
    assert_eq!(sent.lock().unwrap().len(), 2);

    // No body: the SDKs' own words.
    let (fetch, _, _) = timed(|_| Reply::Status(503, String::new()));
    let err = run(provider("openai-chat").as_ref(), req(chat_model(), fetch, 0))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "503 status code (no body)");
    // A body that is not JSON is the message.
    let (fetch, _, _) = timed(|_| Reply::Status(502, "Bad Gateway".into()));
    let err = run(provider("anthropic").as_ref(), req(claude_model(), fetch, 0))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "502 Bad Gateway");
}

#[tokio::test(start_paused = true)]
async fn only_408_409_429_and_5xx_are_retried_unless_x_should_retry_says_otherwise() {
    for (status, retried) in [
        (400, false),
        (401, false),
        (404, false),
        (408, true),
        (409, true),
        (429, true),
        (500, true),
        (529, true),
    ] {
        let (fetch, _, sent) = timed(move |_| Reply::Status(status, "{}".into()));
        let _ = run(provider("openai-chat").as_ref(), req(chat_model(), fetch, 1)).await;
        assert_eq!(sent.lock().unwrap().len(), if retried { 2 } else { 1 }, "{status}");
    }
    let header = |status: u16, value: &'static str| {
        timed(move |_| Reply::WithHeaders(status, "{}".into(), vec![("x-should-retry", value.to_string())]))
    };
    let (fetch, _, sent) = header(500, "false");
    let _ = run(provider("anthropic").as_ref(), req(claude_model(), fetch, 3)).await;
    assert_eq!(sent.lock().unwrap().len(), 1);
    let (fetch, _, sent) = header(400, "true");
    let _ = run(provider("openai-chat").as_ref(), req(chat_model(), fetch, 1)).await;
    assert_eq!(sent.lock().unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn the_wait_is_the_servers_hint_within_each_sdks_bounds_else_a_jittered_doubling_backoff() {
    async fn wait(m: ModelInfo, headers: Vec<(&'static str, &'static str)>) -> u128 {
        let (fetch, times, _) = timed(move |n| {
            let h = headers.iter().map(|(k, v)| (*k, v.to_string())).collect();
            if n == 1 {
                Reply::WithHeaders(429, "{}".into(), h)
            } else {
                Reply::Status(400, "{}".into())
            }
        });
        let api = m.api.clone();
        let _ = run(provider(&api).as_ref(), req(m, fetch, 1)).await;
        gaps(&times)[0]
    }
    let backoff = 375..=500; // 0.5 s minus up to 25%
    // retry-after-ms, then retry-after in seconds or as a date.
    assert_eq!(wait(chat_model(), vec![("retry-after-ms", "1500")]).await, 1500);
    assert_eq!(wait(claude_model(), vec![("retry-after-ms", "1500")]).await, 1500);
    assert_eq!(wait(chat_model(), vec![("retry-after", "2")]).await, 2000);
    assert_eq!(wait(claude_model(), vec![("retry-after", "2")]).await, 2000);
    assert_eq!(
        wait(chat_model(), vec![("retry-after-ms", "1500"), ("retry-after", "9")]).await,
        1500
    );
    let date = (chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc2822();
    let date: &'static str = Box::leak(date.into_boxed_str());
    assert!((28_000..=30_000).contains(&wait(chat_model(), vec![("retry-after", date)]).await));
    // openai takes 0 to 60 s; anthropic anything above 0 (a 0 falls through to retry-after).
    assert!(backoff.contains(&wait(chat_model(), vec![("retry-after-ms", "70000")]).await));
    assert_eq!(wait(claude_model(), vec![("retry-after-ms", "70000")]).await, 70_000);
    assert_eq!(
        wait(chat_model(), vec![("retry-after-ms", "0"), ("retry-after", "3")]).await,
        0
    );
    assert_eq!(
        wait(claude_model(), vec![("retry-after-ms", "0"), ("retry-after", "3")]).await,
        3000
    );
    assert!(backoff.contains(&wait(claude_model(), vec![("retry-after", "-1")]).await));
    assert!(backoff.contains(&wait(chat_model(), vec![("retry-after", "soon")]).await));
    assert!(backoff.contains(&wait(chat_model(), vec![]).await));

    // 0.5 s doubling to at most 8 s, each minus up to 25%.
    let (fetch, times, _) = timed(|_| Reply::Status(500, "{}".into()));
    let _ = run(provider("openai-chat").as_ref(), req(chat_model(), fetch, 6)).await;
    let bounds = [
        (375, 500),
        (750, 1000),
        (1500, 2000),
        (3000, 4000),
        (6000, 8000),
        (6000, 8000),
    ];
    for (gap, (lo, hi)) in gaps(&times).into_iter().zip(bounds) {
        assert!((lo..=hi).contains(&gap), "{gap} not in {lo}..={hi}");
    }
}

#[tokio::test(start_paused = true)]
async fn no_answer_in_600_s_is_request_timed_out_a_failed_connection_is_connection_error_both_retried() {
    let (fetch, times, _) = timed(|_| Reply::Hang);
    let started = Instant::now();
    let err = run(provider("openai-chat").as_ref(), req(chat_model(), fetch, 1))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "Request timed out.");
    assert_eq!(times.lock().unwrap().len(), 2);
    assert!(started.elapsed() >= Duration::from_secs(1200));
    assert!(!is_cut_reply(&err));

    let (fetch, times, _) = timed(|_| Reply::Fail("connection refused".into()));
    let err = run(provider("anthropic").as_ref(), req(claude_model(), fetch, 2))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "Connection error.");
    assert_eq!(times.lock().unwrap().len(), 3);
    assert!(!is_cut_reply(&err));
}

/// The reply, cancelling `signal` once the first delta of the reply arrived.
async fn cancel_on_first_delta(m: ModelInfo, reply: Reply) -> sasacode_ai::AssistantMessage {
    let reply = Arc::new(Mutex::new(Some(reply)));
    let (fetch, _) = fake(move |_, _| reply.lock().unwrap().take().unwrap_or(Reply::Hang));
    let signal = CancellationToken::new();
    let r = Request {
        signal: signal.clone(),
        ..req(m.clone(), fetch, 0)
    };
    let mut stream = provider(&m.api).stream(r);
    let mut done = None;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            StreamEvent::TextDelta { .. } => signal.cancel(),
            StreamEvent::Done { message } => done = Some(message),
            _ => {}
        }
    }
    done.expect("a done event")
}

#[tokio::test]
// The OpenAI SDK returned from an aborted stream without an error; a set signal still means aborted (#160).
async fn a_reply_the_user_interrupted_is_aborted_not_cut_off_when_its_stream_then_ends_without_an_error_all_three_formats()
 {
    let chat = sse(&[chat_chunk(json!({ "content": "Hel" }), None)], false).replace("data: [DONE]\n\n", "");
    let out = cancel_on_first_delta(chat_model(), Reply::Stalling(chat)).await;
    assert_eq!(
        (out.stop_reason, out.error_message.as_deref()),
        (StopReason::Aborted, None)
    );
    assert_eq!(content(&out), json!([{ "type": "text", "text": "Hel" }]));

    let responses = sse(
        &[
            json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "message", "id": "m1", "role": "assistant", "content": [] } }),
            json!({ "type": "response.output_text.delta", "item_id": "m1", "output_index": 0, "content_index": 0, "delta": "Hel" }),
        ],
        true,
    );
    let out = cancel_on_first_delta(responses_model(), Reply::Stalling(responses)).await;
    assert_eq!(
        (out.stop_reason, out.error_message.as_deref()),
        (StopReason::Aborted, None)
    );
    assert_eq!(content(&out), json!([{ "type": "text", "text": "Hel" }]));

    let claude = sse(
        &[
            json!({ "type": "message_start", "message": { "id": "m", "type": "message", "role": "assistant", "model": "claude-x", "content": [], "stop_reason": null, "usage": { "input_tokens": 5, "output_tokens": 0 } } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hel" } }),
        ],
        true,
    );
    let out = cancel_on_first_delta(claude_model(), Reply::Stalling(claude)).await;
    assert_eq!(
        (out.stop_reason, out.error_message.as_deref()),
        (StopReason::Aborted, None)
    );
    assert_eq!(content(&out), json!([{ "type": "text", "text": "Hel" }]));
}

#[tokio::test(start_paused = true)]
async fn an_abort_before_the_answer_or_while_waiting_to_retry_is_aborted_on_every_adapter() {
    for m in [chat_model(), responses_model(), claude_model()] {
        // Waiting for the response headers.
        let (fetch, _) = fixed(|| Reply::Hang);
        let signal = CancellationToken::new();
        let cancel = signal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            cancel.cancel();
        });
        let started = Instant::now();
        let (out, _) = run(
            provider(&m.api).as_ref(),
            Request {
                signal,
                ..req(m.clone(), fetch, 0)
            },
        )
        .await
        .unwrap();
        assert_eq!(out.stop_reason, StopReason::Aborted, "{}", m.api);
        assert!(started.elapsed() < Duration::from_secs(2));

        // Waiting to retry: one attempt, then the abort ends the backoff.
        let signal = CancellationToken::new();
        let cancel = signal.clone();
        let (fetch, sent) = fake(move |_, _| {
            cancel.cancel();
            Reply::Status(500, "{}".into())
        });
        let (out, _) = run(
            provider(&m.api).as_ref(),
            Request {
                signal,
                ..req(m.clone(), fetch, 5)
            },
        )
        .await
        .unwrap();
        assert_eq!(out.stop_reason, StopReason::Aborted, "{}", m.api);
        assert_eq!(sent.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn headers_merge_defaults_then_auth_then_the_models_then_content_type_names_case_insensitive() {
    let mut claude = claude_model();
    claude.headers = Some(
        [("User-Agent", "mine"), ("Content-Type", "text/plain"), ("X-Extra", "1")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into(),
    );
    let (fetch, sent) = fixed(|| Reply::Status(400, "{}".into()));
    let _ = run(provider("anthropic").as_ref(), req(claude, fetch, 0)).await;
    let s = sent.lock().unwrap()[0].clone();
    assert_eq!(s.url, "http://x/v1/messages");
    assert_eq!(s.header("user-agent").as_deref(), Some("mine"));
    assert_eq!(s.header("content-type").as_deref(), Some("application/json"));
    assert_eq!(s.header("x-extra").as_deref(), Some("1"));
    assert_eq!(s.header("x-api-key").as_deref(), Some("k"));
    assert_eq!(s.header("anthropic-version").as_deref(), Some("2023-06-01"));
    assert_eq!(s.header("accept").as_deref(), Some("application/json"));

    let mut chat = chat_model();
    chat.base_url = Some("http://x/v1/".into());
    chat.headers = Some([("Authorization".to_string(), "Bearer other".to_string())].into());
    let (fetch, sent) = fixed(|| Reply::Status(400, "{}".into()));
    let _ = run(provider("openai-chat").as_ref(), req(chat, fetch, 0)).await;
    let s = sent.lock().unwrap()[0].clone();
    // One slash dropped where the base ends with one.
    assert_eq!(s.url, "http://x/v1/chat/completions");
    assert_eq!(s.header("authorization").as_deref(), Some("Bearer other"));
}

/// Real HTTP: a server that sends the headers and part of a chunked body, then closes the connection.
#[tokio::test]
async fn a_connection_closed_mid_body_over_real_http_is_a_cut_reply() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 4096];
        // The request: its headers, then the body its content-length gives.
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            request.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&request).to_string();
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let event = format!("data: {}\n\n", chat_chunk(json!({ "content": "Hel" }), None));
        let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
        let chunk = format!("{:x}\r\n{event}\r\n", event.len());
        socket.write_all(format!("{head}{chunk}").as_bytes()).await.unwrap();
        socket.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(socket);
    });
    let mut m = chat_model();
    m.base_url = Some(format!("http://127.0.0.1:{port}/v1"));
    let r = Request {
        fetch: None,
        ..req(m, Arc::new(Unused), 0)
    };
    let (out, _) = run(provider("openai-chat").as_ref(), r).await.unwrap();
    assert_eq!(
        (out.stop_reason, out.error_message.as_deref()),
        (StopReason::Error, Some(INCOMPLETE_STREAM))
    );
    assert_eq!(content(&out), json!([{ "type": "text", "text": "Hel" }]));
}

struct Unused;
impl sasacode_ai::provider::Fetch for Unused {
    fn send(&self, _: reqwest::Request) -> futures::future::BoxFuture<'static, anyhow::Result<reqwest::Response>> {
        Box::pin(async { anyhow::bail!("not used") })
    }
}

/// Real HTTP: a model API that answers with a redirect. It is not followed (the key would go to the
/// other host); the error names where it pointed.
#[tokio::test]
async fn a_redirect_from_a_model_api_is_not_followed_and_the_error_says_so() {
    let other = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = format!("http://127.0.0.1:{}/v1/messages", other.local_addr().unwrap().port());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let location = target.clone();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 8192];
        let _ = socket.read(&mut buf).await.unwrap();
        let reply = format!("HTTP/1.1 307 Temporary Redirect\r\nlocation: {location}\r\ncontent-length: 0\r\n\r\n");
        socket.write_all(reply.as_bytes()).await.unwrap();
        socket.flush().await.unwrap();
    });
    let client = sasacode_ai::transport::ApiClient::anthropic(
        Some("sk-test"),
        false,
        &format!("http://127.0.0.1:{port}"),
        0,
        None,
        None,
    );
    let err = client
        .post("/v1/messages", &json!({}), &[], &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains(&format!("redirect to \"{target}\" not followed")),
        "{err}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), other.accept())
            .await
            .is_err(),
        "the other host was reached"
    );
}
