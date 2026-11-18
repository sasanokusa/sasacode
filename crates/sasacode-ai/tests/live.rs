//! Against the real APIs, and only when asked: `SASACODE_LIVE=1` plus the provider's key, and `--ignored`.
//! Each run costs a few cents. Checks what the recorded tests cannot: that the providers still accept what
//! the adapters send (tool calls, thinking replay, caching) (`packages/ai/test/live.test.ts`).
//!
//!   SASACODE_LIVE=1 ANTHROPIC_API_KEY=… OPENAI_API_KEY=… cargo test -p sasacode-ai --test live -- --ignored

mod common;

use std::time::Duration;

use common::*;
use sasacode_ai::{AssistantContent, AssistantMessage, Message, ModelInfo, Request, StopReason, ThinkingLevel};
use serde_json::json;

/// The key, when this run is live and has one.
fn live_key(key: &str) -> Option<String> {
    if std::env::var("SASACODE_LIVE").ok().as_deref() != Some("1") {
        return None;
    }
    std::env::var(key).ok().filter(|k| !k.is_empty())
}

async fn send(spec: &str, key: &str, messages: Vec<Message>, thinking: ThinkingLevel) -> AssistantMessage {
    let (provider_name, id) = spec.split_once('/').unwrap();
    let api = if provider_name == "anthropic" {
        "anthropic"
    } else {
        "openai-responses"
    };
    let model: ModelInfo = model(json!({
        "id": id, "provider": provider_name, "api": api, "contextWindow": 200_000, "maxOutput": 32_000, "reasoning": true,
    }));
    let tools = vec![tool_spec(json!({
        "name": "add", "description": "Add two numbers",
        "parameters": { "type": "object", "properties": { "a": { "type": "number" }, "b": { "type": "number" } }, "required": ["a", "b"] },
    }))];
    let req = Request {
        model,
        api_key: Some(key.to_string()),
        system: "Use the add tool for arithmetic.".into(),
        messages,
        tools,
        thinking: Some(thinking),
        max_tokens: Some(2048),
        max_retries: None,
        sampling: None,
        signal: Default::default(),
        fetch: None,
    };
    run(provider(api).as_ref(), req).await.unwrap().0
}

async fn a_tool_call_its_result_and_the_reply_after_it(spec: &str, key_var: &str) {
    let Some(key) = live_key(key_var) else {
        eprintln!("skipped: SASACODE_LIVE=1 and {key_var} are needed");
        return;
    };
    let body = async {
        let mut messages = vec![message(
            json!({ "role": "user", "content": [{ "type": "text", "text": "What is 17 + 25?" }], "timestamp": 0 }),
        )];
        let first = send(spec, &key, messages.clone(), ThinkingLevel::Low).await;
        let call = first.content.iter().find_map(AssistantContent::as_tool_call).cloned();
        assert_eq!(call.as_ref().map(|c| c.name.as_str()), Some("add"));
        let Some(call) = call else { return };
        messages.push(Message::Assistant(first));
        messages.push(message(json!({
            "role": "tool", "toolCallId": call.id, "toolName": "add", "content": [{ "type": "text", "text": "42" }],
            "isError": false, "timestamp": 0,
        })));
        let second = send(spec, &key, messages, ThinkingLevel::Low).await;
        assert_eq!(second.stop_reason, StopReason::Stop);
        assert!(
            second
                .content
                .iter()
                .any(|b| b.as_text().is_some_and(|t| t.contains("42")))
        );
        assert!(second.usage.input + second.usage.cache_read > 0);
    };
    tokio::time::timeout(Duration::from_secs(120), body)
        .await
        .expect("within 120 s");
}

#[tokio::test]
#[ignore = "live: SASACODE_LIVE=1 and ANTHROPIC_API_KEY"]
async fn live_anthropic_claude_sonnet_5_a_tool_call_its_result_and_the_reply_after_it_thinking_replayed() {
    a_tool_call_its_result_and_the_reply_after_it("anthropic/claude-sonnet-5", "ANTHROPIC_API_KEY").await;
}

#[tokio::test]
#[ignore = "live: SASACODE_LIVE=1 and OPENAI_API_KEY"]
async fn live_openai_gpt_5_5_a_tool_call_its_result_and_the_reply_after_it_thinking_replayed() {
    a_tool_call_its_result_and_the_reply_after_it("openai/gpt-5.5", "OPENAI_API_KEY").await;
}
