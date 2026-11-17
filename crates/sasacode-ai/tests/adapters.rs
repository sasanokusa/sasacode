//! The three adapters, without a model: a fake transport records what is sent and replays a recorded
//! stream. Covers the conversion of the history and the parsing of the stream
//! (`packages/ai/test/adapters.test.ts`).

mod common;

use common::*;
use sasacode_ai::anthropic::AnthropicProvider;
use sasacode_ai::openai_chat::OpenAiChatProvider;
use sasacode_ai::openai_responses::OpenAiResponsesProvider;
use sasacode_ai::replay::replay_scope;
use sasacode_ai::transport::ApiError;
use sasacode_ai::{INCOMPLETE_STREAM, Message, ModelInfo, StopReason, StreamEvent, ToolSpec};
use serde_json::{Value, json};

fn png() -> Value {
    json!({ "type": "image", "mediaType": "image/png", "data": "iVBORw0KGgo=" })
}

fn assistant_of(model: &ModelInfo, content: Value) -> Message {
    message(json!({
        "role": "assistant", "content": content, "replayScope": replay_scope(model), "api": model.api,
        "provider": model.provider, "model": model.id,
        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "cost": 0 },
        "stopReason": "tool_use", "timestamp": 0,
    }))
}

fn history(model: &ModelInfo) -> Vec<Message> {
    let signature = if model.api == "openai-responses" {
        json!({ "type": "reasoning", "id": "rs0", "summary": [] }).to_string()
    } else {
        "sig".to_string()
    };
    vec![
        user("look at a.png"),
        assistant_of(
            model,
            json!([
                { "type": "thinking", "thinking": "hmm", "signature": signature },
                { "type": "text", "text": "reading" },
                { "type": "tool_call", "id": "c1", "name": "read", "input": { "path": "a.png" } },
            ]),
        ),
        message(json!({
            "role": "tool", "toolCallId": "c1", "toolName": "read",
            "content": [{ "type": "text", "text": "Image a.png" }, png()], "isError": false, "timestamp": 0,
        })),
        user("and now?"),
    ]
}

fn tools() -> Vec<ToolSpec> {
    vec![tool_spec(json!({
        "name": "read", "description": "read a file",
        "parameters": { "type": "object", "properties": { "path": { "type": "string" } } },
    }))]
}

