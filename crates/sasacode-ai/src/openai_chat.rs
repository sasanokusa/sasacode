//! OpenAI Chat Completions adapter (`packages/ai/src/openai-chat.ts`), also for the many servers that
//! speak it (OpenRouter, Command Code, vLLM, Ollama, llama.cpp, LM Studio).

use std::sync::{Arc, LazyLock};

use futures::stream::BoxStream;
use regex::Regex;
use serde_json::{Value, json};

use crate::effort::with_effort;
use crate::provider::{ContextOverflowError, Provider, Request, StreamEvent};
use crate::replay::{can_replay, new_reply, replay_scope};
use crate::transport::{
    ApiClient, ApiError, ApiErrorKind, EventSource, Next, Yielder, generator, is_cut_reply, js_string, js_stringify,
    js_truthy,
};
use crate::types::{
    AssistantContent, JsonObject, LocalServer, Message, ModelInfo, StopReason, ThinkingContent, ThinkingLevel,
    ToolCall, Usage, UserContent, compute_cost, has_origin, parse_tool_input, reported_cost, sampling_fields,
};

pub struct OpenAiChatProvider;

const OPENAI_BASE: &str = "https://api.openai.com/v1";

fn openrouter(m: &ModelInfo) -> bool {
    has_origin(m.base_url.as_deref(), "https://openrouter.ai")
}

/// The client both OpenAI-style adapters use (`openaiClient`).
pub fn openai_client(req: &Request) -> ApiClient {
    // Local servers (Ollama, vLLM) accept any key; the SDK refused an empty one.
    let key = req.api_key.as_deref().filter(|k| !k.is_empty()).unwrap_or("none");
    ApiClient::openai(
        key,
        req.model.base_url.as_deref().unwrap_or(OPENAI_BASE),
        req.max_retries.unwrap_or(8),
        req.model.headers.as_ref(),
        req.fetch.clone(),
    )
}

static OVERFLOW_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    Regex::new(r"(?i)context length|context window|context size|too many tokens|maximum context").unwrap()
});

/// Did an OpenAI-style server say the request does not fit the context (`isContextOverflow`)?
pub fn is_context_overflow(e: &anyhow::Error) -> bool {
    let Some(e) = e.downcast_ref::<ApiError>() else {
        return false;
    };
    e.code.as_ref().and_then(Value::as_str) == Some("context_length_exceeded")
        || e.error_type.as_ref().and_then(Value::as_str) == Some("exceed_context_size_error")
        || OVERFLOW_TEXT.is_match(&e.message)
}

/// A token count from JSON (`??` semantics: absent and null are both missing).
pub(crate) fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
}

/// A count for [`Usage`]: negative or not a number counts as 0.
pub(crate) fn count(x: f64) -> u64 {
    if x.is_finite() && x > 0.0 { x.round() as u64 } else { 0 }
}

/// A Chat Completions usage object: the standard counts, plus fields vendors add on (cached tokens,
/// DeepSeek's cache hit/miss, OpenRouter's reported `cost`). Prefers the provider's cost, else the
/// price table (`parseChatUsage`).
pub fn parse_chat_usage(model: &ModelInfo, u: &Value) -> Usage {
    let details = u.get("prompt_tokens_details");
    let cached = num(details.and_then(|d| d.get("cached_tokens")))
        .or(num(u.get("prompt_cache_hit_tokens")))
        .unwrap_or(0.0);
    let written = num(details.and_then(|d| d.get("cache_write_tokens"))).unwrap_or(0.0);
    let prompt =
        num(u.get("prompt_tokens")).unwrap_or_else(|| num(u.get("prompt_cache_miss_tokens")).unwrap_or(0.0) + cached);
    let mut usage = Usage {
        input: count(prompt - cached - written),
        output: count(num(u.get("completion_tokens")).unwrap_or(0.0)),
        cache_read: count(cached),
        cache_write: count(written),
        cost: 0.0,
    };
    usage.cost = reported_cost(Some(u)).unwrap_or_else(|| compute_cost(model, &usage));
    usage
}

#[async_trait::async_trait]
impl Provider for OpenAiChatProvider {
    fn api(&self) -> &str {
        "openai-chat"
    }
    fn stream(&self, req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        generator(move |y| run(req, y))
    }
}

/// Key of a streamed call: its `index`, or its id when the server sent no index.
#[derive(Debug, Clone, PartialEq)]
enum CallKey {
    Index(String),
    Id(String),
}

