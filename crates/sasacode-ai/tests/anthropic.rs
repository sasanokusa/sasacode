//! The Anthropic adapter's thinking fields, and its retries against a local Messages API server over real
//! HTTP (`packages/ai/test/anthropic.test.ts`, where the server was `Bun.serve`).

mod common;

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use common::*;
use sasacode_ai::anthropic::{AnthropicProvider, Known, thinking_fields};
use sasacode_ai::replay::replay_scope;
use sasacode_ai::{AssistantMessage, Message, ModelInfo, Request, ThinkingLevel};
use serde_json::{Value, json};

#[test]
fn effort_off_said_explicitly_on_claude_5_low_effort_where_thinking_cannot_be_turned_off() {
    let fields = |id: &str, level: ThinkingLevel, official: bool| {
        Value::Object(thinking_fields(id, Some(level), official, &Known::default()))
    };
    assert_eq!(
        fields("claude-opus-5", ThinkingLevel::Off, true),
        json!({ "thinking": { "type": "disabled" } })
    );
    assert_eq!(
        fields("claude-sonnet-5", ThinkingLevel::Off, true),
        json!({ "thinking": { "type": "disabled" } })
    );
    for id in ["claude-opus-5-5", "claude-fable-5-1"] {
        assert_eq!(
            fields(id, ThinkingLevel::Off, true),
            json!({ "thinking": { "type": "adaptive", "display": "summarized" }, "output_config": { "effort": "low" } }),
            "{id}"
        );
    }
    // A proxy or another server speaking the Messages API gets what it got before.
    assert_eq!(fields("claude-opus-5", ThinkingLevel::Off, false), json!({}));
    assert_eq!(
        fields("claude-opus-5", ThinkingLevel::High, true),
        json!({ "thinking": { "type": "adaptive", "display": "summarized" }, "output_config": { "effort": "high" } })
    );
}

type Bodies = Arc<Mutex<Vec<Value>>>;

/// A Messages API server: rejects a request's thinking blocks as bound elsewhere, or xhigh.
async fn messages(State(bodies): State<Bodies>, Json(body): Json<Value>) -> Response {
    bodies.lock().unwrap().push(body.clone());
    let has_thinking = body["messages"].as_array().into_iter().flatten().any(|m| {
        m["content"]
            .as_array()
            .is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "thinking"))
    });
    let error = |message: &str| {
        let body = json!({ "type": "error", "error": { "type": "invalid_request_error", "message": message } });
        (StatusCode::BAD_REQUEST, Json(body)).into_response()
    };
    if has_thinking {
        return error(
            "messages.1.content.0: Invalid `signature` in `thinking` block. The block is bound to a different conversation.",
        );
    }
    if body["output_config"]["effort"] == "xhigh" {
        return error("output_config.effort: xhigh is not supported for this model");
    }
    let events = [
        json!({ "type": "message_start", "message": { "id": "x", "type": "message", "role": "assistant", "model": body["model"], "content": [], "stop_reason": null, "usage": { "input_tokens": 5, "output_tokens": 0 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "ok" } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 1 } }),
        json!({ "type": "message_stop" }),
    ];
    ([("content-type", "text/event-stream")], sse(&events, true)).into_response()
}

/// The server's base URL, and the bodies it was sent.
async fn serve() -> (String, Bodies) {
    let bodies: Bodies = Arc::default();
    let app = axum::Router::new()
        .route("/v1/messages", axum::routing::post(messages))
        .with_state(bodies.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (base, bodies)
}

fn model_at(base: &str, id: &str) -> ModelInfo {
    model(json!({
        "id": id, "provider": "anthropic", "api": "anthropic", "baseUrl": base, "contextWindow": 1_000_000,
        "maxOutput": 1000, "reasoning": true,
    }))
}

async fn run_at(base: &str, id: &str, messages: Vec<Message>, thinking: Option<ThinkingLevel>) -> AssistantMessage {
    let req = Request {
        api_key: Some("k".into()),
        messages,
        thinking,
        fetch: None,
        ..request(model_at(base, id), Arc::new(NoFetch))
    };
    run(&AnthropicProvider, req).await.unwrap().0
}

/// Never used: the requests above go out through the shared client (`fetch: None`).
struct NoFetch;
impl sasacode_ai::provider::Fetch for NoFetch {
    fn send(&self, _: reqwest::Request) -> futures::future::BoxFuture<'static, anyhow::Result<reqwest::Response>> {
        Box::pin(async { anyhow::bail!("not used") })
    }
}

#[tokio::test]
async fn thinking_bound_to_another_conversation_sent_again_once_without_thinking_blocks() {
    let (base, bodies) = serve().await;
    let earlier = message(json!({
        "replayScope": replay_scope(&model_at(&base, "claude-opus-5-5")), "role": "assistant", "api": "anthropic",
        "provider": "anthropic", "model": "claude-opus-5-5",
        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "cost": 0 }, "stopReason": "stop", "timestamp": 0,
        "content": [{ "type": "thinking", "thinking": "hmm", "signature": "sig" }, { "type": "text", "text": "earlier" }],
    }));
    let out = run_at(&base, "claude-opus-5-5", vec![user("a"), earlier, user("b")], None).await;
    assert_eq!(content(&out), json!([{ "type": "text", "text": "ok" }]));
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    let resent = bodies[1]["messages"].to_string();
    assert!(!resent.contains("thinking"), "{resent}");
    assert!(resent.contains("earlier"), "{resent}");
}

#[tokio::test]
async fn xhigh_rejected_the_closest_effort_is_sent_instead_and_remembered() {
    let (base, bodies) = serve().await;
    let say = || vec![user("a")];
    let out = run_at(&base, "claude-sonnet-4-6", say(), Some(ThinkingLevel::Xhigh)).await;
    assert_eq!(content(&out), json!([{ "type": "text", "text": "ok" }]));
    let efforts = |bodies: &Bodies| -> Vec<Value> {
        bodies
            .lock()
            .unwrap()
            .iter()
            .map(|b| b["output_config"]["effort"].clone())
            .collect()
    };
    assert_eq!(efforts(&bodies), [json!("xhigh"), json!("high")]);
    run_at(&base, "claude-sonnet-4-6", say(), Some(ThinkingLevel::Xhigh)).await;
    assert_eq!(efforts(&bodies).last(), Some(&json!("high")));
    assert_eq!(bodies.lock().unwrap().len(), 3);
}
