//! What the Anthropic adapter sends, and what it learns from the rejections it gets
//! (`packages/ai/test/anthropic-request.test.ts`).

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use sasacode_ai::anthropic::{AnthropicProvider, Known, thinking_fields};
use sasacode_ai::replay::replay_scope;
use sasacode_ai::{AssistantMessage, Message, Request, StopReason, ThinkingLevel};
use serde_json::{Value, json};

fn hello() -> Message {
    message(json!({ "role": "user", "content": [{ "type": "text", "text": "hello" }], "timestamp": 0 }))
}

fn response(stop: &str) -> Reply {
    let events = [
        json!({ "type": "message_start", "message": { "id": "mock", "type": "message", "role": "assistant", "model": "mock", "content": [], "stop_reason": null, "usage": { "input_tokens": 5, "output_tokens": 0, "cache_read_input_tokens": 20, "cache_creation_input_tokens": 30 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "ok" } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": stop }, "usage": { "output_tokens": 2 } }),
        json!({ "type": "message_stop" }),
    ];
    Reply::Sse(sse(&events, true))
}

fn ok() -> Reply {
    response("end_turn")
}

fn bad(message: &str) -> Reply {
    Reply::Status(
        400,
        json!({ "type": "error", "error": { "type": "invalid_request_error", "message": message } }).to_string(),
    )
}

/// A request to `id` at `base` (none: the official API), answered by `handler(body, sent)`.
async fn run_with(
    base: Option<&str>,
    id: &str,
    handler: impl Fn(&Value, &Sent) -> Reply + Send + Sync + 'static,
    extra: impl FnOnce(&mut Request),
) -> anyhow::Result<AssistantMessage> {
    let mut m = json!({ "id": id, "provider": "anthropic", "api": "anthropic", "reasoning": true, "contextWindow": 1_000_000, "maxOutput": 1000 });
    if let Some(base) = base {
        m["baseUrl"] = json!(base);
    }
    let (fetch, _) = fake(move |sent, _| handler(&sent.body, sent));
    let mut req = Request {
        api_key: Some("mock-key".into()),
        system: "system".into(),
        messages: vec![hello()],
        tools: vec![tool_spec(
            json!({ "name": "add", "description": "Add", "parameters": { "type": "object" } }),
        )],
        ..request(model(m), fetch)
    };
    extra(&mut req);
    Ok(run(&AnthropicProvider, req).await?.0)
}

fn thinking(level: ThinkingLevel) -> impl FnOnce(&mut Request) {
    move |r| r.thinking = Some(level)
}

/// `toBeUndefined`: not sent at all (an explicit `null` is not that).
fn absent(v: &Value, key: &str) -> bool {
    v.get(key).is_none()
}

#[tokio::test]
async fn official_only_fields_use_the_url_origin_not_a_hostname_prefix() {
    let cases = [
        (None, true),
        (Some("https://api.anthropic.com/"), true),
        (Some("https://api.anthropic.com.evil.invalid"), false),
        (Some("https://api.anthropic.com@proxy.invalid"), false),
        (Some("https://api.anthropic.com:8443"), false),
        (Some("http://api.anthropic.com"), false),
    ];
    for (base, official) in cases {
        run_with(
            base,
            "claude-opus-5",
            move |b, _| {
                if official {
                    assert_eq!(b["thinking"], json!({ "type": "disabled" }), "{base:?}");
                    assert_eq!(b["tools"][0]["eager_input_streaming"], json!(true), "{base:?}");
                } else {
                    assert!(absent(b, "thinking"), "{base:?}");
                    assert!(absent(&b["tools"][0], "eager_input_streaming"), "{base:?}");
                }
                ok()
            },
            thinking(ThinkingLevel::Off),
        )
        .await
        .unwrap();
    }
}

#[test]
fn off_stays_omitted_on_proxies_including_always_thinking_models() {
    for id in [
        "claude-opus-5-5",
        "claude-fable-5-1",
        "claude-mythos-5-1",
        "claude-sonnet-5-5",
    ] {
        assert_eq!(
            Value::Object(thinking_fields(id, Some(ThinkingLevel::Off), false, &Known::default())),
            json!({}),
            "{id}"
        );
    }
    let official = thinking_fields("claude-sonnet-5-5", Some(ThinkingLevel::Off), true, &Known::default());
    assert_eq!(official["thinking"]["type"], "between_tools");
}

#[tokio::test]
async fn an_effort_rejection_is_remembered_only_for_the_endpoint_that_rejected_it() {
    let efforts = Arc::new(Mutex::new(Vec::<String>::new()));
    let handler = {
        let efforts = efforts.clone();
        move |b: &Value, _: &Sent| {
            let effort = b["output_config"]["effort"].as_str().unwrap_or_default().to_string();
            efforts.lock().unwrap().push(effort.clone());
            if effort == "xhigh" {
                bad("effort xhigh not supported")
            } else {
                ok()
            }
        }
    };
    let h = Arc::new(handler);
    for _ in 0..2 {
        let h = h.clone();
        run_with(
            Some("https://reject-effort.invalid"),
            "claude-test-effort",
            move |b, s| h(b, s),
            thinking(ThinkingLevel::Xhigh),
        )
        .await
        .unwrap();
    }
    assert_eq!(*efforts.lock().unwrap(), ["xhigh", "high", "high"]);
    run_with(
        Some("https://accept-effort.invalid"),
        "claude-test-effort",
        |b, _| {
            assert_eq!(b["output_config"]["effort"], "xhigh");
            ok()
        },
        thinking(ThinkingLevel::Xhigh),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn disabled_rejection_retries_with_low_effort_and_does_not_change_proxy_off() {
    let shapes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = shapes.clone();
    run_with(
        None,
        "claude-test-disable",
        move |b, _| {
            seen.lock().unwrap().push(b["thinking"].clone());
            if b["thinking"]["type"] == "disabled" {
                return bad("thinking.type.disabled is not supported");
            }
            assert_eq!(b["output_config"]["effort"], "low");
            ok()
        },
        thinking(ThinkingLevel::Off),
    )
    .await
    .unwrap();
    assert_eq!(
        *shapes.lock().unwrap(),
        [
            json!({ "type": "disabled" }),
            json!({ "type": "adaptive", "display": "summarized" })
        ]
    );
    run_with(
        None,
        "claude-test-disable",
        |b, _| {
            assert_eq!(b["thinking"]["type"], "adaptive");
            assert_eq!(b["output_config"]["effort"], "low");
            ok()
        },
        thinking(ThinkingLevel::Off),
    )
    .await
    .unwrap();
    run_with(
        Some("https://disable-proxy.invalid"),
        "claude-test-disable",
        |b, _| {
            assert!(absent(b, "thinking"));
            ok()
        },
        thinking(ThinkingLevel::Off),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn binding_beta_preserves_custom_headers_unsupported_binding_retries_without_it() {
    let binding = Arc::new(Mutex::new(Vec::<bool>::new()));
    let seen = binding.clone();
    run_with(
        None,
        "claude-sonnet-5-5",
        move |b, h| {
            let bound = sasacode_ai::transport::js_truthy(&b["thinking"]["block_binding"]);
            seen.lock().unwrap().push(bound);
            if bound {
                assert_eq!(
                    b["thinking"]["block_binding"],
                    json!({ "prefix_mismatch_behavior": "drop_block" })
                );
                assert_eq!(
                    h.header("anthropic-beta").as_deref(),
                    Some("custom-beta,thinking-binding-controls-2026-08-01")
                );
                return bad("block_binding: Extra inputs are not permitted");
            }
            assert_eq!(h.header("anthropic-beta").as_deref(), Some("custom-beta"));
            ok()
        },
        |r| {
            r.model = model(json!({
                "id": "claude-sonnet-5-5", "provider": "anthropic", "api": "anthropic", "reasoning": true,
                "contextWindow": 1_000_000, "maxOutput": 1000, "headers": { "anthropic-beta": "custom-beta" },
            }));
            r.thinking = Some(ThinkingLevel::High);
        },
    )
    .await
    .unwrap();
    assert_eq!(*binding.lock().unwrap(), [true, false]);
}

#[tokio::test]
async fn unrelated_block_binding_errors_do_not_repeat_an_unchanged_request() {
    let calls = Arc::new(Mutex::new(0));
    let counted = calls.clone();
    let result = run_with(
        Some("https://proxy-binding-error.invalid"),
        "claude-test-binding",
        move |_, _| {
            *counted.lock().unwrap() += 1;
            bad("messages contain invalid block_binding")
        },
        |_| {},
    )
    .await;
    assert!(result.is_err());
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn signed_and_redacted_thinking_replay_cache_counters_and_refusal_survive_streaming() {
    let scope = replay_scope(&model(json!({
        "id": "claude-test-replay", "provider": "anthropic", "api": "anthropic", "baseUrl": "https://replay.invalid",
        "contextWindow": 1_000_000, "maxOutput": 1000,
    })));
    let earlier = message(json!({
        "role": "assistant", "provider": "anthropic", "api": "anthropic", "model": "claude-test-replay", "timestamp": 0,
        "stopReason": "stop", "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "cost": 0 },
        "content": [
            { "type": "thinking", "thinking": "reason", "signature": "signed" },
            { "type": "thinking", "thinking": "", "signature": "redacted", "redacted": true },
            { "type": "text", "text": "earlier" },
        ],
        "replayScope": scope,
    }));
    let out = run_with(
        Some("https://replay.invalid"),
        "claude-test-replay",
        |b, _| {
            assert_eq!(b["cache_control"], json!({ "type": "ephemeral" }));
            assert_eq!(
                Value::Array(b["messages"][1]["content"].as_array().unwrap()[..2].to_vec()),
                json!([{ "type": "thinking", "thinking": "reason", "signature": "signed" }, { "type": "redacted_thinking", "data": "redacted" }])
            );
            response("refusal")
        },
        move |r| r.messages = vec![hello(), earlier, hello()],
    )
    .await
    .unwrap();
    assert_eq!(out.stop_reason, StopReason::Refusal);
    assert_eq!(
        (
            out.usage.input,
            out.usage.output,
            out.usage.cache_read,
            out.usage.cache_write
        ),
        (5, 2, 20, 30)
    );
}
