//! What each adapter keeps across a resumed session, negotiates, and reports
//! (`packages/ai/test/provider-contracts.test.ts`).

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use sasacode_ai::openai_chat::parse_chat_usage;
use sasacode_ai::replay::{new_reply, replay_scope};
use sasacode_ai::{
    AssistantMessage, INCOMPLETE_STREAM, Message, ModelInfo, Request, SamplingOptions, StopReason, ThinkingLevel,
    ToolSpec,
};
use serde_json::{Value, json};

fn go() -> Message {
    user("go")
}

fn tool() -> ToolSpec {
    tool_spec(json!({ "name": "read", "description": "Read", "parameters": { "type": "object", "properties": {} } }))
}

fn result() -> Message {
    message(json!({
        "role": "tool", "toolCallId": "c1", "toolName": "read", "content": [{ "type": "text", "text": "file" }],
        "isError": false, "timestamp": 0,
    }))
}

fn model_for(api: &str, provider: &str, base_url: &str) -> ModelInfo {
    model(json!({
        "api": api, "provider": provider, "baseUrl": base_url, "id": "test-contract", "reasoning": true,
        "contextWindow": 100000, "maxOutput": 1000, "price": { "input": 1, "output": 2, "cacheRead": 0.1, "cacheWrite": 1.25 },
    }))
}

fn mock_model(api: &str) -> ModelInfo {
    model_for(api, "test", "https://mock.invalid/v1")
}

fn router() -> ModelInfo {
    model_for("openai-chat", "openrouter", "https://openrouter.ai/api/v1")
}

/// `data:` events plus `[DONE]`.
fn events(events: &[Value]) -> Reply {
    Reply::Sse(sse(events, false))
}