#[tokio::test]
async fn openai_chat_tool_results_are_tool_messages_their_images_follow_as_a_user_message_stream_parsed() {
    let model = model(json!({
        "id": "gpt-x", "provider": "openai", "api": "openai-chat", "baseUrl": "http://x/v1",
        "contextWindow": 100000, "maxOutput": 1000, "price": { "input": 1, "output": 2 },
    }));
    let (fetch, sent) = fixed(|| {
        Reply::Sse(sse(
            &[
                chat_chunk(json!({ "role": "assistant", "reasoning_content": "think" }), None),
                chat_chunk(json!({ "content": "Hel" }), None),
                chat_chunk(json!({ "content": "lo" }), None),
                chat_chunk(
                    json!({ "tool_calls": [{ "index": 0, "id": "c2", "type": "function", "function": { "name": "read", "arguments": "{\"pa" } }] }),
                    None,
                ),
                chat_chunk(
                    json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "th\":\"b\"}" } }] }),
                    Some("tool_calls"),
                ),
                json!({ "id": "1", "object": "chat.completion.chunk", "created": 0, "model": "gpt-x", "choices": [],
                        "usage": { "prompt_tokens": 100, "completion_tokens": 20, "prompt_tokens_details": { "cached_tokens": 40 } } }),
            ],
            false,
        ))
    });
    let req = sasacode_ai::Request {
        system: "sys".into(),
        messages: history(&model),
        tools: tools(),
        ..request(model.clone(), fetch)
    };
    let (message, _) = run(&OpenAiChatProvider, req).await.unwrap();
    let body = &bodies(&sent)[0];
    let roles: Vec<&str> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["system", "user", "assistant", "tool", "user", "user"]);
    assert_eq!(
        body["messages"][2]["tool_calls"][0]["function"],
        json!({ "name": "read", "arguments": "{\"path\":\"a.png\"}" })
    );
    assert!(
        body["messages"][4]["content"][1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    assert_eq!(
        content(&message),
        json!([
            { "type": "thinking", "thinking": "think", "reasoningField": "reasoning_content" },
            { "type": "text", "text": "Hello" },
            { "type": "tool_call", "id": "c2", "name": "read", "input": { "path": "b" } },
        ])
    );
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(
        (message.usage.input, message.usage.cache_read, message.usage.output),
        (60, 40, 20)
    );
}

#[tokio::test]
async fn openai_chat_a_context_overflow_error_becomes_context_overflow_error_a_truncated_reply_is_max_tokens() {
    let model = model(json!({
        "id": "m", "provider": "p", "api": "openai-chat", "baseUrl": "http://x/v1", "contextWindow": 100000, "maxOutput": 1000,
    }));
    let (fetch, _) = fixed(|| {
        Reply::Status(
            400,
            json!({ "error": { "message": "This model's maximum context length is 8192 tokens", "code": "context_length_exceeded" } })
                .to_string(),
        )
    });
    let err = run(&OpenAiChatProvider, request(model.clone(), fetch))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("maximum context length"), "{err}");
    assert!(sasacode_ai::is_context_overflow(&err));
    let (cut, _) = fixed(|| {
        Reply::Sse(sse(
            &[
                json!({ "id": "1", "object": "chat.completion.chunk", "created": 0, "model": "m",
                      "choices": [{ "index": 0, "delta": { "content": "par" }, "finish_reason": "length" }] }),
            ],
            false,
        ))
    });
    let (message, _) = run(&OpenAiChatProvider, request(model, cut)).await.unwrap();
    assert_eq!(message.stop_reason, StopReason::MaxTokens);
}

#[tokio::test]
async fn openai_responses_history_as_input_items_reasoning_text_and_function_calls_parsed() {
    let model = model(json!({
        "id": "gpt-r", "provider": "openai", "api": "openai-responses", "baseUrl": "http://x/v1",
        "contextWindow": 100000, "maxOutput": 1000, "reasoning": true,
    }));
    let (fetch, sent) = fixed(|| {
        Reply::Sse(sse(
            &[
                json!({ "type": "response.created", "sequence_number": 0, "response": { "id": "r", "output": [] } }),
                json!({ "type": "response.output_item.added", "sequence_number": 1, "output_index": 0, "item": { "type": "reasoning", "id": "rs", "summary": [] } }),
                json!({ "type": "response.reasoning_summary_text.delta", "sequence_number": 2, "item_id": "rs", "output_index": 0, "summary_index": 0, "delta": "plan" }),
                json!({ "type": "response.output_item.done", "sequence_number": 3, "output_index": 0, "item": { "type": "reasoning", "id": "rs", "summary": [{ "type": "summary_text", "text": "plan" }] } }),
                json!({ "type": "response.output_item.added", "sequence_number": 4, "output_index": 1, "item": { "type": "message", "id": "m1", "role": "assistant", "content": [] } }),
                json!({ "type": "response.output_text.delta", "sequence_number": 5, "item_id": "m1", "output_index": 1, "content_index": 0, "delta": "ok" }),
                json!({ "type": "response.output_item.added", "sequence_number": 6, "output_index": 2, "item": { "type": "function_call", "id": "f1", "call_id": "c3", "name": "read", "arguments": "" } }),
                json!({ "type": "response.function_call_arguments.delta", "sequence_number": 7, "item_id": "f1", "output_index": 2, "delta": "{\"path\":\"c\"}" }),
                json!({ "type": "response.output_item.done", "sequence_number": 8, "output_index": 2, "item": { "type": "function_call", "id": "f1", "call_id": "c3", "name": "read", "arguments": "{\"path\":\"c\"}" } }),
                json!({ "type": "response.completed", "sequence_number": 9, "response": { "id": "r", "output": [], "usage": { "input_tokens": 50, "output_tokens": 7, "input_tokens_details": { "cached_tokens": 10 } } } }),
            ],
            true,
        ))
    });
    let req = sasacode_ai::Request {
        system: "sys".into(),
        messages: history(&model),
        tools: tools(),
        thinking: Some(sasacode_ai::ThinkingLevel::High),
        ..request(model.clone(), fetch)
    };
    let (message, _) = run(&OpenAiResponsesProvider, req).await.unwrap();
    let body = &bodies(&sent)[0];
    let types: Vec<String> = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            i.get("type")
                .or(i.get("role"))
                .and_then(Value::as_str)
                .unwrap()
                .to_string()
        })
        .collect();
    assert!(types.contains(&"reasoning".to_string()), "{types:?}");
    assert!(types.contains(&"function_call".to_string()));
    assert!(types.contains(&"function_call_output".to_string()));
    let all = content(&message);
    assert_eq!(all[0]["type"], "thinking");
    assert_eq!(all[0]["thinking"], "plan");
    assert_eq!(
        Value::Array(all.as_array().unwrap()[1..].to_vec()),
        json!([
            { "type": "text", "text": "ok" },
            { "type": "tool_call", "id": "c3", "name": "read", "input": { "path": "c" } },
        ])
    );
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(
        (message.usage.input, message.usage.cache_read, message.usage.output),
        (40, 10, 7)
    );
}

