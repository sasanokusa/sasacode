//! Which reasoning the chat adapter sends back to servers that want it, and in what field
//! (`packages/ai/test/openai-reasoning-replay.test.ts`).

mod common;

use common::*;
use sasacode_ai::openai_chat::OpenAiChatProvider;
use sasacode_ai::replay::replay_scope;
use sasacode_ai::{AssistantContent, AssistantMessage, Message, Request, ToolSpec};
use serde_json::{Value, json};

fn tool() -> ToolSpec {
    tool_spec(json!({ "name": "add", "description": "Add", "parameters": { "type": "object", "properties": {} } }))
}

fn go() -> Message {
    user("go")
}

/// An earlier reply of `id` at the mock server: its thoughts, a text, and two calls when `calls`.
fn earlier(id: &str, calls: bool, thoughts: &[&str]) -> Value {
    let scope = replay_scope(&model(json!({
        "id": id, "provider": "test", "api": "openai-chat", "baseUrl": "https://mock.invalid/v1",
        "contextWindow": 1_000_000, "maxOutput": 1000,
    })));
    let mut content: Vec<Value> = thoughts
        .iter()
        .map(|t| json!({ "type": "thinking", "thinking": t }))
        .collect();
    content.push(json!({ "type": "text", "text": if calls { "working" } else { "done" } }));
    if calls {
        for i in [1, 2] {
            content.push(json!({ "type": "tool_call", "id": format!("call{i}"), "name": "add", "input": {} }));
        }
    }
    json!({
        "replayScope": scope, "role": "assistant", "api": "openai-chat", "provider": "test", "model": id, "timestamp": 0,
        "stopReason": if calls { "tool_use" } else { "stop" },
        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "cost": 0 }, "content": content,
    })
}

fn assistant_msg(id: &str, calls: bool, thoughts: &[&str]) -> Message {
    message(earlier(id, calls, thoughts))
}

fn first_second(id: &str) -> Message {
    assistant_msg(id, true, &["first", "second"])
}

fn response(field: &str) -> Reply {
    let chunks = [
        json!({ "choices": [{ "index": 0, "delta": { field: "new-thought" }, "finish_reason": null }] }),
        json!({ "choices": [{ "index": 0, "delta": { "content": "ok" }, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 4, "completion_tokens": 7, "prompt_tokens_details": { "cached_tokens": 0 } } }),
    ];
    Reply::Sse(sse(&chunks, false))
}

async fn run_check(
    id: &str,
    messages: Vec<Message>,
    check: impl Fn(&Value) -> Option<Reply> + Send + Sync + 'static,
    tools: Vec<ToolSpec>,
    field: &'static str,
) -> AssistantMessage {
    let (fetch, _) = fake(move |s, _| check(&s.body).unwrap_or_else(|| response(field)));
    let m = model(json!({
        "id": id, "provider": "test", "api": "openai-chat", "baseUrl": "https://mock.invalid/v1", "reasoning": false,
        "contextWindow": 1_000_000, "maxOutput": 1000,
    }));
    let req = Request {
        api_key: Some("fake".into()),
        system: "system".into(),
        messages,
        tools,
        ..request(m, fetch)
    };
    let (done, _) = run(&OpenAiChatProvider, req).await.unwrap();
    assert!(
        done.content
            .iter()
            .any(|c| matches!(c, AssistantContent::Thinking(t) if t.thinking == "new-thought")),
        "{id}"
    );
    done
}

fn results() -> Vec<Message> {
    [1, 2]
        .map(|i| {
            message(json!({
                "role": "tool", "toolCallId": format!("call{i}"), "toolName": "add", "content": [{ "type": "text", "text": "42" }],
                "isError": false, "timestamp": 0,
            }))
        })
        .to_vec()
}

fn of_role<'a>(b: &'a Value, role: &str) -> Vec<&'a Value> {
    b["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == role)
        .collect()
}

fn has(m: &Value, key: &str) -> bool {
    m.get(key).is_some()
}

#[tokio::test]
async fn deepseek_keeps_complete_reasoning_for_tool_requests_including_plain_replies_across_user_turns() {
    for id in [
        "deepseek-flash",
        "deepseek-v4-flash",
        "deepseek-v4.1-flash",
        "deepseek/deepseek-v4.1-flash",
    ] {
        let mut messages = vec![go(), first_second(id)];
        messages.extend(results());
        messages.extend([assistant_msg(id, false, &["final thought"]), go()]);
        let check = move |b: &Value| {
            let a = of_role(b, "assistant");
            let reasoning: Vec<&Value> = a.iter().map(|m| &m["reasoning_content"]).collect();
            assert_eq!(reasoning, [&json!("firstsecond"), &json!("final thought")], "{id}");
            let ids: Vec<&Value> = a[0]["tool_calls"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| &c["id"])
                .collect();
            assert_eq!(ids, [&json!("call1"), &json!("call2")]);
            let tool: Vec<&Value> = of_role(b, "tool").iter().map(|m| &m["content"]).collect();
            assert_eq!(tool, [&json!("42"), &json!("42")]);
            None
        };
        run_check(id, messages, check, vec![tool()], "reasoning_content").await;
    }
}

