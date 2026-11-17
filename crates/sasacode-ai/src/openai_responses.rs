//! OpenAI Responses adapter (`packages/ai/src/openai-responses.ts`).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use futures::stream::BoxStream;
use regex::Regex;
use serde_json::{Value, json};

use crate::effort::with_effort;
use crate::openai_chat::{count, is_context_overflow, num, openai_client};
use crate::provider::{ContextOverflowError, Provider, Request, StreamEvent};
use crate::replay::{can_replay, new_reply, replay_scope};
use crate::transport::{
    ApiError, ApiErrorKind, EventSource, Next, Yielder, generator, is_cut_reply, js_string, js_stringify, js_truthy,
};
use crate::types::{
    AssistantContent, JsonObject, Message, ModelInfo, Phase, StopReason, TextContent, ThinkingContent, ToolCall,
    UserContent, compute_cost, parse_tool_input, reported_cost, sampling_fields,
};

pub struct OpenAiResponsesProvider;

#[async_trait::async_trait]
impl Provider for OpenAiResponsesProvider {
    fn api(&self) -> &str {
        "openai-responses"
    }
    fn stream(&self, req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        generator(move |y| run(req, y))
    }
}

/// A message's `phase`: `Some(None)` for an explicit null. A value this version does not know is dropped.
fn phase_of(item: &Value) -> Option<Option<Phase>> {
    match item.get("phase")? {
        Value::Null => Some(None),
        Value::String(s) if s == "commentary" => Some(Some(Phase::Commentary)),
        Value::String(s) if s == "final_answer" => Some(Some(Phase::FinalAnswer)),
        _ => None,
    }
}

/// JS `trim()`: whitespace, line terminators and the BOM.
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

static CONTEXT: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    Regex::new("(?i)context").unwrap()
});

struct Item {
    index: usize,
    json: Option<String>,
}