#[tokio::test]
async fn anthropic_same_role_messages_merged_tool_result_images_kept_own_thinking_replayed_stream_parsed() {
    let model = model(json!({
        "id": "claude-sonnet-4-6", "provider": "anthropic", "api": "anthropic", "baseUrl": "http://x",
        "contextWindow": 100000, "maxOutput": 1000, "reasoning": true,
    }));
    let (fetch, sent) = fixed(move || {
        Reply::Sse(sse(
            &[
                json!({ "type": "message_start", "message": { "id": "x", "type": "message", "role": "assistant", "model": "claude-sonnet-4-6", "content": [], "stop_reason": null, "usage": { "input_tokens": 30, "output_tokens": 0, "cache_read_input_tokens": 5 } } }),
                json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "", "signature": "" } }),
                json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "t" } }),
                json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "signature_delta", "signature": "S" } }),
                json!({ "type": "content_block_stop", "index": 0 }),
                json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "tool_use", "id": "c4", "name": "read", "input": {} } }),
                json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": "{\"path\":\"d\"}" } }),
                json!({ "type": "content_block_stop", "index": 1 }),
                json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 9 } }),
                json!({ "type": "message_stop" }),
            ],
            true,
        ))
    });
    let req = sasacode_ai::Request {
        api_key: Some("k".into()),
        system: "sys".into(),
        messages: history(&model),
        tools: tools(),
        ..request(model.clone(), fetch)
    };
    let (message, _) = run(&AnthropicProvider, req).await.unwrap();
    let msgs = bodies(&sent)[0]["messages"].clone();
    let roles: Vec<&str> = msgs
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["user", "assistant", "user"]);
    assert_eq!(
        msgs[1]["content"][0],
        json!({ "type": "thinking", "thinking": "hmm", "signature": "sig" })
    );
    assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
    assert_eq!(msgs[2]["content"][0]["content"][1]["type"], "image");
    assert_eq!(msgs[2]["content"][1], json!({ "type": "text", "text": "and now?" }));
    assert_eq!(
        content(&message),
        json!([
            { "type": "thinking", "thinking": "t", "signature": "S" },
            { "type": "tool_call", "id": "c4", "name": "read", "input": { "path": "d" } },
        ])
    );
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(
        (message.usage.input, message.usage.cache_read, message.usage.output),
        (30, 5, 9)
    );
}

