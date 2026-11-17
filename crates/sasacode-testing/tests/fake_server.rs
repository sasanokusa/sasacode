//! The fake server's wire format, pinned to what `packages/e2e/test/fake-server.ts` sent (captured
//! with curl from the TS server; its random tool-call ids replaced by the Rust ones).

use sasacode_testing::{FakeServer, Reply, ToolCallShape, ToolCallSpec};
use serde_json::json;

struct Got {
    status: u16,
    content_type: String,
    body: String,
}

async fn post(server: &FakeServer, body: &str) -> Got {
    let r = sasacode_ai::http::client()
        .post(format!("{}/chat/completions", server.url()))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .expect("the request");
    got(r).await
}

async fn get(server: &FakeServer, path: &str) -> Got {
    got(sasacode_ai::http::client()
        .get(format!("{}{path}", server.url()))
        .send()
        .await
        .expect("the request"))
    .await
}

async fn got(r: reqwest::Response) -> Got {
    let status = r.status().as_u16();
    let content_type = r
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Got {
        status,
        content_type,
        body: r.text().await.expect("the body"),
    }
}

const CHUNK: &str =
    r#"{"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":"#;

fn data(delta: &str) -> String {
    format!("data: {CHUNK}{delta},\"finish_reason\":null}}]}}\n\n")
}

fn tail(finish: &str) -> String {
    format!(
        "data: {CHUNK}{{}},\"finish_reason\":\"{finish}\"}}]}}\n\ndata: {{\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"m\",\"choices\":[],\"usage\":{{\"prompt_tokens\":100,\"completion_tokens\":20,\"total_tokens\":120}}}}\n\n"
    )
}