#[tokio::test]
async fn deepseek_without_tools_omits_ignored_reasoning() {
    let id = "deepseek-flash";
    let check = |b: &Value| {
        assert!(!has(&b["messages"][2], "reasoning_content"));
        None
    };
    run_check(
        id,
        vec![go(), assistant_msg(id, false, &["first", "second"]), go()],
        check,
        vec![],
        "reasoning_content",
    )
    .await;
}

#[tokio::test]
async fn mimo_preserves_tool_call_reasoning_across_user_turns_including_when_tools_are_subsequently_absent() {
    for id in ["mimo-v2.6-flash", "xiaomi/mimo-v2.6-flash"] {
        for tools in [vec![tool()], vec![]] {
            let mut messages = vec![go(), first_second(id)];
            messages.extend(results());
            messages.extend([assistant_msg(id, false, &["first", "second"]), go()]);
            let check = |b: &Value| {
                let a = of_role(b, "assistant");
                assert_eq!(a[0]["reasoning_content"], "firstsecond");
                assert!(!has(a[1], "reasoning_content"));
                None
            };
            run_check(id, messages, check, tools, "reasoning").await;
        }
    }
}

#[tokio::test]
async fn required_fields_are_empty_for_no_thinking_messages_foreign_and_redacted_thinking_are_not_replayed() {
    for id in ["deepseek-flash", "mimo-v2.6-flash"] {
        let mut redacted = earlier(id, true, &["first", "second"]);
        let mut content =
            vec![json!({ "type": "thinking", "thinking": "do not replay", "redacted": true, "signature": "opaque" })];
        content.extend(
            redacted["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| c["type"] != "thinking")
                .cloned(),
        );
        redacted["content"] = Value::Array(content);
        let foreign = assistant_msg("other-model", true, &["first", "second"]);
        let mut other_api = earlier(id, true, &["first", "second"]);
        other_api["api"] = json!("anthropic");
        let messages = vec![
            go(),
            assistant_msg(id, true, &[]),
            foreign,
            message(other_api),
            message(redacted),
        ];
        let check = |b: &Value| {
            let reasoning: Vec<&Value> = of_role(b, "assistant")
                .iter()
                .map(|m| &m["reasoning_content"])
                .collect();
            assert_eq!(reasoning, [&json!(""), &json!(""), &json!(""), &json!("")]);
            None
        };
        run_check(id, messages, check, vec![tool()], "reasoning_content").await;
    }
}

#[tokio::test]
async fn other_models_keep_the_standard_openai_shape_without_vendor_only_fields() {
    for id in [
        "gpt-5.5",
        "claude-sonnet-5-5",
        "mimo-v2.6-flash-other",
        "deepseek-v4.1-flash-other",
        "deepseek-chat",
    ] {
        let mut messages = vec![go(), first_second(id)];
        messages.extend(results());
        let check = move |b: &Value| {
            assert!(
                of_role(b, "assistant").iter().all(|m| !has(m, "reasoning_content")),
                "{id}"
            );
            None
        };
        run_check(id, messages, check, vec![tool()], "reasoning_content").await;
    }
}

#[tokio::test]
async fn a_strict_compatible_server_accepts_preserved_reasoning_instead_of_returning_missing_reasoning_400() {
    let id = "deepseek-flash";
    let mut messages = vec![go(), first_second(id)];
    messages.extend(results());
    let check = |b: &Value| {
        for m in of_role(b, "assistant") {
            if m["reasoning_content"] != "firstsecond" {
                let body =
                    json!({ "error": { "message": "missing reasoning_content", "type": "invalid_request_error" } });
                return Some(Reply::Status(400, body.to_string()));
            }
            assert_eq!(m["reasoning_content"], "firstsecond");
        }
        None
    };
    run_check(id, messages, check, vec![tool()], "reasoning_content").await;
}

#[tokio::test]
async fn a_turn_of_thinking_alone_is_kept_when_its_reasoning_is_sent_back_42() {
    for id in ["deepseek-v4-flash"] {
        let mut thinking = earlier(id, false, &["only thinking"]);
        thinking["stopReason"] = json!("max_tokens");
        let only: Vec<Value> = thinking["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["type"] == "thinking")
            .cloned()
            .collect();
        thinking["content"] = Value::Array(only);
        let check = |b: &Value| {
            let a = of_role(b, "assistant");
            assert_eq!(a.len(), 1);
            assert_eq!(a[0]["reasoning_content"], "only thinking");
            assert_eq!(a[0]["content"], "");
            None
        };
        run_check(
            id,
            vec![go(), message(thinking), go()],
            check,
            vec![tool()],
            "reasoning_content",
        )
        .await;
    }
}