#[tokio::test]
async fn a_stream_cut_before_the_reply_finished_is_an_error_with_what_arrived_not_a_finished_reply_all_three_formats() {
    let chunk = |delta: Value| chat_chunk(delta, None);
    let chat = model(json!({
        "id": "m", "provider": "p", "api": "openai-chat", "baseUrl": "http://x/v1", "contextWindow": 100000, "maxOutput": 1000,
    }));
    // Thinking, then nothing: no finish reason, no usage chunk, no [DONE].
    let cut_body = sse(&[chunk(json!({ "reasoning_content": "let me see" }))], false).replace("data: [DONE]\n\n", "");
    let (cut_chat, _) = fixed(move || Reply::Sse(cut_body.clone()));
    let (m1, _) = run(&OpenAiChatProvider, request(chat.clone(), cut_chat)).await.unwrap();
    assert_eq!(m1.stop_reason, StopReason::Error);
    assert_eq!(m1.error_message.as_deref(), Some(INCOMPLETE_STREAM));
    assert_eq!(
        content(&m1),
        json!([{ "type": "thinking", "thinking": "let me see", "reasoningField": "reasoning_content" }])
    );
    // A server that never sends finish_reason but does send the usage chunk has finished.
    let (usage_only, _) = fixed(move || {
        let mut usage = chunk(json!({}));
        usage["choices"] = json!([]);
        usage["usage"] = json!({ "prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4 });
        Reply::Sse(sse(&[chunk(json!({ "content": "hi" })), usage], false))
    });
    assert_eq!(
        run(&OpenAiChatProvider, request(chat, usage_only))
            .await
            .unwrap()
            .0
            .stop_reason,
        StopReason::Stop
    );

    let responses = model(json!({
        "id": "gpt-r", "provider": "openai", "api": "openai-responses", "baseUrl": "http://x/v1",
        "contextWindow": 100000, "maxOutput": 1000,
    }));
    let (cut_responses, _) = fixed(|| {
        Reply::Sse(sse(
            &[
                json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "message", "id": "m1", "role": "assistant", "content": [] } }),
                json!({ "type": "response.output_text.delta", "item_id": "m1", "output_index": 0, "content_index": 0, "delta": "par" }),
            ],
            true,
        ))
    });
    let (m2, _) = run(&OpenAiResponsesProvider, request(responses, cut_responses))
        .await
        .unwrap();
    assert_eq!(
        (m2.stop_reason, m2.error_message.as_deref()),
        (StopReason::Error, Some(INCOMPLETE_STREAM))
    );

    let claude = model(json!({
        "id": "claude-x", "provider": "anthropic", "api": "anthropic", "baseUrl": "http://x", "contextWindow": 100000, "maxOutput": 1000,
    }));
    let (cut_claude, _) = fixed(|| {
        Reply::Sse(sse(
            &[
                json!({ "type": "message_start", "message": { "id": "m", "type": "message", "role": "assistant", "model": "claude-x", "content": [], "stop_reason": null, "usage": { "input_tokens": 5, "output_tokens": 0 } } }),
                json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
                json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "par" } }),
            ],
            true,
        ))
    });
    let req = sasacode_ai::Request {
        api_key: Some("k".into()),
        ..request(claude, cut_claude)
    };
    let (m3, _) = run(&AnthropicProvider, req).await.unwrap();
    assert_eq!(
        (m3.stop_reason, m3.error_message.as_deref()),
        (StopReason::Error, Some(INCOMPLETE_STREAM))
    );
    assert_eq!(content(&m3), json!([{ "type": "text", "text": "par" }]));
}

fn req_for(model: ModelInfo, fetch: std::sync::Arc<dyn sasacode_ai::provider::Fetch>) -> sasacode_ai::Request {
    sasacode_ai::Request {
        tools: tools(),
        ..request(model, fetch)
    }
}

