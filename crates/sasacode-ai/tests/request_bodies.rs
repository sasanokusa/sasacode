//! The request bodies byte for byte: key order, number format and escaping as the TS adapters wrote
//! them. `fixtures/request-bodies.json` is what the TS sent for the same request (made by a Bun
//! probe removed with the TS: `git log --diff-filter=D -- tests/fixtures/request-bodies.probe.ts`);
//! `Value` equality would not see the order.

mod common;

use common::*;
use sasacode_ai::replay::replay_scope;
use sasacode_ai::{Message, ModelInfo, Request, SamplingOptions, ThinkingLevel};
use serde_json::{Value, json};

const GOLDEN: &str = include_str!("fixtures/request-bodies.json");

fn png() -> Value {
    json!({ "type": "image", "mediaType": "image/png", "data": "iVBORw0KGgo=" })
}

fn history(m: &ModelInfo, signature: Option<&str>) -> Vec<Message> {
    let mut thinking = json!({ "type": "thinking", "thinking": "hmm" });
    if let Some(s) = signature {
        thinking["signature"] = json!(s);
    }
    vec![
        message(
            json!({ "role": "user", "content": [{ "type": "text", "text": "look at a.png" }, png()], "timestamp": 0 }),
        ),
        message(json!({
            "role": "assistant", "api": m.api, "provider": m.provider, "model": m.id, "replayScope": replay_scope(m),
            "timestamp": 0, "stopReason": "tool_use", "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "cost": 0 },
            "content": [
                thinking,
                { "type": "text", "text": "reading" },
                { "type": "tool_call", "id": "c1", "name": "read", "input": { "path": "a.png", "limit": 3 } },
            ],
        })),
        message(json!({
            "role": "tool", "toolCallId": "c1", "toolName": "read", "content": [{ "type": "text", "text": "Image a.png" }, png()],
            "isError": false, "timestamp": 0,
        })),
        message(
            json!({ "role": "user", "content": [{ "type": "text", "text": "and now? \u{2028} \"quoted\" é" }], "timestamp": 0 }),
        ),
    ]
}

async fn sent(m: ModelInfo, signature: Option<&str>, extra_body: Value) -> String {
    let (fetch, sent) = fixed(|| Reply::Fail("stop".into()));
    let req = Request {
        api_key: Some("k".into()),
        system: "sys".into(),
        messages: history(&m, signature),
        tools: vec![tool_spec(json!({
            "name": "read", "description": "read a file",
            "parameters": { "type": "object", "properties": { "path": { "type": "string" }, "limit": { "type": "number", "default": 2.5 } }, "required": ["path"] },
        }))],
        thinking: Some(ThinkingLevel::High),
        max_tokens: Some(500),
        sampling: Some(SamplingOptions {
            temperature: Some(0.7),
            top_p: Some(1.0),
            frequency_penalty: Some(0.1),
            presence_penalty: None,
            extra_body: extra_body.as_object().cloned(),
        }),
        ..request(m.clone(), fetch)
    };
    // The transport fails: only the first body matters.
    let _ = run(provider(&m.api).as_ref(), req).await;
    sent.lock().unwrap()[0].raw.clone()
}

fn golden(api: &str) -> String {
    let all: Value = serde_json::from_str(GOLDEN).unwrap();
    all[api].as_str().unwrap().to_string()
}

#[tokio::test]
async fn openai_chat_sends_the_body_the_ts_sent() {
    let m = model(json!({
        "id": "deepseek-v4-flash", "provider": "p", "api": "openai-chat", "baseUrl": "https://mock.invalid/v1",
        "contextWindow": 100000, "maxOutput": 1000, "reasoning": true,
    }));
    // Floats that hold integers (2.0) print as JS prints them.
    let extra = json!({ "stream_options": { "include_usage": false, "extra": 1 }, "seed": 2.0, "top_k": 40.5 });
    assert_eq!(sent(m, None, extra).await, golden("openai-chat"));
}

#[tokio::test]
async fn openai_responses_sends_the_body_the_ts_sent() {
    let m = model(json!({
        "id": "gpt-5.5", "provider": "openai", "api": "openai-responses", "baseUrl": "https://mock.invalid/v1",
        "contextWindow": 100000, "maxOutput": 1000, "reasoning": true,
    }));
    let signature = json!({ "type": "reasoning", "id": "rs0", "summary": [], "encrypted_content": "ENC" }).to_string();
    let extra = json!({ "store": true, "seed": 2.0, "top_k": 40.5 });
    assert_eq!(sent(m, Some(&signature), extra).await, golden("openai-responses"));
}

#[tokio::test]
async fn anthropic_sends_the_body_the_ts_sent() {
    let m = model(json!({
        "id": "claude-opus-5", "provider": "anthropic", "api": "anthropic", "contextWindow": 100000, "maxOutput": 1000,
        "reasoning": true,
    }));
    let extra = json!({ "max_tokens": 4096.0, "seed": 2.0, "top_k": 40.5 });
    assert_eq!(sent(m, Some("sig"), extra).await, golden("anthropic"));
}
