//! compaction (`packages/bundled/src/compaction.ts`): summarizes older history when the context
//! fills up (or at `threshold` of the window), and on /compact.

use futures::future::BoxFuture;
use serde_json::Value;

use sasacode_agent::agent::{content_chars, response_room};
use sasacode_ai::{AssistantContent, Message, ModelInfo, UserContent, now_ms};
use sasacode_host::t;
use sasacode_plugin_api::{Command, CompleteOptions, ContextLimitResult, NotifyLevel, PluginApi, hook};

use crate::repair::js::{js_trim, utf16_len, utf16_prefix};

const SYSTEM: &str = "You write handoff summaries of coding sessions so the work can continue in a fresh context. Include: the user's goals and constraints, decisions made, files read or changed (paths), commands run and their outcomes, the current state, and the next steps. Be specific and concise.";

/// Characters outside ASCII (in UTF-16 units): local tokenizers (Qwen, Gemma, Llama) make about a
/// token of each, not a quarter of one.
fn wide(s: &str) -> usize {
    s.chars().filter(|c| !c.is_ascii()).map(char::len_utf16).sum()
}

fn wide_value(v: &Value) -> usize {
    match v {
        Value::String(s) => wide(s),
        Value::Array(a) => a.iter().map(wide_value).sum(),
        Value::Object(o) => o.iter().map(|(k, v)| wide_entry(k, v)).sum(),
        _ => 0,
    }
}

/// A key and its value, as the replacer of `estimate` in the TS left them: a "data" string over 64
/// units (base64, wherever it is) counts as empty.
fn wide_entry(k: &str, v: &Value) -> usize {
    match v {
        Value::String(s) if k == "data" && s.encode_utf16().nth(64).is_some() => wide(k),
        _ => wide(k) + wide_value(v),
    }
}

/// `wide(JSON.stringify(m.content))` with an image's base64 (over 64 characters) left out: the
/// JSON around the strings is ASCII, so only the strings count.
fn wide_content(m: &Message) -> usize {
    let user = |c: &[UserContent]| -> usize {
        c.iter()
            .map(|c| match c {
                UserContent::Text(t) => wide(&t.text),
                // Counting stops past 64 units: the base64 runs to megabytes.
                UserContent::Image(i) => {
                    wide(&i.media_type)
                        + if i.data.encode_utf16().nth(64).is_some() {
                            0
                        } else {
                            wide(&i.data)
                        }
                }
            })
            .sum()
    };
    match m {
        Message::User(u) => user(&u.content),
        Message::Tool(t) => user(&t.content),
        Message::Assistant(a) => a
            .content
            .iter()
            .map(|c| match c {
                AssistantContent::Text(t) => wide(&t.text),
                AssistantContent::Thinking(t) => {
                    wide(&t.thinking)
                        + t.signature.as_deref().map_or(0, wide)
                        + t.reasoning_field.as_deref().map_or(0, wide)
                }
                AssistantContent::ToolCall(c) => {
                    wide(&c.id)
                        + wide(&c.name)
                        + c.input.iter().map(|(k, v)| wide_entry(k, v)).sum::<usize>()
                        + c.raw_input.as_deref().map_or(0, wide)
                }
            })
            .sum(),
    }
}

/// Tokens of a message, weighed as summary_request weighs them: at 4 characters a token, the tail
/// kept of a Japanese session was 3 times its budget. The wide characters are looked for in what
/// content_chars counts, without an image's base64: going through megabytes of it took 0.6 s.
fn estimate(m: &Message) -> f64 {
    ((content_chars(&without_base64(m)) + 3 * wide_content(m)) as f64 / 4.0).ceil()
}