#[tokio::test]
async fn each_servers_context_overflow_is_context_overflow_error_llama_cpp_s_included_57() {
    let err =
        |error: &Value| -> anyhow::Error { ApiError::openai_status(400, Some(json!({ "error": error })), None).into() };
    // As llama-server b9860 answered a prompt longer than its slot; then the wording of llama.cpp PR 15780.
    let llama = json!({ "code": 400, "message": "request (391 tokens) exceeds the available context size (256 tokens), try increasing it", "type": "exceed_context_size_error", "n_prompt_tokens": 391, "n_ctx": 256 });
    let overflows = [
        llama.clone(),
        json!({ "code": 400, "message": "the request exceeds the available context size. try increasing the context size or enable context shift", "type": "exceed_context_size_error" }),
        json!({ "code": 400, "message": "input (391 tokens) is larger than the max context size (256 tokens). skipping", "type": "exceed_context_size_error" }),
        json!({ "code": 400, "message": "prompt too long", "type": "exceed_context_size_error" }),
        // The wording alone, for a build or a proxy that drops the type.
        json!({ "code": 400, "message": "the request exceeds the available context size. try increasing the context size or enable context shift" }),
        json!({ "message": "This model's maximum context length is 8192 tokens. However, you requested 9000 tokens", "type": "BadRequestError", "code": 400 }), // vLLM
        json!({ "message": "The number of tokens to keep from the initial prompt is greater than the context length" }), // LM Studio
        json!({ "message": "Your input exceeds the context window of this model.", "code": "context_length_exceeded" }), // OpenAI
    ];
    for error in &overflows {
        assert!(
            sasacode_ai::openai_chat::is_context_overflow(&err(error)),
            "{}",
            error["message"]
        );
    }
    assert!(!sasacode_ai::openai_chat::is_context_overflow(&err(
        &json!({ "message": "Unsupported value: 'reasoning_effort'", "type": "invalid_request_error" })
    )));
    // Both OpenAI-style adapters turn llama-server's 400 into the error the agent compacts on.
    let chat = model(json!({
        "id": "m", "provider": "llama", "api": "openai-chat", "baseUrl": "http://x/v1", "contextWindow": 256, "maxOutput": 100, "server": "llamacpp",
    }));
    let mut responses = chat.clone();
    responses.api = "openai-responses".into();
    let body = json!({ "error": llama }).to_string();
    for (provider, model) in [
        (&OpenAiChatProvider as &dyn sasacode_ai::Provider, chat.clone()),
        (&OpenAiResponsesProvider, responses),
    ] {
        let body = body.clone();
        let (fetch, _) = fixed(move || Reply::Status(400, body.clone()));
        let e = run(provider, req_for(model, fetch)).await.unwrap_err();
        assert!(sasacode_ai::is_context_overflow(&e), "{e}");
    }
    // The same error as the first event of a 200 stream, where the error has no status: by its type, and by its
    // wording alone.
    for error in [llama.clone(), json!({ "message": overflows[4]["message"] })] {
        let (fetch, _) = fixed(move || Reply::Sse(sse(&[json!({ "error": error })], false)));
        let e = run(&OpenAiChatProvider, req_for(chat.clone(), fetch))
            .await
            .unwrap_err();
        assert!(sasacode_ai::is_context_overflow(&e), "{e}");
    }
}