async fn run(req: Request, y: Yielder) -> anyhow::Result<()> {
    let model = &req.model;
    let client = openai_client(&req);
    let mut params = JsonObject::new();
    params.insert("model".into(), json!(model.id));
    params.insert("instructions".into(), json!(req.system));
    params.insert("input".into(), Value::Array(to_responses_input(&req.messages, model)?));
    params.insert("stream".into(), json!(true));
    params.insert("store".into(), json!(false));
    params.insert(
        "max_output_tokens".into(),
        json!(req.max_tokens.unwrap_or(model.max_output)),
    );
    if !req.tools.is_empty() {
        let tools = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function", "name": t.name, "description": t.description,
                    "parameters": t.parameters, "strict": false,
                })
            })
            .collect();
        params.insert("tools".into(), Value::Array(tools));
    }
    for (k, v) in sampling_fields(req.sampling.as_ref(), &["temperature", "top_p"]) {
        params.insert(k, v);
    }
    let effort_given = params
        .get("reasoning")
        .and_then(Value::as_object)
        .is_some_and(|r| r.contains_key("effort"));
    let level = if effort_given || model.reasoning != Some(true) {
        None
    } else {
        req.thinking
    };

    let mut out = Arc::new(new_reply(model));
    y.emit(StreamEvent::Start { partial: out.clone() }).await;
    let mut items: HashMap<String, Item> = HashMap::new();
    let mut ended = false;

    let result: anyhow::Result<()> = async {
        let scope = replay_scope(model);
        let (client, signal, params) = (&client, &req.signal, &params);
        let (mut src, mut next) = with_effort(&scope, level, |effort| {
            let mut body = params.clone();
            // Request encrypted state explicitly for compatible services that predate automatic inclusion.
            if let Some(effort) = effort {
                let mut reasoning = JsonObject::new();
                reasoning.insert("effort".into(), json!(effort));
                reasoning.insert("summary".into(), json!("auto"));
                if let Some(Value::Object(given)) = params.get("reasoning") {
                    for (k, v) in given {
                        reasoning.insert(k.clone(), v.clone());
                    }
                }
                body.insert("reasoning".into(), Value::Object(reasoning));
                let mut include: Vec<Value> = match params.get("include") {
                    Some(Value::Array(a)) => a.clone(),
                    Some(Value::String(s)) => s.chars().map(|c| json!(c.to_string())).collect(),
                    _ => Vec::new(),
                };
                include.push(json!("reasoning.encrypted_content"));
                // `[...new Set(...)]`: equal primitives once each; objects are never the same.
                let mut unique: Vec<Value> = Vec::new();
                for v in include {
                    if v.is_object() || v.is_array() || !unique.contains(&v) {
                        unique.push(v);
                    }
                }
                body.insert("include".into(), Value::Array(unique));
            }
            async move {
                let resp = client.post("/responses", &Value::Object(body), &[], signal).await?;
                let mut src = EventSource::new(resp, true);
                let first = src.next_openai(signal).await?;
                if let Next::Item(ev) = &first
                    && ev.get("type").and_then(Value::as_str) == Some("error")
                {
                    let message = ev.get("message").and_then(Value::as_str);
                    return Err(ApiError::openai(None, Some(ev.clone()), message).into());
                }
                Ok((src, first))
            }
        })
        .await?;
        loop {
            // The SDK's stream ends quietly when the signal aborts it mid-reply.
            let Next::Item(ev) = next else { break };
            let str_of = |k: &str| ev.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
            let item_key = js_string(ev.get("item_id"));
            match ev.get("type").and_then(Value::as_str).unwrap_or_default() {
                "response.output_item.added" => {
                    let item = ev.get("item").cloned().unwrap_or(Value::Null);
                    let id = js_string(item.get("id"));
                    let field = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                    match item.get("type").and_then(Value::as_str) {
                        Some("function_call") => {
                            let (call_id, name) = (field("call_id"), field("name"));
                            let index = {
                                let o = Arc::make_mut(&mut out);
                                o.content.push(AssistantContent::ToolCall(ToolCall {
                                    id: call_id.clone(),
                                    name: name.clone(),
                                    input: JsonObject::new(),
                                    raw_input: None,
                                }));
                                o.content.len() - 1
                            };
                            items.insert(
                                id,
                                Item {
                                    index,
                                    json: Some(String::new()),
                                },
                            );
                            y.emit(StreamEvent::ToolcallStart {
                                index,
                                id: call_id,
                                name,
                                partial: out.clone(),
                            })
                            .await;
                        }
                        Some("reasoning") => {
                            let o = Arc::make_mut(&mut out);
                            o.content.push(AssistantContent::Thinking(ThinkingContent {
                                thinking: String::new(),
                                signature: None,
                                redacted: None,
                                reasoning_field: None,
                            }));
                            items.insert(
                                id,
                                Item {
                                    index: o.content.len() - 1,
                                    json: None,
                                },
                            );
                        }
                        Some("message") => {
                            let o = Arc::make_mut(&mut out);
                            o.content.push(AssistantContent::Text(TextContent {
                                text: String::new(),
                                phase: phase_of(&item),
                            }));
                            items.insert(
                                id,
                                Item {
                                    index: o.content.len() - 1,
                                    json: None,
                                },
                            );
                        }
                        _ => {}
                    }
                }
                ty @ ("response.refusal.delta" | "response.output_text.delta") => {
                    if ty == "response.refusal.delta" {
                        Arc::make_mut(&mut out).stop_reason = StopReason::Refusal;
                    }
                    if let Some(it) = items.get(&item_key)
                        && let AssistantContent::Text(b) = &mut Arc::make_mut(&mut out).content[it.index]
                    {
                        let delta = str_of("delta");
                        b.text.push_str(&delta);
                        let index = it.index;
                        y.emit(StreamEvent::TextDelta {
                            index,
                            delta,
                            partial: out.clone(),
                        })
                        .await;
                    }
                }
                "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                    if let Some(it) = items.get(&item_key)
                        && let AssistantContent::Thinking(b) = &mut Arc::make_mut(&mut out).content[it.index]
                    {
                        let delta = str_of("delta");
                        b.thinking.push_str(&delta);
                        let index = it.index;
                        y.emit(StreamEvent::ThinkingDelta {
                            index,
                            delta,
                            partial: out.clone(),
                        })
                        .await;
                    }
                }
                "response.reasoning_summary_part.done" => {
                    if let Some(it) = items.get(&item_key)
                        && let AssistantContent::Thinking(b) = &mut Arc::make_mut(&mut out).content[it.index]
                    {
                        b.thinking.push_str("\n\n");
                    }
                }
                "response.function_call_arguments.delta" => {
                    if let Some(it) = items.get_mut(&item_key) {
                        let delta = str_of("delta");
                        it.json.get_or_insert_with(String::new).push_str(&delta);
                        let index = it.index;
                        y.emit(StreamEvent::ToolcallDelta {
                            index,
                            delta,
                            partial: out.clone(),
                        })
                        .await;
                    }
                }
                "response.output_item.done" => {
                    let item = ev.get("item").cloned().unwrap_or(Value::Null);
                    let Some(it) = items.get(&js_string(item.get("id"))) else {
                        next = src.next_openai(signal).await?;
                        continue;
                    };
                    let o = Arc::make_mut(&mut out);
                    match (item.get("type").and_then(Value::as_str), &mut o.content[it.index]) {
                        (Some("function_call"), AssistantContent::ToolCall(b)) => {
                            // `item.arguments || it.json || ""`
                            let arguments = item.get("arguments").and_then(Value::as_str).filter(|a| !a.is_empty());
                            let json = arguments
                                .or(it.json.as_deref().filter(|j| !j.is_empty()))
                                .unwrap_or_default();
                            let (input, raw_input) = parse_tool_input(json);
                            b.input = input;
                            if raw_input.is_some() {
                                b.raw_input = raw_input;
                            }
                        }
                        (Some("reasoning"), AssistantContent::Thinking(b)) => {
                            b.thinking = js_trim(&b.thinking).to_string();
                            b.signature = Some(js_stringify(&item));
                        }
                        (Some("message"), AssistantContent::Text(b)) if item.get("phase").is_some() => {
                            b.phase = phase_of(&item);
                        }
                        _ => {}
                    }
                }
                ty @ ("response.completed" | "response.incomplete") => {
                    ended = true;
                    let response = ev.get("response");
                    let o = Arc::make_mut(&mut out);
                    if let Some(u) = response.and_then(|r| r.get("usage")).filter(|u| js_truthy(u)) {
                        let details = u.get("input_tokens_details");
                        let cached = num(details.and_then(|d| d.get("cached_tokens"))).unwrap_or(0.0);
                        let written = num(details.and_then(|d| d.get("cache_write_tokens"))).unwrap_or(0.0);
                        let input = num(u.get("input_tokens")).unwrap_or(f64::NAN);
                        o.usage.input = count(input - cached - written);
                        o.usage.cache_write = count(written);
                        o.usage.cache_read = count(cached);
                        o.usage.output = count(num(u.get("output_tokens")).unwrap_or(f64::NAN));
                        // Proxies over OpenAI (OpenRouter, Command Code) may report actual spend on the usage.
                        if let Some(reported) = reported_cost(Some(u)) {
                            o.usage.cost = reported;
                        }
                    }
                    if ty == "response.incomplete" {
                        let reason = response
                            .and_then(|r| r.get("incomplete_details"))
                            .and_then(|d| d.get("reason"))
                            .and_then(Value::as_str);
                        o.stop_reason = if reason == Some("content_filter") {
                            StopReason::Refusal
                        } else {
                            StopReason::MaxTokens
                        };
                    }
                }
                "response.failed" => {
                    let error = ev.get("response").and_then(|r| r.get("error"));
                    let message = error.and_then(|e| e.get("message")).filter(|m| !m.is_null());
                    let code = js_string(error.and_then(|e| e.get("code")));
                    let text = format!("{code} {}", js_string(message));
                    if CONTEXT.is_match(&text) {
                        // As an "error" event too (#99).
                        let message = message.map(|m| js_string(Some(m))).unwrap_or_default();
                        return Err(ContextOverflowError(message).into());
                    }
                    let message = message
                        .map(|m| js_string(Some(m)))
                        .unwrap_or_else(|| "response failed".into());
                    return Err(anyhow::anyhow!(message));
                }
                "error" => {
                    let message = ev.get("message").map(|m| js_string(Some(m))).unwrap_or_default();
                    if CONTEXT.is_match(&message) {
                        return Err(ContextOverflowError(message).into());
                    }
                    return Err(anyhow::anyhow!(message));
                }
                _ => {}
            }
            next = src.next_openai(signal).await?;
        }
        Ok(())
    }
    .await;

    if let Err(e) = result {
        let user_abort = e
            .downcast_ref::<ApiError>()
            .is_some_and(|a| a.kind == ApiErrorKind::UserAbort);
        if user_abort || req.signal.is_cancelled() {
            Arc::make_mut(&mut out).stop_reason = StopReason::Aborted;
        } else if is_context_overflow(&e) {
            return Err(ContextOverflowError(e.to_string()).into());
        } else if !is_cut_reply(&e) {
            return Err(e);
        }
    }
    let o = Arc::make_mut(&mut out);
    o.content
        .retain(|c| !matches!(c, AssistantContent::Text(t) if t.text.is_empty()));
    o.mark_if_cut(ended); // neither response.completed nor response.incomplete
    if o.stop_reason == StopReason::Stop && o.tool_calls().next().is_some() {
        o.stop_reason = StopReason::ToolUse;
    }
    // A cost reported with the usage stands; otherwise estimate from the price table.
    if o.usage.cost == 0.0 {
        o.usage.cost = compute_cost(model, &o.usage);
    }
    y.emit(StreamEvent::Done {
        message: Arc::unwrap_or_clone(out),
    })
    .await;
    Ok(())
}