/// A tool call being streamed in.
#[derive(Debug)]
struct Call {
    key: CallKey,
    /// Its place in the reply's content.
    index: usize,
    json: String,
    /// Braces open outside strings, counted chunk by chunk.
    depth: i64,
    in_str: bool,
    esc: bool,
}

#[cfg(test)]
thread_local! {
    /// Parses of tool arguments longer than 4096 bytes (the #62 test counts them).
    static LARGE_PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

async fn run(req: Request, y: Yielder) -> anyhow::Result<()> {
    let model = &req.model;
    let client = openai_client(&req);
    let router = openrouter(model);
    let mut params = JsonObject::new();
    params.insert("model".into(), json!(model.id));
    let mut messages = vec![json!({ "role": "system", "content": req.system })];
    messages.extend(to_chat_messages(&req.messages, model, !req.tools.is_empty())?);
    params.insert("messages".into(), Value::Array(messages));
    params.insert("stream".into(), json!(true));
    params.insert("stream_options".into(), json!({ "include_usage": true }));
    // Local servers read max_tokens (Ollama ignores the other); OpenAI's reasoning models refuse it.
    let max_tokens = req.max_tokens.unwrap_or(model.max_output);
    let key = if model.server.is_some() {
        "max_tokens"
    } else {
        "max_completion_tokens"
    };
    params.insert(key.into(), json!(max_tokens));
    if !req.tools.is_empty() {
        let tools = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": { "name": t.name, "description": t.description, "parameters": t.parameters },
                })
            })
            .collect();
        params.insert("tools".into(), Value::Array(tools));
    }
    let sampling = sampling_fields(
        req.sampling.as_ref(),
        &["temperature", "top_p", "frequency_penalty", "presence_penalty"],
    );
    for (k, v) in sampling {
        params.insert(k, v);
    }

    let router_reasoning = params.get("reasoning").cloned();
    // `(params.reasoning_effort ?? routerReasoning?.effort) !== undefined`: the caller chose an effort.
    let effort_given = match params.get("reasoning_effort") {
        Some(v) if !v.is_null() => true,
        _ => router_reasoning
            .as_ref()
            .and_then(Value::as_object)
            .is_some_and(|o| o.contains_key("effort")),
    };
    // "off" is sent too: servers such as vLLM think by default unless told "none".
    let level = if effort_given || model.reasoning != Some(true) {
        None
    } else {
        req.thinking
    };
    // llama-server before b10121 ignores reasoning_effort; its templates (Qwen3) read enable_thinking. The
    // caller's kwargs win.
    if level == Some(ThinkingLevel::Off) && model.server == Some(LocalServer::Llamacpp) {
        let mut kwargs = JsonObject::new();
        kwargs.insert("enable_thinking".into(), json!(false));
        if let Some(Value::Object(given)) = params.get("chat_template_kwargs") {
            for (k, v) in given {
                kwargs.insert(k.clone(), v.clone());
            }
        }
        params.insert("chat_template_kwargs".into(), Value::Object(kwargs));
    }

    let mut out = Arc::new(new_reply(model));
    y.emit(StreamEvent::Start { partial: out.clone() }).await;
    let mut thinking: Option<usize> = None;
    let mut details_block: Option<usize> = None;
    let mut details: Vec<Value> = Vec::new();
    let mut text: Option<usize> = None;
    let mut calls: Vec<Call> = Vec::new();
    let mut ended = false;

    let result: anyhow::Result<()> = async {
        let scope = replay_scope(model);
        let (client, signal, params, router_reasoning) = (&client, &req.signal, &params, &router_reasoning);
        let (mut src, mut next) = with_effort(&scope, level, |effort| {
            let mut body = params.clone();
            if let Some(effort) = effort {
                if router {
                    let mut r = JsonObject::new();
                    r.insert("effort".into(), json!(effort));
                    if let Some(Value::Object(given)) = router_reasoning {
                        for (k, v) in given {
                            r.insert(k.clone(), v.clone());
                        }
                    }
                    body.insert("reasoning".into(), Value::Object(r));
                } else {
                    body.insert("reasoning_effort".into(), json!(effort));
                }
            }
            async move {
                let resp = client
                    .post("/chat/completions", &Value::Object(body), &[], signal)
                    .await?;
                let mut src = EventSource::new(resp, true);
                // A refused effort can arrive as the first event of an HTTP 200 stream (vLLM).
                let first = src.next_openai(signal).await?;
                Ok((src, first))
            }
        })
        .await?;
        loop {
            // The SDK's stream ends quietly when the signal aborts it mid-reply.
            let Next::Item(chunk) = next else { break };
            if let Some(u) = chunk.get("usage").filter(|u| js_truthy(u)) {
                Arc::make_mut(&mut out).usage = parse_chat_usage(model, u);
                ended = true;
            }
            if let Some(choice) = chunk.get("choices").and_then(|c| c.get(0)).filter(|c| js_truthy(c)) {
                let empty = Value::Object(JsonObject::new());
                let delta = choice.get("delta").filter(|d| d.is_object()).unwrap_or(&empty);
                let reasoning_details = delta.get("reasoning_details").and_then(Value::as_array);
                if router && reasoning_details.is_some_and(|d| !d.is_empty()) {
                    let o = Arc::make_mut(&mut out);
                    let i = *details_block.get_or_insert_with(|| {
                        o.content.push(AssistantContent::Thinking(ThinkingContent {
                            thinking: String::new(),
                            signature: None,
                            redacted: Some(true),
                            reasoning_field: None,
                        }));
                        o.content.len() - 1
                    });
                    details.extend(reasoning_details.into_iter().flatten().cloned());
                    // Consecutive chunks stay in their original order and shape.
                    if let AssistantContent::Thinking(b) = &mut o.content[i] {
                        b.signature = Some(js_stringify(&Value::Array(details.clone())));
                    }
                }
                let reasoning_content = delta.get("reasoning_content").filter(|v| !v.is_null());
                let r = reasoning_content.or_else(|| delta.get("reasoning").filter(|v| !v.is_null()));
                if let Some(r) = r.filter(|r| js_truthy(r)) {
                    let r = js_string(Some(r));
                    let i = {
                        let o = Arc::make_mut(&mut out);
                        let i = *thinking.get_or_insert_with(|| {
                            let field = if reasoning_content.is_some() {
                                "reasoning_content"
                            } else {
                                "reasoning"
                            };
                            o.content.push(AssistantContent::Thinking(ThinkingContent {
                                thinking: String::new(),
                                signature: None,
                                redacted: None,
                                reasoning_field: Some(field.into()),
                            }));
                            o.content.len() - 1
                        });
                        if let AssistantContent::Thinking(b) = &mut o.content[i] {
                            b.thinking.push_str(&r);
                        }
                        i
                    };
                    y.emit(StreamEvent::ThinkingDelta {
                        index: i,
                        delta: r,
                        partial: out.clone(),
                    })
                    .await;
                }
                if delta.get("refusal").is_some_and(js_truthy) {
                    Arc::make_mut(&mut out).stop_reason = StopReason::Refusal;
                }
                let prose = delta
                    .get("content")
                    .filter(|v| !v.is_null())
                    .or_else(|| delta.get("refusal").filter(|v| !v.is_null()));
                if let Some(prose) = prose.filter(|p| js_truthy(p)) {
                    let prose = js_string(Some(prose));
                    let i = {
                        let o = Arc::make_mut(&mut out);
                        let i = *text.get_or_insert_with(|| {
                            o.content.push(AssistantContent::text(""));
                            o.content.len() - 1
                        });
                        if let AssistantContent::Text(b) = &mut o.content[i] {
                            b.text.push_str(&prose);
                        }
                        i
                    };
                    y.emit(StreamEvent::TextDelta {
                        index: i,
                        delta: prose,
                        partial: out.clone(),
                    })
                    .await;
                }
                for tc in delta.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
                    let function = tc.get("function");
                    let name = function
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .filter(|n| !n.is_empty());
                    let id = tc.get("id").and_then(Value::as_str).filter(|i| !i.is_empty());
                    let tc_index = tc.get("index").filter(|i| !i.is_null());
                    let block_of = |c: &Call| match &out.content[c.index] {
                        AssistantContent::ToolCall(b) => (b.id.clone(), b.name.clone()),
                        _ => (String::new(), String::new()),
                    };
                    // Without an index (some servers), match by a non-empty id, else continue the latest call
                    // unless this one starts another: a new id, or a name after a call with another name or
                    // with complete arguments.
                    let last = calls.len().checked_sub(1);
                    let starts_another = name.is_some_and(|name| {
                        last.is_some_and(|l| {
                            let (_, last_name) = block_of(&calls[l]);
                            !last_name.is_empty()
                                && (name != last_name || (!calls[l].json.is_empty() && calls[l].depth == 0))
                        })
                    });
                    let found = match tc_index {
                        Some(ix) => {
                            let key = CallKey::Index(js_stringify(ix));
                            calls.iter().position(|c| c.key == key)
                        }
                        None => {
                            let by_id = id.and_then(|id| calls.iter().position(|c| block_of(c).0 == id));
                            by_id.or(if id.is_some() || starts_another { None } else { last })
                        }
                    };
                    let c = match found {
                        Some(c) => {
                            // The name came after the id.
                            if let Some(name) = name
                                && let AssistantContent::ToolCall(b) =
                                    &mut Arc::make_mut(&mut out).content[calls[c].index]
                                && b.name.is_empty()
                            {
                                b.name = name.to_string();
                            }
                            c
                        }
                        None => {
                            let block_id = match id {
                                Some(id) => id.to_string(),
                                None => format!(
                                    "call_{}",
                                    tc_index
                                        .map(|i| js_string(Some(i)))
                                        .unwrap_or_else(|| calls.len().to_string())
                                ),
                            };
                            let block_name = name.unwrap_or_default().to_string();
                            let index = {
                                let o = Arc::make_mut(&mut out);
                                o.content.push(AssistantContent::ToolCall(ToolCall {
                                    id: block_id.clone(),
                                    name: block_name.clone(),
                                    input: JsonObject::new(),
                                    raw_input: None,
                                }));
                                o.content.len() - 1
                            };
                            let key = match tc_index {
                                Some(ix) => CallKey::Index(js_stringify(ix)),
                                None => CallKey::Id(block_id.clone()),
                            };
                            let call = Call {
                                key,
                                index,
                                json: String::new(),
                                depth: 0,
                                in_str: false,
                                esc: false,
                            };
                            // `Map.set`: an existing key keeps its place and gets the new call.
                            let c = match calls.iter().position(|c| c.key == call.key) {
                                Some(p) => {
                                    calls[p] = call;
                                    p
                                }
                                None => {
                                    calls.push(call);
                                    calls.len() - 1
                                }
                            };
                            y.emit(StreamEvent::ToolcallStart {
                                index,
                                id: block_id,
                                name: block_name,
                                partial: out.clone(),
                            })
                            .await;
                            c
                        }
                    };
                    let args = function.and_then(|f| f.get("arguments")).filter(|a| js_truthy(a));
                    if let Some(args) = args {
                        let args = js_string(Some(args));
                        let call = &mut calls[c];
                        call.json.push_str(&args);
                        // Back to 0 once the arguments are complete, so the check above never re-parses the
                        // whole string on every chunk.
                        for ch in args.chars() {
                            if call.esc {
                                call.esc = false;
                            } else if call.in_str {
                                call.esc = ch == '\\';
                                call.in_str = ch != '"';
                            } else if ch == '"' {
                                call.in_str = true;
                            } else if ch == '{' {
                                call.depth += 1;
                            } else if ch == '}' {
                                call.depth -= 1;
                            }
                        }
                        let index = call.index;
                        y.emit(StreamEvent::ToolcallDelta {
                            index,
                            delta: args,
                            partial: out.clone(),
                        })
                        .await;
                    }
                }
                if let Some(finish) = choice.get("finish_reason").filter(|f| js_truthy(f)) {
                    ended = true;
                    match finish.as_str() {
                        Some("length") => Arc::make_mut(&mut out).stop_reason = StopReason::MaxTokens,
                        Some("content_filter") => Arc::make_mut(&mut out).stop_reason = StopReason::Refusal,
                        _ => {}
                    }
                }
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
    o.mark_if_cut(ended); // neither a finish reason nor the usage chunk
    for c in &calls {
        #[cfg(test)]
        if c.json.len() > 4096 {
            LARGE_PARSES.with(|n| n.set(n.get() + 1));
        }
        let (input, raw_input) = parse_tool_input(&c.json);
        if let AssistantContent::ToolCall(b) = &mut o.content[c.index] {
            b.input = input;
            if raw_input.is_some() {
                b.raw_input = raw_input;
            }
        }
    }
    if o.stop_reason == StopReason::Stop && !calls.is_empty() {
        o.stop_reason = StopReason::ToolUse;
    }
    y.emit(StreamEvent::Done {
        message: Arc::unwrap_or_clone(out),
    })
    .await;
    Ok(())
}

static DEEPSEEK: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    Regex::new(r"^deepseek-(flash|v4(?:\.1)?-flash)$").unwrap()
});

