//! Anthropic Messages API adapter (`packages/ai/src/anthropic.ts`).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use futures::stream::BoxStream;
use regex::Regex;
use serde_json::{Value, json};

use crate::provider::{ContextOverflowError, Provider, Request, StreamEvent};
use crate::replay::{can_replay, new_reply};
use crate::transport::{
    ApiClient, ApiError, ApiErrorKind, EventSource, Next, Yielder, generator, is_cut_reply, js_string, js_stringify,
};
use crate::types::{
    AssistantContent, AssistantMessage, JsonObject, Message, ModelInfo, StopReason, ThinkingContent, ThinkingLevel,
    ToolCall, UserContent, compute_cost, has_origin, parse_tool_input, sampling_fields,
};

pub struct AnthropicProvider;

const OFFICIAL_BASE: &str = "https://api.anthropic.com";

fn re(pattern: &str) -> Regex {
    // INFALLIBLE: only called with the fixed patterns below.
    Regex::new(pattern).unwrap()
}

/// Thinking cannot be turned off on these: effort is the only control.
static ALWAYS_THINKS: LazyLock<Regex> = LazyLock::new(|| re("^claude-(opus-5-5|fable|mythos)"));
/// These reject preserved thinking when the prompt, tools or earlier messages change.
static BINDS_THINKING: LazyLock<Regex> = LazyLock::new(|| re("^claude-(opus-5-5|sonnet-5-5|fable-5-1)"));
static SONNET_5_5: LazyLock<Regex> = LazyLock::new(|| re("^claude-sonnet-5-5(?:-|$)"));
const BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";

static TOO_LONG: LazyLock<Regex> = LazyLock::new(|| re("(?i)prompt is too long|context window"));
static DISABLE_REFUSED: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)thinking\.type\.disabled|disabled[^\n\r  ]*not supported"));
static ABOUT_HISTORY: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)messages[.\[]|signature|reasoning_content"));
static EFFORT: LazyLock<Regex> = LazyLock::new(|| re("(?i)effort"));
static BOUND_ELSEWHERE: LazyLock<Regex> = LazyLock::new(|| re("(?i)bound to a different conversation"));

/// What a model turned out not to accept, so later requests go straight to what works.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Known {
    pub no_disable: bool,
    pub no_xhigh: bool,
    pub no_binding: bool,
}

/// Per endpoint and model, shared by every request to them.
static LEARNED: LazyLock<Mutex<HashMap<String, Arc<Mutex<Known>>>>> = LazyLock::new(Default::default);

fn learned(key: &str) -> Arc<Mutex<Known>> {
    LEARNED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key.to_string())
        .or_default()
        .clone()
}

/// The thinking fields for a level. Only the official API gets fields beyond what was sent before
/// (`thinkingFields`).
pub fn thinking_fields(id: &str, level: Option<ThinkingLevel>, official: bool, known: &Known) -> JsonObject {
    let adaptive = |effort: Option<&str>| {
        let mut o = JsonObject::new();
        o.insert(
            "thinking".into(),
            json!({ "type": "adaptive", "display": "summarized" }),
        );
        if let Some(effort) = effort {
            o.insert("output_config".into(), json!({ "effort": effort }));
        }
        o
    };
    let single = |k: &str, v: Value| {
        let mut o = JsonObject::new();
        o.insert(k.into(), v);
        o
    };
    let Some(level) = level else {
        return if BINDS_THINKING.is_match(id) && official {
            adaptive(None)
        } else {
            JsonObject::new()
        };
    };
    if level == ThinkingLevel::Off {
        if !official {
            return JsonObject::new();
        }
        // The installed SDK predated Sonnet 5.5's lowest thinking setting.
        if SONNET_5_5.is_match(id) {
            return single("thinking", json!({ "type": "between_tools" }));
        }
        if ALWAYS_THINKS.is_match(id) || known.no_disable {
            return adaptive(Some("low"));
        }
        return if id.starts_with("claude-") {
            single("thinking", json!({ "type": "disabled" }))
        } else {
            JsonObject::new()
        };
    }
    adaptive(Some(if level == ThinkingLevel::Xhigh && known.no_xhigh {
        "high"
    } else {
        level.as_str()
    }))
}