fn chat(delta: Value, finish: Value) -> Value {
    json!({ "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] })
}

fn stop(delta: Value) -> Value {
    chat(delta, json!("stop"))
}

fn responses(items: &[Value], usage: Value) -> Vec<Value> {
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        out.push(json!({ "type": "response.output_item.added", "item": item, "output_index": i }));
        if item["type"] == "message" {
            out.push(json!({ "type": "response.output_text.delta", "item_id": item["id"], "output_index": i, "delta": item["content"][0]["text"] }));
        }
        out.push(json!({ "type": "response.output_item.done", "item": item, "output_index": i }));
    }
    let mut u = json!({ "input_tokens": 100, "output_tokens": 10, "input_tokens_details": { "cached_tokens": 40, "cache_write_tokens": 20 } });
    for (k, v) in usage.as_object().into_iter().flatten() {
        u[k] = v.clone();
    }
    out.push(json!({ "type": "response.completed", "response": { "usage": u } }));
    out
}

fn no_responses() -> Vec<Value> {
    responses(&[], json!({}))
}

/// A request to `m` answered by `reply(body, attempt)`: the reply, and every body sent.
async fn ask(
    m: &ModelInfo,
    messages: Vec<Message>,
    reply: impl Fn(&Value, usize) -> Reply + Send + Sync + 'static,
    extra: impl FnOnce(&mut Request),
) -> anyhow::Result<(AssistantMessage, Vec<Value>)> {
    let (fetch, sent) = fake(move |s, n| reply(&s.body, n));
    let provider = provider(&m.api);
    let mut req = Request {
        system: "sys".into(),
        api_key: Some("fake".into()),
        messages,
        tools: vec![tool()],
        ..request(m.clone(), fetch)
    };
    extra(&mut req);
    let (out, _) = run(provider.as_ref(), req).await?;
    Ok((out, bodies(&sent)))
}

fn none(_: &mut Request) {}

fn thinking(level: ThinkingLevel) -> impl FnOnce(&mut Request) {
    move |r| r.thinking = Some(level)
}

fn thinking_with(level: ThinkingLevel, extra_body: Value) -> impl FnOnce(&mut Request) {
    move |r| {
        r.thinking = Some(level);
        r.sampling = Some(SamplingOptions {
            extra_body: extra_body.as_object().cloned(),
            ..Default::default()
        });
    }
}

fn absent(v: &Value, key: &str) -> bool {
    v.get(key).is_none()
}

/// `toMatchObject`: every field of `want` is in `got`, equal.
fn matches(got: &Value, want: Value) -> bool {
    want.as_object()
        .is_some_and(|w| w.iter().all(|(k, v)| got.get(k) == Some(v)))
}

fn includes(list: &Value, item: &str) -> bool {
    list.as_array().is_some_and(|l| l.iter().any(|i| i == item))
}

/// The reply after a JSON session round trip.
fn resumed(out: &AssistantMessage) -> Message {
    Message::Assistant(serde_json::from_str(&serde_json::to_string(out).unwrap()).unwrap())
}

#[tokio::test]
async fn openrouter_keeps_consecutive_reasoning_details_exactly_prefers_them_to_display_text_and_uses_unified_effort() {
    let m = router();
    let details = json!([
        { "type": "reasoning.text", "text": "visible", "signature": "SIGNED", "index": 0 },
        { "type": "reasoning.encrypted", "data": "OPAQUE", "format": "anthropic-v1", "index": 1 },
    ]);
    let d = details.clone();
    let (first, _) = ask(
        &m,
        vec![go()],
        move |b, _| {
            assert_eq!(b["reasoning"], json!({ "effort": "high" }));
            assert!(absent(b, "reasoning_effort"));
            events(&[
                chat(json!({ "reasoning": "display", "reasoning_details": [d[0]] }), json!("")),
                chat(
                    json!({ "reasoning_details": [d[1]], "tool_calls": [{ "index": 0, "id": "c1", "function": { "name": "read", "arguments": "{}" } }] }),
                    json!("tool_calls"),
                ),
            ])
        },
        thinking(ThinkingLevel::High),
    )
    .await
    .unwrap();
    // A JSON session round trip retains opaque data and the provenance digest.
    let resumed = resumed(&first);
    ask(
        &m,
        vec![go(), resumed.clone(), result()],
        move |b, _| {
            assert_eq!(b["messages"][2]["reasoning_details"], details);
            assert!(absent(&b["messages"][2], "reasoning"));
            assert!(absent(&b["messages"][2], "reasoning_content"));
            events(&[stop(json!({ "content": "done" }))])
        },
        none,
    )
    .await
    .unwrap();
    let mut other_id = m.clone();
    other_id.id = "other".into();
    let mut other_provider = m.clone();
    other_provider.provider = "other".into();
    for changed in [
        model_for("openai-chat", "openrouter", "https://proxy.invalid/v1"),
        other_id,
        other_provider,
    ] {
        let (_, bodies) = ask(
            &changed,
            vec![go(), resumed.clone(), result()],
            |_, _| events(&[stop(json!({ "content": "done" }))]),
            none,
        )
        .await
        .unwrap();
        let sent = bodies[0].to_string();
        assert!(!sent.contains("OPAQUE"), "{sent}");
        assert!(!sent.contains("SIGNED"), "{sent}");
    }
}

#[tokio::test]
async fn responses_preserve_every_opaque_reasoning_item_and_each_message_phase_in_sequence() {
    let m = mock_model("openai-responses");
    let items = [
        json!({ "type": "reasoning", "id": "rs1", "summary": [], "encrypted_content": "FIRST" }),
        json!({ "type": "message", "id": "msg1", "role": "assistant", "phase": "commentary", "content": [{ "type": "output_text", "text": "checking", "annotations": [] }] }),
        json!({ "type": "function_call", "id": "fc1", "call_id": "c1", "name": "read", "arguments": "{}" }),
        json!({ "type": "reasoning", "id": "rs2", "summary": [], "encrypted_content": "SECOND" }),
        json!({ "type": "message", "id": "msg2", "role": "assistant", "phase": "final_answer", "content": [{ "type": "output_text", "text": "answer", "annotations": [] }] }),
    ];
    let all = items.to_vec();
    let (first, first_bodies) = ask(
        &m,
        vec![go()],
        move |b, _| {
            assert_eq!(b["store"], json!(false));
            assert!(includes(&b["include"], "reasoning.encrypted_content"));
            assert!(matches(&b["reasoning"], json!({ "effort": "high", "summary": "detailed" })), "{}", b["reasoning"]);
            events(&responses(&all, json!({ "cost": 0.01 })))
        },
        thinking_with(
            ThinkingLevel::High,
            json!({ "reasoning": { "summary": "detailed", "context": "all_turns" }, "include": ["message.output_text.logprobs"] }),
        ),
    )
    .await
    .unwrap();
    // Provider fields are merged with extraBody, rather than silently removing caller controls.
    assert!(matches(
        &first_bodies[0]["reasoning"],
        json!({ "summary": "detailed", "context": "all_turns" })
    ));
    assert!(includes(&first_bodies[0]["include"], "message.output_text.logprobs"));
    let usage = serde_json::to_value(&first.usage).unwrap();
    assert!(
        matches(
            &usage,
            json!({ "input": 40, "cacheRead": 40, "cacheWrite": 20, "cost": 0.01 })
        ),
        "{usage}"
    );
    let resumed = resumed(&first);
    let (_, second) = ask(
        &m,
        vec![go(), resumed.clone(), result()],
        |_, _| events(&no_responses()),
        none,
    )
    .await
    .unwrap();
    assert_eq!(
        Value::Array(second[0]["input"].as_array().unwrap()[1..6].to_vec()),
        json!([
            items[0], { "role": "assistant", "content": "checking", "phase": "commentary" },
            { "type": "function_call", "call_id": "c1", "name": "read", "arguments": "{}" },
            items[3], { "role": "assistant", "content": "answer", "phase": "final_answer" },
        ])
    );
    let mut elsewhere = m.clone();
    elsewhere.base_url = Some("https://other.invalid/v1".into());
    let (_, other) = ask(
        &elsewhere,
        vec![go(), resumed, result()],
        |_, _| events(&no_responses()),
        none,
    )
    .await
    .unwrap();
    let sent = Value::Array(other).to_string();
    assert!(!sent.contains("FIRST"), "{sent}");
    assert!(!sent.contains("SECOND"), "{sent}");
}

#[tokio::test]
async fn old_sessions_without_endpoint_provenance_never_forward_opaque_state() {
    for api in ["anthropic", "openai-responses", "openai-chat"] {
        let m = if api == "openai-chat" {
            router()
        } else {
            model_for(api, "test", "https://mock.invalid/v1")
        };
        let mut earlier = new_reply(&m);
        earlier.replay_scope = None;
        let signature = match api {
            "anthropic" => "OLD_SIGNATURE",
            "openai-chat" => r#"[{"type":"reasoning.encrypted","data":"OLD_SIGNATURE"}]"#,
            _ => r#"{"type":"reasoning","id":"old","encrypted_content":"OLD_SIGNATURE"}"#,
        };
        earlier.content = serde_json::from_value(json!([
            { "type": "thinking", "thinking": "OLD_REASONING", "signature": signature, "redacted": api == "openai-chat" },
            { "type": "text", "text": "keep visible text" },
        ]))
        .unwrap();
        let id = m.id.clone();
        let (fetch, _) = fake(move |s, _| {
            assert!(!s.raw.contains("OLD_SIGNATURE"), "{api}");
            assert!(!s.raw.contains("OLD_REASONING"), "{api}");
            assert!(s.raw.contains("keep visible text"), "{api}");
            match api {
                "anthropic" => Reply::Sse(sse(
                    &[
                        json!({ "type": "message_start", "message": { "id": "msg", "type": "message", "role": "assistant", "model": id, "content": [], "stop_reason": null, "usage": { "input_tokens": 1, "output_tokens": 0 } } }),
                        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 0 } }),
                        json!({ "type": "message_stop" }),
                    ],
                    true,
                )),
                "openai-chat" => events(&[stop(json!({ "content": "ok" }))]),
                _ => events(&no_responses()),
            }
        });
        let req = Request {
            api_key: Some("fake".into()),
            system: "sys".into(),
            messages: vec![go(), Message::Assistant(earlier), go()],
            ..request(m.clone(), fetch)
        };
        run(provider(api).as_ref(), req).await.unwrap();
        assert_eq!(replay_scope(&m).len(), 64);
    }
}