#[tokio::test]
async fn replies_stream_like_the_ts_server() {
    let no_index = Reply {
        reasoning: Some("abcdefg".into()),
        text: Some("hi".into()),
        tool_calls: Some(vec![ToolCallSpec::new("w", json!({ "a": 1, "b": "x" }))]),
        tool_call_shape: Some(ToolCallShape::NoIndex),
        ..Reply::default()
    };
    let default_shape = Reply {
        text: Some("日本語のテキスト".into()),
        tool_calls: Some(vec![
            ToolCallSpec::new("write", json!({ "path": "a.txt", "content": "x\n" })),
            ToolCallSpec::new("read", json!({ "path": "b" })),
        ]),
        ..Reply::default()
    };
    let name_later = Reply::tool_calls(vec![ToolCallSpec::new("read", json!({ "path": "b.txt" }))])
        .with_shape(ToolCallShape::NameLater);
    let mut server = FakeServer::with_models(
        vec![
            no_index,
            default_shape,
            name_later,
            Reply::status(503),
            Reply::stream_error("bad effort"),
            Reply::text("x").with_cut(),
            Reply {
                reasoning: Some(String::new()),
                text: Some(String::new()),
                cut: true,
                ..Reply::default()
            },
            Reply {
                status: Some(0),
                ..Reply::text("z")
            },
        ],
        vec![json!({ "id": "m", "max_model_len": 4096 })],
    )
    .unwrap();

    let r = post(&server, r#"{"model":"m","messages":[]}"#).await;
    assert_eq!((r.status, r.content_type.as_str()), (200, "text/event-stream"));
    let expected = [
        data(r#"{"reasoning_content":"abc"}"#),
        data(r#"{"reasoning_content":"def"}"#),
        data(r#"{"reasoning_content":"g"}"#),
        data(r#"{"content":"h"}"#),
        data(r#"{"content":"i"}"#),
        data(r#"{"tool_calls":[{"id":"call_0_000001","type":"function","function":{"name":"w","arguments":"{\"a\":1,\""}}]}"#),
        data(r#"{"tool_calls":[{"function":{"name":"w","arguments":"b\":\"x\"}"}}]}"#),
        tail("tool_calls"),
        "data: [DONE]\n\n".to_string(),
    ]
    .concat();
    assert_eq!(r.body, expected);

    let r = post(&server, r#"{"model":"m"}"#).await;
    let expected = [
        data(r#"{"content":"日本語"}"#),
        data(r#"{"content":"のテキ"}"#),
        data(r#"{"content":"スト"}"#),
        data(
            r#"{"tool_calls":[{"index":0,"id":"call_0_000002","type":"function","function":{"name":"write","arguments":"{\"path\":\"a.txt\",\"content\":\"x\\n\"}"}}]}"#,
        ),
        data(r#"{"tool_calls":[{"index":1,"id":"call_1_000002","type":"function","function":{"name":"read","arguments":"{\"path\":\"b\"}"}}]}"#),
        tail("tool_calls"),
        "data: [DONE]\n\n".to_string(),
    ]
    .concat();
    assert_eq!(r.body, expected);

    let r = post(&server, r#"{"model":"m"}"#).await;
    let expected = [
        data(r#"{"tool_calls":[{"index":0,"id":"call_0_000003","type":"function","function":{"arguments":""}}]}"#),
        data(r#"{"tool_calls":[{"index":0,"function":{"name":"read"}}]}"#),
        data(r#"{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"b.txt\"}"}}]}"#),
        tail("tool_calls"),
        "data: [DONE]\n\n".to_string(),
    ]
    .concat();
    assert_eq!(r.body, expected);

    let r = post(&server, r#"{"model":"m"}"#).await;
    assert_eq!(
        (r.status, r.content_type.as_str()),
        (503, "application/json;charset=utf-8")
    );
    assert_eq!(r.body, r#"{"error":{"message":"fake failure 503"}}"#);

    let r = post(&server, r#"{"model":"m"}"#).await;
    assert_eq!(r.status, 200);
    assert_eq!(
        r.body,
        "data: {\"error\":{\"message\":\"bad effort\",\"type\":\"BadRequestError\",\"code\":400}}\n\n"
    );

    let r = post(&server, r#"{"model":"m"}"#).await;
    assert_eq!(r.body, data(r#"{"content":"x"}"#));

    let r = post(&server, r#"{"model":"m"}"#).await;
    assert_eq!((r.status, r.body.as_str()), (200, ""));

    // No model in the request: none in the chunks. A zero status is no status.
    let r = post(&server, r#"{"messages":[]}"#).await;
    let unnamed = |s: String| s.replace(r#""model":"m","#, "");
    assert_eq!(
        r.body,
        unnamed([data(r#"{"content":"z"}"#), tail("stop"), "data: [DONE]\n\n".to_string()].concat())
    );

    let r = post(&server, r#"{"model":"m"}"#).await;
    assert_eq!(
        (r.status, r.content_type.as_str()),
        (500, "application/json;charset=utf-8")
    );
    assert_eq!(r.body, r#"{"error":{"message":"the fake server has no more replies"}}"#);

    let r = get(&server, "/models").await;
    assert_eq!(
        (r.status, r.content_type.as_str()),
        (200, "application/json;charset=utf-8")
    );
    assert_eq!(
        r.body,
        r#"{"object":"list","data":[{"object":"model","owned_by":"fake","id":"m","max_model_len":4096}]}"#
    );
    let r = get(&server, "/models?x=1").await;
    assert_eq!(r.status, 200);

    let r = get(&server, "/other").await;
    assert_eq!(
        (r.status, r.content_type.as_str(), r.body.as_str()),
        (404, "text/plain;charset=utf-8", "not found")
    );

    assert_eq!(server.left(), 0);
    let requests = server.requests();
    assert_eq!(requests.len(), 9);
    assert_eq!(requests[0], json!({ "model": "m", "messages": [] }));
    assert_eq!(requests[7], json!({ "messages": [] }));
    server.stop();
    assert!(
        sasacode_ai::http::client()
            .get(format!("{}/models", server.url()))
            .send()
            .await
            .is_err()
    );
}

#[test]
fn it_serves_from_a_synchronous_test_and_stops_on_drop() {
    let server = FakeServer::start(vec![Reply::text("ok")]).unwrap();
    let url = server.url().to_string();
    assert!(url.starts_with("http://127.0.0.1:") && url.ends_with("/v1"), "{url}");
    let models = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            sasacode_ai::http::client()
                .get(format!("{url}/models"))
                .send()
                .await
                .unwrap()
                .text()
                .await
        });
    assert_eq!(
        models.unwrap(),
        r#"{"object":"list","data":[{"object":"model","owned_by":"fake","id":"m"}]}"#
    );
    drop(server);
    assert!(std::net::TcpStream::connect(url.trim_start_matches("http://").trim_end_matches("/v1")).is_err());
}