#[tokio::test]
async fn openai_chat_a_local_server_gets_max_tokens_a_cloud_provider_keeps_max_completion_tokens_58() {
    async fn sent(model: ModelInfo) -> (Value, Value) {
        let (fetch, sent) = fixed(|| Reply::Sse(sse(&[chat_chunk(json!({ "content": "ok" }), Some("stop"))], false)));
        let req = sasacode_ai::Request {
            max_tokens: Some(500),
            ..req_for(model, fetch)
        };
        run(&OpenAiChatProvider, req).await.unwrap();
        let body = &bodies(&sent)[0];
        (
            body.get("max_tokens").cloned().unwrap_or(Value::Null),
            body.get("max_completion_tokens").cloned().unwrap_or(Value::Null),
        )
    }
    let base = |provider: &str, base_url: Option<&str>, server: Option<&str>| {
        let mut m = json!({ "id": "m", "api": "openai-chat", "contextWindow": 100000, "maxOutput": 1000, "provider": provider });
        if let Some(b) = base_url {
            m["baseUrl"] = json!(b);
        }
        if let Some(s) = server {
            m["server"] = json!(s);
        }
        model(m)
    };
    let cloud = [
        ("openai", None),
        ("openrouter", Some("https://openrouter.ai/api/v1")),
        ("commandcode", Some("https://api.commandcode.ai/provider/v1")),
        ("vllm", Some("http://gpu:8000/v1")),
    ];
    for (provider, base_url) in cloud {
        assert_eq!(
            sent(base(provider, base_url, None)).await,
            (Value::Null, json!(500)),
            "{provider}"
        );
    }
    for server in ["ollama", "llamacpp", "lmstudio"] {
        assert_eq!(
            sent(base("myollama", Some("http://host:11434/v1"), Some(server))).await,
            (json!(500), Value::Null),
            "{server}"
        );
    }
}