#[tokio::test]
async fn local_chat_aliases_replay_their_observed_reasoning_field_ollama_uses_supported_max_tokens() {
    for field in ["reasoning", "reasoning_content"] {
        let mut m = model_for("openai-chat", "ollama", "http://localhost:11434/v1");
        m.server = Some(sasacode_ai::LocalServer::Ollama);
        let (first, _) = ask(
            &m,
            vec![go()],
            move |b, _| {
                assert_eq!(b["max_tokens"], json!(1000));
                assert!(absent(b, "max_completion_tokens"));
                events(&[stop(json!({ field: "thought", "content": "ok" }))])
            },
            none,
        )
        .await
        .unwrap();
        ask(
            &m,
            vec![go(), Message::Assistant(first), go()],
            move |b, _| {
                assert_eq!(b["messages"][2][field], "thought");
                let other = if field == "reasoning" {
                    "reasoning_content"
                } else {
                    "reasoning"
                };
                assert!(absent(&b["messages"][2], other));
                events(&[stop(json!({ "content": "ok" }))])
            },
            none,
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn responses_negotiate_a_first_event_effort_error_never_a_later_error_or_history_rejection() {
    let m = mock_model("openai-responses");
    let (_, bodies) = ask(
        &m,
        vec![go()],
        |_, n| {
            if n == 1 {
                events(&[json!({ "type": "error", "code": "400", "message": "reasoning effort xhigh unsupported", "param": "reasoning.effort" })])
            } else {
                events(&no_responses())
            }
        },
        thinking(ThinkingLevel::Xhigh),
    )
    .await
    .unwrap();
    let efforts: Vec<&Value> = bodies.iter().map(|b| &b["reasoning"]["effort"]).collect();
    assert_eq!(efforts, [&json!("xhigh"), &json!("high")]);
    let created = json!({ "type": "response.created", "response": { "id": "started", "output": [] } });
    for before in [vec![], vec![created]] {
        let calls = Arc::new(Mutex::new(0));
        let counted = calls.clone();
        let message = if before.is_empty() {
            "messages[1].reasoning_content is missing for effort high"
        } else {
            "reasoning effort unsupported"
        };
        let mut errored = m.clone();
        errored.id = format!("error-{}", before.len());
        let result = ask(
            &errored,
            vec![go()],
            move |_, _| {
                *counted.lock().unwrap() += 1;
                let mut all = before.clone();
                all.push(json!({ "type": "error", "code": "400", "message": message, "param": "reasoning.effort" }));
                events(&all)
            },
            thinking(ThinkingLevel::High),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(*calls.lock().unwrap(), 1);
    }
}

#[tokio::test]
async fn refusal_text_survives_chat_and_responses_streaming() {
    let (a, _) = ask(
        &mock_model("openai-chat"),
        vec![go()],
        |_, _| events(&[stop(json!({ "refusal": "cannot help" }))]),
        none,
    )
    .await
    .unwrap();
    assert_eq!(a.stop_reason, StopReason::Refusal);
    assert_eq!(content(&a), json!([{ "type": "text", "text": "cannot help" }]));
    let (b, _) = ask(
        &mock_model("openai-responses"),
        vec![go()],
        |_, _| {
            let mut all = vec![
                json!({ "type": "response.output_item.added", "item": { "type": "message", "id": "refusal", "role": "assistant", "content": [] } }),
                json!({ "type": "response.refusal.delta", "item_id": "refusal", "delta": "cannot help" }),
            ];
            all.extend(no_responses());
            events(&all)
        },
        none,
    )
    .await
    .unwrap();
    assert_eq!(b.stop_reason, StopReason::Refusal);
    assert_eq!(content(&b), json!([{ "type": "text", "text": "cannot help" }]));
}

#[tokio::test]
async fn cache_write_counts_are_disjoint_from_fresh_read_input_native_deepseek_aliases_and_reported_spend_remain_exact()
{
    let m = mock_model("openai-chat");
    let usage = |u: Value| serde_json::to_value(parse_chat_usage(&m, &u)).unwrap();
    let a = usage(
        json!({ "prompt_tokens": 100, "completion_tokens": 10, "prompt_tokens_details": { "cached_tokens": 40, "cache_write_tokens": 20 }, "cost": 0.01 }),
    );
    assert!(
        matches(
            &a,
            json!({ "input": 40, "cacheRead": 40, "cacheWrite": 20, "cost": 0.01 })
        ),
        "{a}"
    );
    let b = usage(json!({ "prompt_cache_hit_tokens": 70, "prompt_cache_miss_tokens": 30, "completion_tokens": 5 }));
    assert!(
        matches(&b, json!({ "input": 30, "cacheRead": 70, "cacheWrite": 0 })),
        "{b}"
    );
    let (r, _) = ask(
        &m,
        vec![go()],
        |_, _| {
            events(&[
                json!({ "choices": [], "usage": { "prompt_tokens": 100, "completion_tokens": 10, "cost": 0.01 } }),
            ])
        },
        none,
    )
    .await
    .unwrap();
    assert_eq!(r.usage.cost, 0.01);
}

/// The TS set `OPENAI_BASE_URL` in its own process. Setting a variable is `unsafe` in edition 2024, so
/// the test runs itself again as a child process with the variable set, and the body runs there.
#[tokio::test]
async fn implicit_sdk_endpoint_environment_cannot_invalidate_replay_provenance() {
    const NAME: &str = "implicit_sdk_endpoint_environment_cannot_invalidate_replay_provenance";
    const CHILD: &str = "SASACODE_AI_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--test-threads=1"])
            .env(CHILD, "1")
            .env("OPENAI_BASE_URL", "https://unrelated.invalid/v1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
        assert!(stdout.contains("1 passed"), "{stdout}");
        return;
    }
    assert_eq!(
        std::env::var("OPENAI_BASE_URL").as_deref(),
        Ok("https://unrelated.invalid/v1")
    );
    let mut m = mock_model("openai-chat");
    m.base_url = None;
    let (fetch, sent) = fake(|s, _| {
        assert!(s.url.starts_with("https://api.openai.com/v1/"), "{}", s.url);
        events(&[stop(json!({ "content": "ok" }))])
    });
    let req = Request {
        api_key: Some("fake".into()),
        system: String::new(),
        messages: vec![go()],
        tools: vec![],
        max_retries: None,
        ..request(m, fetch)
    };
    run(provider("openai-chat").as_ref(), req).await.unwrap();
    assert_eq!(sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn explicit_plugin_effort_controls_are_preserved_and_do_not_trigger_repeated_unchanged_negotiation() {
    let cases = [
        (
            mock_model("openai-responses"),
            json!({ "reasoning": { "effort": "medium", "context": "current_turn" } }),
        ),
        (
            router(),
            json!({ "reasoning": { "effort": "medium", "exclude": true } }),
        ),
        (mock_model("openai-chat"), json!({ "reasoning_effort": "medium" })),
    ];
    for (m, extra) in cases {
        let calls = Arc::new(Mutex::new(0));
        let counted = calls.clone();
        let result = ask(
            &m,
            vec![go()],
            move |b, _| {
                *counted.lock().unwrap() += 1;
                let effort = b.get("reasoning").and_then(|r| r.get("effort")).or(b.get("reasoning_effort"));
                assert_eq!(effort, Some(&json!("medium")));
                Reply::Status(
                    400,
                    json!({ "error": { "message": "reasoning effort medium unsupported", "type": "invalid_request_error" } })
                        .to_string(),
                )
            },
            thinking_with(ThinkingLevel::High, extra),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(*calls.lock().unwrap(), 1);
    }
}

#[tokio::test]
async fn deepseek_and_mimo_plain_replies_do_not_gain_reasoning_fields_from_the_local_server_replay_path() {
    for id in ["deepseek-flash", "mimo-v2.6-flash"] {
        let mut m = mock_model("openai-chat");
        m.id = id.into();
        let no_tools = |r: &mut Request| r.tools = vec![];
        let (first, _) = ask(
            &m,
            vec![go()],
            |_, _| events(&[stop(json!({ "reasoning_content": "thought", "content": "ok" }))]),
            no_tools,
        )
        .await
        .unwrap();
        ask(
            &m,
            vec![go(), Message::Assistant(first), go()],
            |b, _| {
                assert!(absent(&b["messages"][2], "reasoning_content"));
                events(&[stop(json!({ "content": "ok" }))])
            },
            no_tools,
        )
        .await
        .unwrap();
    }
}

/// The TS told the two apart by the error's message; the port by its kind: an io or hyper error under
/// the body read is a broken connection, any other error the body stream raised is thrown.
#[tokio::test]
async fn a_connection_that_breaks_mid_reply_is_a_cut_reply_to_ask_again_for_other_stream_errors_are_thrown() {
    fn bun() -> Box<dyn std::error::Error + Send + Sync> {
        Box::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()",
        ))
    }
    fn boom() -> Box<dyn std::error::Error + Send + Sync> {
        "boom".into()
    }
    let chat_first = format!("data: {}\n\n", chat(json!({ "content": "Hel" }), Value::Null));
    let responses_first = format!(
        "data: {}\n\n",
        json!({ "type": "response.output_item.added", "item": { "type": "message", "id": "m1", "role": "assistant", "content": [] }, "output_index": 0 })
    );
    let start = json!({ "type": "message_start", "message": { "id": "x", "type": "message", "role": "assistant", "model": "m", "content": [], "stop_reason": null, "usage": { "input_tokens": 5, "output_tokens": 0 } } });
    let anthropic_first = format!("event: message_start\ndata: {start}\n\n");
    for (api, first) in [
        ("openai-chat", chat_first),
        ("openai-responses", responses_first),
        ("anthropic", anthropic_first),
    ] {
        let f = first.clone();
        let (out, _) = ask(
            &mock_model(api),
            vec![go()],
            move |_, _| Reply::Breaking(f.clone(), bun()),
            none,
        )
        .await
        .unwrap();
        assert_eq!(out.stop_reason, StopReason::Error, "{api}");
        assert_eq!(out.error_message.as_deref(), Some(INCOMPLETE_STREAM), "{api}");
        let err = ask(
            &mock_model(api),
            vec![go()],
            move |_, _| Reply::Breaking(first.clone(), boom()),
            none,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("boom"), "{api}: {err}");
    }
}