fn input_content(content: &[UserContent]) -> Vec<Value> {
    content
        .iter()
        .map(|c| match c {
            UserContent::Text(t) => json!({ "type": "input_text", "text": t.text }),
            UserContent::Image(i) => json!({
                "type": "input_image",
                "image_url": format!("data:{};base64,{}", i.media_type, i.data),
                "detail": "auto",
            }),
        })
        .collect()
}

/// The history as Responses input items (`toResponsesInput`).
fn to_responses_input(messages: &[Message], model: &ModelInfo) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();
    for m in messages {
        match m {
            Message::User(u) => out.push(json!({ "role": "user", "content": input_content(&u.content) })),
            Message::Tool(t) => out.push(json!({
                "type": "function_call_output",
                "call_id": t.tool_call_id,
                "output": input_content(&t.content),
            })),
            Message::Assistant(a) => {
                // Encrypted reasoning only goes back to the service and model that produced it.
                let same_model = can_replay(a, model);
                for c in &a.content {
                    match c {
                        AssistantContent::Text(t) => {
                            if t.text.is_empty() {
                                continue;
                            }
                            let mut msg = JsonObject::new();
                            msg.insert("role".into(), json!("assistant"));
                            msg.insert("content".into(), json!(t.text));
                            if a.api == model.api
                                && let Some(phase) = &t.phase
                            {
                                msg.insert("phase".into(), json!(phase));
                            }
                            out.push(Value::Object(msg));
                        }
                        AssistantContent::Thinking(t) => {
                            // Reasoning items only carry over to the model that produced them.
                            if let Some(sig) = t.signature.as_deref().filter(|s| same_model && !s.is_empty()) {
                                out.push(crate::sse::parse_json(sig)?);
                            }
                        }
                        AssistantContent::ToolCall(t) => out.push(json!({
                            "type": "function_call",
                            "call_id": t.id,
                            "name": t.name,
                            "arguments": t.raw_input.clone().unwrap_or_else(|| js_stringify(&Value::Object(t.input.clone()))),
                        })),
                    }
                }
            }
        }
    }
    Ok(out)
}