#[tokio::test]
async fn openai_chat_a_tool_name_sent_after_the_id_and_tool_calls_without_an_index_are_kept_apart_and_named_62() {
    let model = model(json!({
        "id": "m", "provider": "local", "api": "openai-chat", "baseUrl": "http://x/v1", "contextWindow": 100000, "maxOutput": 1000,
    }));
    let tc = |calls: Value, finish: Option<&str>| chat_chunk(json!({ "tool_calls": calls }), finish);
    let f = Some("tool_calls");
    /// The calls a stream should give: (id, name, input).
    type Calls = Vec<(Option<&'static str>, &'static str, Value)>;
    let streams: Vec<(&str, Vec<Value>, Calls)> = vec![
        // The OpenAI shape: the name in the first chunk, an index on every chunk. Unchanged.
        (
            "openai",
            vec![
                tc(
                    json!([{ "index": 0, "id": "a", "type": "function", "function": { "name": "read", "arguments": "{\"path\":" } }]),
                    None,
                ),
                tc(json!([{ "index": 0, "function": { "arguments": "\"a\"}" } }]), None),
                tc(
                    json!([{ "index": 1, "id": "b", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"b\"}" } }]),
                    f,
                ),
            ],
            vec![
                (Some("a"), "read", json!({ "path": "a" })),
                (Some("b"), "read", json!({ "path": "b" })),
            ],
        ),
        // The first chunk carries only the id and type; the name follows.
        (
            "name later",
            vec![
                tc(
                    json!([{ "index": 0, "id": "a", "type": "function", "function": { "arguments": "" } }]),
                    None,
                ),
                tc(json!([{ "index": 0, "function": { "name": "read" } }]), None),
                tc(
                    json!([{ "index": 0, "function": { "arguments": "{\"path\":\"a\"}" } }]),
                    f,
                ),
            ],
            vec![(Some("a"), "read", json!({ "path": "a" }))],
        ),
        // No index: two calls told apart by their ids, the second one's arguments arriving by id.
        (
            "ids, no index",
            vec![
                tc(
                    json!([{ "id": "a", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"a\"}" } }]),
                    None,
                ),
                tc(
                    json!([{ "id": "b", "type": "function", "function": { "name": "ls", "arguments": "{\"path\":" } }]),
                    None,
                ),
                tc(json!([{ "id": "b", "function": { "arguments": "\"b\"}" } }]), f),
            ],
            vec![
                (Some("a"), "read", json!({ "path": "a" })),
                (Some("b"), "ls", json!({ "path": "b" })),
            ],
        ),
        // Neither index nor id: arguments continue the latest call; a second name starts the next.
        (
            "no index, no id",
            vec![
                tc(
                    json!([{ "function": { "name": "read", "arguments": "{\"path\"" } }]),
                    None,
                ),
                tc(json!([{ "function": { "arguments": ":\"a\"}" } }]), None),
                tc(
                    json!([{ "function": { "name": "ls", "arguments": "{\"path\":\"b\"}" } }]),
                    f,
                ),
            ],
            vec![
                (None, "read", json!({ "path": "a" })),
                (None, "ls", json!({ "path": "b" })),
            ],
        ),
        // No index, the name repeated on every chunk: one call while its arguments are unfinished, the next once
        // they are.
        (
            "no index, name repeated",
            vec![
                tc(
                    json!([{ "id": "a", "type": "function", "function": { "name": "read", "arguments": "{\"path\":" } }]),
                    None,
                ),
                tc(json!([{ "function": { "name": "read", "arguments": "\"a\"}" } }]), None),
                tc(
                    json!([{ "function": { "name": "read", "arguments": "{\"path\":\"b\"}" } }]),
                    f,
                ),
            ],
            vec![
                (Some("a"), "read", json!({ "path": "a" })),
                (None, "read", json!({ "path": "b" })),
            ],
        ),
        (
            "no index, no id, name repeated",
            vec![
                tc(
                    json!([{ "type": "function", "function": { "name": "read", "arguments": "" } }]),
                    None,
                ),
                tc(
                    json!([{ "function": { "name": "read", "arguments": "{\"path\":" } }]),
                    None,
                ),
                tc(json!([{ "function": { "name": "read", "arguments": "\"a\"}" } }]), f),
            ],
            vec![(None, "read", json!({ "path": "a" }))],
        ),
        // A brace inside a string does not close the arguments: a counter that ignored strings would split this
        // call in two.
        (
            "no index, name repeated, brace in a string",
            vec![
                tc(
                    json!([{ "id": "a", "type": "function", "function": { "name": "write", "arguments": "{\"path\":\"a\",\"content\":\"}" } }]),
                    None,
                ),
                tc(json!([{ "function": { "name": "write", "arguments": "x\"}" } }]), f),
            ],
            vec![(Some("a"), "write", json!({ "path": "a", "content": "}x" }))],
        ),
        // No index, a name-only chunk after complete arguments: it starts the next call, whose arguments follow
        // without the name.
        (
            "no index, name first",
            vec![
                tc(
                    json!([{ "id": "a", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"a\"}" } }]),
                    None,
                ),
                tc(json!([{ "function": { "name": "read" } }]), None),
                tc(json!([{ "function": { "arguments": "{\"path\":\"b\"}" } }]), f),
            ],
            vec![
                (Some("a"), "read", json!({ "path": "a" })),
                (None, "read", json!({ "path": "b" })),
            ],
        ),
        // The trade-off of that rule: a server that ended a call with a name-only chunk would get an extra call
        // with no arguments (main folded it into the call). No server is known to send one.
        (
            "no index, trailing name",
            vec![
                tc(
                    json!([{ "id": "a", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"a\"}" } }]),
                    None,
                ),
                tc(json!([{ "function": { "name": "read", "arguments": "" } }]), f),
            ],
            vec![(Some("a"), "read", json!({ "path": "a" })), (None, "read", json!({}))],
        ),
        // An empty id is no id: it neither matches the earlier call nor becomes the new one's id.
        (
            "no index, empty ids",
            vec![
                tc(
                    json!([{ "id": "", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"a\"}" } }]),
                    None,
                ),
                tc(
                    json!([{ "id": "", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"b\"}" } }]),
                    f,
                ),
            ],
            vec![
                (None, "read", json!({ "path": "a" })),
                (None, "read", json!({ "path": "b" })),
            ],
        ),
        // Two index-less calls in one chunk.
        (
            "no index, one chunk",
            vec![tc(
                json!([{ "function": { "name": "read", "arguments": "{\"path\":\"a\"}" } }, { "function": { "name": "ls", "arguments": "{\"path\":\"b\"}" } }]),
                f,
            )],
            vec![
                (None, "read", json!({ "path": "a" })),
                (None, "ls", json!({ "path": "b" })),
            ],
        ),
    ];
    for (label, chunks, want) in streams {
        let body = sse(&chunks, false);
        let (fetch, _) = fixed(move || Reply::Sse(body.clone()));
        let (message, events) = run(&OpenAiChatProvider, req_for(model.clone(), fetch)).await.unwrap();
        let got: Vec<&sasacode_ai::ToolCall> = message.tool_calls().collect();
        let got_shape: Vec<(String, Value, Option<String>)> = got
            .iter()
            .map(|c| (c.name.clone(), Value::Object(c.input.clone()), c.raw_input.clone()))
            .collect();
        let want_shape: Vec<(String, Value, Option<String>)> = want
            .iter()
            .map(|(_, name, input)| (name.to_string(), input.clone(), None))
            .collect();
        assert_eq!(got_shape, want_shape, "{label}");
        for (i, (id, _, _)) in want.iter().enumerate() {
            if let Some(id) = id {
                assert_eq!(got[i].id, *id, "{label}");
            }
        }
        let ids: std::collections::HashSet<&str> =
            got.iter().map(|c| c.id.as_str()).filter(|i| !i.is_empty()).collect();
        assert_eq!(ids.len(), got.len(), "{label}");
        // One start per call, ahead of its deltas, at the call's own place in the content.
        let mut starts = Vec::new();
        for e in &events {
            match e {
                StreamEvent::ToolcallStart { index, .. } => starts.push(*index),
                StreamEvent::ToolcallDelta { index, .. } => assert!(starts.contains(index), "{label}"),
                _ => {}
            }
        }
        let positions: Vec<usize> = got
            .iter()
            .map(|c| {
                message
                    .content
                    .iter()
                    .position(|b| b.as_tool_call().is_some_and(|t| std::ptr::eq(t, *c)))
                    .unwrap()
            })
            .collect();
        assert_eq!(starts, positions, "{label}");
        assert_eq!(message.stop_reason, StopReason::ToolUse, "{label}");
    }
}