fn image_part(media_type: &str, data: &str) -> Value {
    json!({ "type": "image_url", "image_url": { "url": format!("data:{media_type};base64,{data}") } })
}

/// The history as Chat messages (`toChatMessages`).
fn to_chat_messages(messages: &[Message], model: &ModelInfo, has_tools: bool) -> anyhow::Result<Vec<Value>> {
    let id = model.id.rsplit('/').next().unwrap_or_default();
    // DeepSeek needs all reasoning with tools; MiMo needs it on tool-call messages.
    let deepseek_model = DEEPSEEK.is_match(id);
    let deepseek = has_tools && deepseek_model;
    let mimo = id == "mimo-v2.6-flash";
    let router = openrouter(model);
    let mut out: Vec<Value> = Vec::new();
    let mut pending_images: Vec<Value> = Vec::new();
    for m in messages {
        match m {
            Message::User(u) => {
                let content: Vec<Value> = u
                    .content
                    .iter()
                    .map(|c| match c {
                        UserContent::Text(t) => json!({ "type": "text", "text": t.text }),
                        UserContent::Image(i) => image_part(&i.media_type, &i.data),
                    })
                    .collect();
                out.push(json!({ "role": "user", "content": content }));
            }
            Message::Assistant(a) => {
                let text = a.text();
                let tools: Vec<&ToolCall> = a.tool_calls().collect();
                let replay: Vec<&ThinkingContent> = if can_replay(a, model) {
                    a.content
                        .iter()
                        .filter_map(|c| match c {
                            AssistantContent::Thinking(t) => Some(t),
                            _ => None,
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                let reasoning_text: String = replay
                    .iter()
                    .filter(|c| c.redacted != Some(true))
                    .map(|c| c.thinking.as_str())
                    .collect();
                let mut reasoning_details = Vec::new();
                if router {
                    for c in &replay {
                        if let (Some(true), Some(sig)) = (c.redacted, c.signature.as_deref().filter(|s| !s.is_empty()))
                        {
                            match crate::sse::parse_json(sig)? {
                                Value::Array(items) => reasoning_details.extend(items),
                                other => reasoning_details.push(other),
                            }
                        }
                    }
                }
                let mut reasoning = JsonObject::new();
                if router {
                    if !reasoning_details.is_empty() {
                        reasoning.insert("reasoning_details".into(), Value::Array(reasoning_details));
                    } else if !reasoning_text.is_empty() {
                        reasoning.insert("reasoning".into(), json!(reasoning_text));
                    }
                } else if deepseek || (mimo && !tools.is_empty()) {
                    reasoning.insert("reasoning_content".into(), json!(reasoning_text));
                } else if !deepseek_model
                    && !mimo
                    && !reasoning_text.is_empty()
                    && let Some(field) = replay.first().and_then(|c| c.reasoning_field.as_deref())
                {
                    reasoning.insert(field.into(), json!(reasoning_text));
                }
                // A turn of thinking alone (max_tokens spent on it) is kept when its reasoning is sent back.
                let has_reasoning = reasoning.values().any(|v| match v {
                    Value::String(s) => !s.is_empty(),
                    Value::Array(a) => !a.is_empty(),
                    _ => false,
                });
                if text.is_empty() && tools.is_empty() && !has_reasoning {
                    continue;
                }
                let mut msg = JsonObject::new();
                msg.insert("role".into(), json!("assistant"));
                let content = if !text.is_empty() {
                    json!(text)
                } else if !tools.is_empty() {
                    Value::Null
                } else {
                    json!("")
                };
                msg.insert("content".into(), content);
                msg.extend(reasoning);
                if !tools.is_empty() {
                    let calls = tools
                        .iter()
                        .map(|t| {
                            let arguments =
                                t.raw_input.clone().unwrap_or_else(|| js_stringify(&Value::Object(t.input.clone())));
                            json!({ "id": t.id, "type": "function", "function": { "name": t.name, "arguments": arguments } })
                        })
                        .collect();
                    msg.insert("tool_calls".into(), Value::Array(calls));
                }
                out.push(Value::Object(msg));
            }
            Message::Tool(t) => {
                // Tool messages are text-only; replay images in a follow-up user message.
                let text = t
                    .content
                    .iter()
                    .filter_map(UserContent::as_text)
                    .collect::<Vec<_>>()
                    .join("\n");
                let content = if !text.is_empty() {
                    text
                } else if t.is_error {
                    "error".into()
                } else {
                    "ok".into()
                };
                out.push(json!({ "role": "tool", "tool_call_id": t.tool_call_id, "content": content }));
                for c in &t.content {
                    if let UserContent::Image(i) = c {
                        pending_images.push(image_part(&i.media_type, &i.data));
                    }
                }
                continue;
            }
        }
        flush(&mut out, &mut pending_images);
    }
    flush(&mut out, &mut pending_images);
    Ok(out)
}

/// Images from tool results, as a user message after all tool messages of a turn: inserted before
/// the message just pushed when that one is not a tool message.
fn flush(out: &mut Vec<Value>, pending: &mut Vec<Value>) {
    if pending.is_empty() {
        return;
    }
    let mut content = vec![json!({ "type": "text", "text": "Images from tool results:" })];
    content.append(pending);
    let msg = json!({ "role": "user", "content": content });
    match out.last() {
        Some(last) if last.get("role").and_then(Value::as_str) != Some("tool") => {
            let at = out.len() - 1;
            out.insert(at, msg);
        }
        _ => out.push(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Fetch;
    use futures::StreamExt;
    use futures::future::BoxFuture;

    struct Replay(String);

    impl Fetch for Replay {
        fn send(&self, _req: reqwest::Request) -> BoxFuture<'static, anyhow::Result<reqwest::Response>> {
            let body = self.0.clone();
            Box::pin(async move {
                let resp = http::Response::builder()
                    .status(200)
                    .header("content-type", "text/event-stream")
                    .body(body)?;
                Ok(reqwest::Response::from(resp))
            })
        }
    }

    fn chunk(delta: Value, finish: Option<&str>) -> Value {
        json!({ "id": "1", "object": "chat.completion.chunk", "created": 0, "model": "m",
                "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] })
    }

    /// TS: "openai-chat: an index-less call that repeats its name is not parsed again on every chunk (#62)".
    #[tokio::test]
    async fn an_index_less_call_that_repeats_its_name_is_not_parsed_again_on_every_chunk() {
        let model: ModelInfo = serde_json::from_value(json!({
            "id": "m", "provider": "local", "api": "openai-chat", "baseUrl": "http://x/v1",
            "contextWindow": 100000, "maxOutput": 1000
        }))
        .unwrap();
        // About 20 KB of code with braces, quotes and backslashes inside the string, in 3-character chunks
        // that split its escapes.
        let content = "if (a) { s = \"}\\\"{\\\\\"; }\n".repeat(800);
        let args = serde_json::to_string(&json!({ "path": "a.js", "content": content })).unwrap();
        let chars: Vec<char> = args.chars().collect();
        let pieces: Vec<String> = chars.chunks(3).map(|c| c.iter().collect()).collect();
        let write = |args: &str, id: Option<&str>| {
            let mut call = json!({ "function": { "name": "write", "arguments": args } });
            if let Some(id) = id {
                call["id"] = json!(id);
                call["type"] = json!("function");
            }
            chunk(json!({ "tool_calls": [call] }), None)
        };
        let mut chunks = vec![write(&pieces[0], Some("a"))];
        chunks.extend(pieces[1..].iter().map(|p| write(p, None)));
        chunks.push(write(r#"{"path":"b"}"#, None));
        chunks.push(chunk(json!({}), Some("tool_calls")));
        let body: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect::<String>() + "data: [DONE]\n\n";
        let req = Request {
            model,
            api_key: None,
            system: String::new(),
            messages: vec![Message::user(vec![UserContent::text("x")])],
            tools: vec![],
            thinking: None,
            max_tokens: None,
            max_retries: Some(0),
            sampling: None,
            signal: Default::default(),
            fetch: Some(Arc::new(Replay(body))),
        };
        LARGE_PARSES.with(|n| n.set(0));
        let mut stream = OpenAiChatProvider.stream(req);
        let mut done = None;
        while let Some(ev) = stream.next().await {
            if let StreamEvent::Done { message } = ev.unwrap() {
                done = Some(message);
            }
        }
        let message = done.unwrap();
        let got: Vec<(String, Value)> = message
            .tool_calls()
            .map(|c| (c.name.clone(), Value::Object(c.input.clone())))
            .collect();
        assert_eq!(
            got,
            vec![
                ("write".to_string(), json!({ "path": "a.js", "content": content })),
                ("write".to_string(), json!({ "path": "b" })),
            ]
        );
        // Only the final parse of the first call's arguments.
        assert_eq!(LARGE_PARSES.with(|n| n.get()), 1);
    }
}