#[async_trait::async_trait]
impl Provider for AnthropicProvider {
    fn api(&self) -> &str {
        "anthropic"
    }
    fn stream(&self, req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        generator(move |y| run(req, y))
    }
}

/// `new Headers(model.headers).get(name)`: case-insensitive, several values joined.
fn header_of(model: &ModelInfo, name: &str) -> Option<String> {
    let values: Vec<&str> = model
        .headers
        .iter()
        .flatten()
        .filter(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

/// Adjust a rejected request only before any content has arrived. Returns whether to send it again.
fn adjust(message: &str, params: &Value, known: &Mutex<Known>, strip_thinking: &mut bool) -> bool {
    let thinking = params.get("thinking");
    let thinking_type = thinking.and_then(|t| t.get("type")).and_then(Value::as_str);
    let mut k = known.lock().unwrap_or_else(|e| e.into_inner());
    if thinking_type == Some("disabled") && DISABLE_REFUSED.is_match(message) {
        k.no_disable = true;
        return true;
    }
    let effort = params
        .get("output_config")
        .and_then(|o| o.get("effort"))
        .and_then(Value::as_str);
    if effort == Some("xhigh") && !ABOUT_HISTORY.is_match(message) && EFFORT.is_match(message) {
        k.no_xhigh = true;
        return true;
    }
    if thinking_type == Some("adaptive")
        && thinking.and_then(|t| t.get("block_binding")).is_some()
        && message.contains("block_binding")
        && !k.no_binding
    {
        k.no_binding = true;
        return true;
    }
    // Preserved thinking where the binding controls are not offered: send the history without thinking once.
    if BOUND_ELSEWHERE.is_match(message) && !*strip_thinking {
        *strip_thinking = true;
        return true;
    }
    false
}

fn read_usage(out: &mut AssistantMessage, u: Option<&Value>) {
    let Some(u) = u else { return };
    let get = |k: &str| u.get(k).and_then(Value::as_f64).map(crate::openai_chat::count);
    if let Some(v) = get("input_tokens") {
        out.usage.input = v;
    }
    if let Some(v) = get("output_tokens") {
        out.usage.output = v;
    }
    if let Some(v) = get("cache_read_input_tokens") {
        out.usage.cache_read = v;
    }
    if let Some(v) = get("cache_creation_input_tokens") {
        out.usage.cache_write = v;
    }
}

fn map_stop(r: Option<&str>) -> StopReason {
    match r {
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("refusal") => StopReason::Refusal,
        _ => StopReason::Stop,
    }
}

async fn run(req: Request, y: Yielder) -> anyhow::Result<()> {
    let model = &req.model;
    let base = model.base_url.as_deref().unwrap_or(OFFICIAL_BASE);
    // Without a key, ANTHROPIC_API_KEY / ANTHROPIC_AUTH_TOKEN, else an `ant auth login` profile (as the SDK did).
    let client = ApiClient::anthropic(
        req.api_key.as_deref(),
        base,
        req.max_retries.unwrap_or(8),
        model.headers.as_ref(),
        req.fetch.clone(),
    );
    let official = has_origin(Some(base), OFFICIAL_BASE);
    let known = learned(&js_stringify(&json!([base, model.id])));
    let tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| {
            let mut o = JsonObject::new();
            o.insert("name".into(), json!(t.name));
            o.insert("description".into(), json!(t.description));
            o.insert("input_schema".into(), t.parameters.clone());
            if official {
                o.insert("eager_input_streaming".into(), json!(true));
            }
            Value::Object(o)
        })
        .collect();
    let build = |strip_thinking: bool, known: Known| -> (Value, Option<String>) {
        let mut params = JsonObject::new();
        params.insert("model".into(), json!(model.id));
        params.insert("max_tokens".into(), json!(req.max_tokens.unwrap_or(model.max_output)));
        params.insert("system".into(), json!(req.system));
        params.insert(
            "messages".into(),
            Value::Array(to_anthropic_messages(&req.messages, model, strip_thinking)),
        );
        params.insert("tools".into(), Value::Array(tools.clone()));
        params.insert("stream".into(), json!(true));
        params.insert("cache_control".into(), json!({ "type": "ephemeral" }));
        if model.reasoning == Some(true) {
            params.extend(thinking_fields(&model.id, req.thinking, official, &known));
        }
        // Newer Claude models reject sampling parameters; only what a plugin or config set explicitly is sent.
        for (k, v) in sampling_fields(req.sampling.as_ref(), &["temperature", "top_p"]) {
            params.insert(k, v);
        }
        let mut beta = None;
        let adaptive = params
            .get("thinking")
            .and_then(|t| t.get("type"))
            .and_then(Value::as_str)
            == Some("adaptive");
        if official
            && adaptive
            && BINDS_THINKING.is_match(&model.id)
            && !known.no_binding
            && let Some(Value::Object(thinking)) = params.get_mut("thinking")
        {
            // A block whose context changed is dropped instead of failing the whole request.
            thinking.insert(
                "block_binding".into(),
                json!({ "prefix_mismatch_behavior": "drop_block" }),
            );
            let parts: Vec<String> = header_of(model, "anthropic-beta")
                .into_iter()
                .chain([BINDING_BETA.to_string()])
                .filter(|s| !s.is_empty())
                .collect();
            beta = Some(parts.join(","));
        }
        (Value::Object(params), beta)
    };

    let mut out = Arc::new(new_reply(model));
    y.emit(StreamEvent::Start { partial: out.clone() }).await;
    // The API's block index → the block's place in the content (blocks of unknown types are skipped).
    let mut slots: HashMap<i64, usize> = HashMap::new();
    let mut json_buf: HashMap<i64, String> = HashMap::new();
    let mut ended = false;
    let mut strip_thinking = false;
    let mut attempt = 0;
    loop {
        let current = *known.lock().unwrap_or_else(|e| e.into_inner());
        let (params, beta) = build(strip_thinking, current);
        let result: anyhow::Result<()> = async {
            let extra: Vec<(&str, &str)> = beta.iter().map(|b| ("anthropic-beta", b.as_str())).collect();
            let resp = client.post("/v1/messages", &params, &extra, &req.signal).await?;
            let mut src = EventSource::new(resp, false);
            let mut started = false;
            loop {
                let ev = match src.next_anthropic(&req.signal).await? {
                    Next::Item(ev) => ev,
                    Next::End => break,
                    // The SDK's MessageStream throws APIUserAbortError.
                    Next::Aborted => return Err(ApiError::user_abort().into()),
                };
                let ty = ev.get("type").and_then(Value::as_str).unwrap_or_default();
                // The order the SDK's MessageStream insisted on.
                if ty == "message_start" {
                    if started {
                        anyhow::bail!("Unexpected event order, got message_start before receiving \"message_stop\"");
                    }
                    started = true;
                } else if !started {
                    anyhow::bail!(
                        "Unexpected event order, got {} before \"message_start\"",
                        js_string(ev.get("type"))
                    );
                }
                let api_index = ev.get("index").and_then(Value::as_i64).unwrap_or(-1);
                let str_at = |v: Option<&Value>, k: &str| {
                    v.and_then(|v| v.get(k))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                match ty {
                    "message_start" => {
                        read_usage(Arc::make_mut(&mut out), ev.get("message").and_then(|m| m.get("usage")));
                    }
                    "content_block_start" => {
                        let b = ev.get("content_block");
                        let block = match b.and_then(|b| b.get("type")).and_then(Value::as_str) {
                            Some("text") => Some(AssistantContent::text("")),
                            Some("thinking") => Some(AssistantContent::Thinking(ThinkingContent {
                                thinking: String::new(),
                                signature: Some(String::new()),
                                redacted: None,
                                reasoning_field: None,
                            })),
                            Some("redacted_thinking") => Some(AssistantContent::Thinking(ThinkingContent {
                                thinking: String::new(),
                                signature: b
                                    .and_then(|b| b.get("data"))
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                                redacted: Some(true),
                                reasoning_field: None,
                            })),
                            Some("tool_use") => Some(AssistantContent::ToolCall(ToolCall {
                                id: str_at(b, "id"),
                                name: str_at(b, "name"),
                                input: JsonObject::new(),
                                raw_input: None,
                            })),
                            _ => None,
                        };
                        let Some(block) = block else { continue };
                        let call = block.as_tool_call().map(|c| (c.id.clone(), c.name.clone()));
                        let index = {
                            let o = Arc::make_mut(&mut out);
                            match slots.get(&api_index) {
                                Some(&i) => {
                                    o.content[i] = block;
                                    i
                                }
                                None => {
                                    o.content.push(block);
                                    slots.insert(api_index, o.content.len() - 1);
                                    o.content.len() - 1
                                }
                            }
                        };
                        if let Some((id, name)) = call {
                            json_buf.insert(api_index, String::new());
                            y.emit(StreamEvent::ToolcallStart {
                                index,
                                id,
                                name,
                                partial: out.clone(),
                            })
                            .await;
                        }
                    }
                    "content_block_delta" => {
                        let Some(&index) = slots.get(&api_index) else { continue };
                        let d = ev.get("delta");
                        let delta_type = d
                            .and_then(|d| d.get("type"))
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let event = {
                            let o = Arc::make_mut(&mut out);
                            match (delta_type, &mut o.content[index]) {
                                ("text_delta", AssistantContent::Text(b)) => {
                                    let delta = str_at(d, "text");
                                    b.text.push_str(&delta);
                                    Some((0, delta))
                                }
                                ("thinking_delta", AssistantContent::Thinking(b)) => {
                                    let delta = str_at(d, "thinking");
                                    b.thinking.push_str(&delta);
                                    Some((1, delta))
                                }
                                ("signature_delta", AssistantContent::Thinking(b)) => {
                                    b.signature
                                        .get_or_insert_with(String::new)
                                        .push_str(&str_at(d, "signature"));
                                    None
                                }
                                ("input_json_delta", AssistantContent::ToolCall(_)) => {
                                    let delta = str_at(d, "partial_json");
                                    json_buf.entry(api_index).or_default().push_str(&delta);
                                    Some((2, delta))
                                }
                                _ => None,
                            }
                        };
                        let partial = out.clone();
                        match event {
                            Some((0, delta)) => y.emit(StreamEvent::TextDelta { index, delta, partial }).await,
                            Some((1, delta)) => y.emit(StreamEvent::ThinkingDelta { index, delta, partial }).await,
                            Some((_, delta)) => y.emit(StreamEvent::ToolcallDelta { index, delta, partial }).await,
                            None => {}
                        }
                    }
                    "content_block_stop" => {
                        if let Some(&index) = slots.get(&api_index)
                            && let AssistantContent::ToolCall(b) = &mut Arc::make_mut(&mut out).content[index]
                        {
                            // A malformed buffer becomes rawInput here (the SDK's partial-JSON parser threw on it).
                            let (input, raw_input) =
                                parse_tool_input(json_buf.get(&api_index).map_or("", String::as_str));
                            b.input = input;
                            if raw_input.is_some() {
                                b.raw_input = raw_input;
                            }
                        }
                    }
                    "message_delta" => {
                        let o = Arc::make_mut(&mut out);
                        read_usage(o, ev.get("usage"));
                        let stop = ev
                            .get("delta")
                            .and_then(|d| d.get("stop_reason"))
                            .filter(|s| !s.is_null());
                        let stop_str = stop.and_then(Value::as_str);
                        if stop_str == Some("model_context_window_exceeded") {
                            return Err(ContextOverflowError("model context window exceeded".into()).into());
                        }
                        o.stop_reason = map_stop(stop_str);
                        if stop.is_some_and(crate::transport::js_truthy) {
                            ended = true;
                        }
                    }
                    "message_stop" => ended = true,
                    _ => {}
                }
            }
            if !started {
                anyhow::bail!("request ended without sending any chunks");
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {}
            Err(e) => {
                let api = e.downcast_ref::<ApiError>();
                let bad_request = api.filter(|a| a.is_bad_request());
                if api.is_some_and(|a| a.kind == ApiErrorKind::UserAbort) || req.signal.is_cancelled() {
                    Arc::make_mut(&mut out).stop_reason = StopReason::Aborted;
                } else if let Some(a) = bad_request.filter(|a| TOO_LONG.is_match(&a.message)) {
                    return Err(ContextOverflowError(a.message.clone()).into());
                } else if attempt < 3
                    && out.content.is_empty()
                    && let Some(a) = bad_request
                    && adjust(&a.message, &params, &known, &mut strip_thinking)
                {
                    attempt += 1;
                    continue;
                } else if !is_cut_reply(&e) {
                    return Err(e);
                }
            }
        }
        break;
    }
    let o = Arc::make_mut(&mut out);
    o.mark_if_cut(ended, req.signal.is_cancelled()); // no stop reason and no message_stop
    o.usage.cost = compute_cost(model, &o.usage);
    y.emit(StreamEvent::Done {
        message: Arc::unwrap_or_clone(out),
    })
    .await;
    Ok(())
}

fn user_blocks(content: &[UserContent]) -> Vec<Value> {
    content
        .iter()
        .map(|c| match c {
            UserContent::Text(t) => json!({ "type": "text", "text": t.text }),
            UserContent::Image(i) => json!({
                "type": "image",
                "source": { "type": "base64", "media_type": i.media_type, "data": i.data },
            }),
        })
        .collect()
}

/// The history as Messages API messages (`toAnthropicMessages`). `no_thinking` leaves out every
/// thinking block (a model that rejected them as bound elsewhere).
pub fn to_anthropic_messages(messages: &[Message], model: &ModelInfo, no_thinking: bool) -> Vec<Value> {
    let mut out: Vec<(&'static str, Vec<Value>)> = Vec::new();
    // Consecutive same-role messages (tool results + a queued user message) must be merged.
    let mut push = |role: &'static str, blocks: Vec<Value>| match out.last_mut() {
        Some((last, content)) if *last == role => content.extend(blocks),
        _ => out.push((role, blocks)),
    };
    for m in messages {
        match m {
            Message::User(u) => push("user", user_blocks(&u.content)),
            Message::Tool(t) => push(
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": t.tool_call_id,
                    "content": user_blocks(&t.content),
                    "is_error": t.is_error,
                })],
            ),
            Message::Assistant(a) => {
                let same_model = can_replay(a, model);
                let mut blocks = Vec::new();
                for c in &a.content {
                    match c {
                        AssistantContent::Text(t) => {
                            if !t.text.is_empty() {
                                blocks.push(json!({ "type": "text", "text": t.text }));
                            }
                        }
                        AssistantContent::Thinking(t) => {
                            // Thinking blocks are bound to the model that produced them; drop them elsewhere.
                            let Some(sig) = t.signature.as_deref().filter(|s| !s.is_empty()) else {
                                continue;
                            };
                            if !same_model || no_thinking {
                                continue;
                            }
                            if t.redacted == Some(true) {
                                blocks.push(json!({ "type": "redacted_thinking", "data": sig }));
                            } else {
                                blocks.push(json!({ "type": "thinking", "thinking": t.thinking, "signature": sig }));
                            }
                        }
                        AssistantContent::ToolCall(t) => {
                            blocks.push(json!({ "type": "tool_use", "id": t.id, "name": t.name, "input": t.input }));
                        }
                    }
                }
                if !blocks.is_empty() {
                    push("assistant", blocks);
                }
            }
        }
    }
    out.into_iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect()
}