/// An image's base64 over 64 units as the empty string content_chars counts it as, so content_chars
/// does not read through it to find that out (the count is the same).
fn without_base64(m: &Message) -> std::borrow::Cow<'_, Message> {
    let long = |c: &UserContent| matches!(c, UserContent::Image(i) if i.data.encode_utf16().nth(64).is_some());
    let blank = |content: &mut Vec<UserContent>| {
        for c in content.iter_mut() {
            if long(c)
                && let UserContent::Image(i) = c
            {
                i.data = std::sync::Arc::from("");
            }
        }
    };
    match m {
        Message::User(u) if u.content.iter().any(long) => {
            let mut u = u.clone();
            blank(&mut u.content);
            std::borrow::Cow::Owned(Message::User(u))
        }
        Message::Tool(t) if t.content.iter().any(long) => {
            let mut t = t.clone();
            blank(&mut t.content);
            std::borrow::Cow::Owned(Message::Tool(t))
        }
        _ => std::borrow::Cow::Borrowed(m),
    }
}

fn clip(s: &str, n: usize) -> String {
    let len = utf16_len(s);
    if len > n {
        format!("{}… [{} more chars]", utf16_prefix(s, n), len - n)
    } else {
        s.to_string()
    }
}

fn user_text(c: &[UserContent]) -> String {
    c.iter()
        .map(|c| match c {
            UserContent::Text(t) => t.text.clone(),
            UserContent::Image(_) => "[image]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Plain-text transcript, so tool call/result pairing does not matter to the summarizer. Tool
/// results and call arguments are clipped (`transcript(messages, 2000, 500)` by default in the TS).
pub fn transcript(messages: &[Message], result_chars: usize, call_chars: usize) -> String {
    messages
        .iter()
        .map(|m| match m {
            Message::User(u) => format!("USER: {}", user_text(&u.content)),
            Message::Tool(t) => format!(
                "TOOL RESULT ({}{}): {}",
                t.tool_name,
                if t.is_error { ", error" } else { "" },
                clip(&user_text(&t.content), result_chars)
            ),
            Message::Assistant(a) => format!(
                "ASSISTANT: {}",
                a.content
                    .iter()
                    .map(|c| match c {
                        AssistantContent::Text(t) => t.text.clone(),
                        AssistantContent::ToolCall(c) => format!(
                            "[call {} {}]",
                            c.name,
                            clip(
                                &sasacode_ai::json::stringify(&Value::Object(c.input.clone())),
                                call_chars
                            )
                        ),
                        AssistantContent::Thinking(_) => String::new(),
                    })
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

const LEFT_OUT: &str = "[The start of the session is left out here, so that this request fits the context window.]\n\n";

fn user_message(text: String) -> Message {
    Message::User(sasacode_ai::UserMessage {
        content: vec![UserContent::text(text)],
        timestamp: now_ms(),
    })
}

/// The summary request, made to fit the window: servers such as vLLM and llama.cpp reject one whose
/// input plus max_tokens is over it, and a fixed 8,192-token summary broke compaction in the small
/// windows that need it most (#60). The summary gets about 15% of the window (as response_room gives
/// a reply), at most 8,192 tokens; the input gets the rest, estimated as the core sizes replies
/// (about 4 characters a token, plus 10%), except that a character outside ASCII counts as a token.
/// A transcript too long for that has its tool results and calls clipped harder first, which keeps
/// every step; if that is not enough, its oldest part is dropped. Instructions that then take more
/// than half the room are an error, and the history stays as it was.
fn summary_request(model: &ModelInfo, head: &[Message], instructions: &str) -> anyhow::Result<(u64, Message)> {
    let max_tokens = 8192.min(response_room(model));
    let max_chars =
        (((model.context_window as f64 - max_tokens as f64) / 1.1) * 4.0).floor() - utf16_len(SYSTEM) as f64 - 8.0;
    let ask = format!(
        "\n\n---\nSummarize the session above.{}",
        if instructions.is_empty() {
            String::new()
        } else {
            format!(" {instructions}")
        }
    );
    // Strings here are always well-formed: a cut inside a surrogate pair became U+FFFD (#88).
    let message = |text: &str| user_message(format!("{text}{ask}"));
    let size = |text: &str| (content_chars(&message(text)) + 3 * wide(&format!("{text}{ask}"))) as f64;
    let mut text = transcript(head, 2000, 500);
    if size(&text) > max_chars {
        text = transcript(head, 200, 100);
    }
    if size(&text) > max_chars {
        if size(LEFT_OUT) > max_chars / 2.0 {
            anyhow::bail!(
                "the /compact instructions leave too little room for the session in the context window; the history was left as it was"
            );
        }
        // The longest recent part that fits, found by halving, since what a character weighs varies (code, Japanese).
        let full: Vec<u16> = text.encode_utf16().collect();
        let tail = |n: usize| format!("{LEFT_OUT}{}", String::from_utf16_lossy(&full[full.len() - n..]));
        let mut keep = 0usize;
        let mut most = full.len();
        while keep < most {
            let mid = (keep + most).div_ceil(2);
            if size(&tail(mid)) > max_chars {
                most = mid - 1;
            } else {
                keep = mid;
            }
        }
        text = tail(keep);
    }
    Ok((max_tokens, message(&text)))
}

/// Index where the kept tail starts, with the tail under `keep_tokens`: a user message, or, when the
/// tail from one would keep less than half of that, the start of an assistant turn further back (its
/// tool calls and their results stay together, after it). A long run of tool calls after one prompt
/// has no user message within the budget: all of it was summarized, nothing of the latest steps was
/// kept as it was, and a small model then made up what those steps had done.
pub fn split_point(messages: &[Message], keep_tokens: f64) -> usize {
    let mut tokens = 0.0;
    let mut user = messages.len();
    let mut user_tokens = 0.0;
    let mut turn = messages.len();
    for (i, m) in messages.iter().enumerate().rev() {
        tokens += estimate(m);
        if tokens > keep_tokens {
            break;
        }
        match m {
            Message::User(_) => {
                user = i;
                user_tokens = tokens;
            }
            Message::Assistant(_) => turn = i,
            Message::Tool(_) => {}
        }
    }
    if user < messages.len() && user_tokens >= keep_tokens / 2.0 {
        user
    } else {
        user.min(turn)
    }
}

/// Index of the last assistant message whose tool calls have not all got their results yet (a
/// /compact while tools run), or the length: summarized away, the calls would leave their results,
/// which arrive later, without them (#118).
pub fn pending_calls(messages: &[Message]) -> usize {
    for (i, m) in messages.iter().enumerate().rev() {
        let Message::Assistant(a) = m else { continue };
        let answered: Vec<&str> = messages[i + 1..]
            .iter()
            .filter_map(|r| match r {
                Message::Tool(t) => Some(t.tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        return if a.tool_calls().any(|c| !answered.contains(&c.id.as_str())) {
            i
        } else {
            messages.len()
        };
    }
    messages.len()
}

/// `manual` (/compact) summarizes everything; automatic compaction keeps a recent tail verbatim.
pub async fn compact(api: &PluginApi, instructions: &str, manual: bool) -> anyhow::Result<bool> {
    let messages = api.session().messages();
    let model = api.agent().model();
    let keep = 40_000f64.min((model.context_window as f64 * 0.15).floor());
    let split = (if manual {
        messages.len()
    } else {
        split_point(&messages, keep)
    })
    .min(pending_calls(&messages));
    let head = &messages[..split];
    if head.len() < 2 {
        return Ok(false);
    }
    // One long model call with nothing streaming to the screen: say that it is under way.
    api.ui()
        .set_status("compaction", Some(t!("履歴を要約中（{n} 件）…", n = head.len())));
    let summary = async {
        let (max_tokens, message) = summary_request(&model, head, instructions)?;
        api.agent()
            .complete(CompleteOptions {
                system: SYSTEM.to_string(),
                max_tokens: Some(max_tokens),
                messages: vec![message],
                ..CompleteOptions::default()
            })
            .await
    }
    .await;
    api.ui().set_status("compaction", None);
    let summary = summary?;
    // An empty answer (the output spent on reasoning, a cut-off stream) must not replace the history.
    if js_trim(&summary).is_empty() {
        anyhow::bail!("the model returned an empty summary; the history was left as it was");
    }
    // Results that came in while it summarized are kept, after split; another replacement is not.
    // The TS compared the array's identity; here the history must still begin with what was summarized.
    let now = api.session().messages();
    if now.len() < messages.len() || now[..messages.len()] != messages[..] {
        anyhow::bail!("the history was replaced while it was being summarized; it was left as it was");
    }
    let mut next = vec![user_message(format!(
        "[Summary of the earlier conversation, compacted to save context]\n\n{summary}"
    ))];
    next.extend_from_slice(&now[split..]);
    api.session().replace_messages(next)?;
    api.ui().notify(
        &t!("コンテキストを圧縮しました（{n} 件のメッセージを要約）", n = head.len()),
        NotifyLevel::Info,
    );
    Ok(true)
}

/// `Number(v ?? d)`.
fn js_number(v: Option<&Value>, d: f64) -> f64 {
    match v {
        None | Some(Value::Null) => d,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::String(s)) => {
            let s = js_trim(s);
            if s.is_empty() {
                0.0
            } else {
                s.parse().unwrap_or(f64::NAN)
            }
        }
        Some(_) => f64::NAN,
    }
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let threshold = js_number(api.settings().get("threshold"), 0.8);
        let a = api.clone();
        api.on::<hook::ContextLimit, _, _>(move |_e, _c| {
            let a = a.clone();
            async move {
                Ok(Some(ContextLimitResult {
                    retry: compact(&a, "", false).await?,
                }))
            }
        });
        let a = api.clone();
        api.on::<hook::TurnEnd, _, _>(move |ev, _c| {
            let a = a.clone();
            async move {
                let u = &ev.message.usage;
                if (u.input + u.cache_read + u.cache_write + u.output) as f64
                    > a.agent().model().context_window as f64 * threshold
                {
                    compact(&a, "", false).await?;
                }
                Ok(None)
            }
        });
        let a = api.clone();
        api.register_command(
            Command::new(
                "compact",
                &t!("会話履歴を要約してコンテキストを空ける"),
                move |c| {
                    let a = a.clone();
                    async move {
                        if !compact(&a, &c.args, true).await? {
                            a.ui().notify(&t!("圧縮するほどの履歴がありません"), NotifyLevel::Info);
                        }
                        Ok(())
                    }
                },
            )
            .hint(&t!("[要約への指示]")),
        );
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repair::js::utf16_slice;

    #[test]
    fn clip_counts_utf16_units_and_keeps_text_well_formed() {
        assert_eq!(clip("abc", 5), "abc");
        assert_eq!(clip("abcdef", 2), "ab… [4 more chars]");
        assert_eq!(clip("a😀b", 2), "a\u{fffd}… [2 more chars]");
        assert_eq!(utf16_slice("😀", 1, 2), "\u{fffd}");
    }

    #[test]
    fn an_images_base64_is_counted_without_being_read_through() {
        let image = |data: &str| {
            UserContent::Image(sasacode_ai::ImageContent {
                media_type: "image/png".into(),
                data: std::sync::Arc::from(data),
            })
        };
        for data in ["", "short", &"A".repeat(64), &"A".repeat(65), &"A".repeat(10_000)] {
            let m = Message::User(sasacode_ai::UserMessage {
                content: vec![UserContent::text("look"), image(data)],
                timestamp: 0,
            });
            assert_eq!(content_chars(&without_base64(&m)), content_chars(&m), "{}", data.len());
        }
    }
}