#[tokio::test]
async fn openai_responses_an_overflow_sent_as_response_failed_is_context_overflow_error_as_it_is_as_an_error_event_99()
{
    let model = model(json!({
        "id": "gpt-r", "provider": "openai", "api": "openai-responses", "baseUrl": "http://x/v1",
        "contextWindow": 100000, "maxOutput": 1000,
    }));
    let message = "Your input exceeds the context window of this model. Please adjust your input and try again.";
    let failed = |error: Value| {
        vec![
            json!({ "type": "response.created", "sequence_number": 0, "response": { "id": "r", "output": [] } }),
            json!({ "type": "response.failed", "sequence_number": 1, "response": { "id": "r", "output": [], "error": error } }),
        ]
    };
    for events in [
        failed(json!({ "code": "context_length_exceeded", "message": message })),
        vec![json!({ "type": "error", "sequence_number": 0, "message": message })],
    ] {
        let body = sse(&events, true);
        let (fetch, _) = fixed(move || Reply::Sse(body.clone()));
        let e = run(&OpenAiResponsesProvider, req_for(model.clone(), fetch))
            .await
            .unwrap_err();
        assert!(sasacode_ai::is_context_overflow(&e), "{e}");
    }
    let body = sse(
        &failed(json!({ "code": "server_error", "message": "The server had an error" })),
        true,
    );
    let (fetch, _) = fixed(move || Reply::Sse(body.clone()));
    let other = run(&OpenAiResponsesProvider, req_for(model, fetch)).await.unwrap_err();
    assert!(other.to_string().contains("The server had an error"), "{other}");
    assert!(!sasacode_ai::is_context_overflow(&other));
}
