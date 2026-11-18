//! The OpenAI-style `reasoning_effort` means different things to different servers: vLLM takes "none"
//! to turn thinking off, Command Code refuses "none" but takes "max", older OpenAI models take no
//! effort at all. Each level therefore has a list of values to try, closest first, and the one a
//! server accepted is remembered so later requests go straight to it (`packages/ai/src/effort.ts`).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{LazyLock, Mutex};

use regex::Regex;

use crate::transport::{ApiError, js_string};
use crate::types::ThinkingLevel;

/// Values to send for a level, closest first; `None` leaves the field out (the server's default).
fn candidates(level: ThinkingLevel) -> &'static [Option<&'static str>] {
    match level {
        ThinkingLevel::Off => &[Some("none"), Some("minimal"), Some("low"), None],
        ThinkingLevel::Low => &[Some("low"), Some("minimal"), None],
        ThinkingLevel::Medium => &[Some("medium"), None],
        ThinkingLevel::High => &[Some("high"), Some("xhigh"), Some("medium"), None],
        ThinkingLevel::Xhigh => &[Some("xhigh"), Some("high"), None],
        ThinkingLevel::Max => &[Some("max"), Some("xhigh"), Some("high"), None],
    }
}

static ACCEPTED: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Default::default);

/// JS `.` without the `s` flag: anything but a line terminator.
const DOT: &str = r"[^\n\r\u2028\u2029]";

static ABOUT_HISTORY: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    Regex::new(r"(?i)reasoning_content|reasoning_details|signature|encrypted_content|messages\[|messages\.").unwrap()
});
static ABOUT_EFFORT: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    Regex::new(&format!(
        r"(?i)reasoning[_ .]effort|(?-u:\b)effort(?-u:\b)|(?:does not support|unsupported){DOT}*thinking"
    ))
    .unwrap()
});
static BAD_REQUEST_TYPE: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    Regex::new(r"(?i)BadRequest|invalid_request").unwrap()
});

/// A 400 about the effort itself (not about the prompt or the tools). vLLM sends it as the stream's
/// first event rather than as the HTTP status, so the error carries no status there.
pub fn is_effort_rejected(e: &anyhow::Error) -> bool {
    let Some(e) = e.downcast_ref::<ApiError>() else {
        return false;
    };
    let bad = e.status == Some(400)
        || e.status == Some(422)
        || (e.status.is_none()
            && (js_string(e.code.as_ref()) == "400" || BAD_REQUEST_TYPE.is_match(&js_string(e.error_type.as_ref()))));
    // `${e.param ?? ""} ${e.message}`
    let param = e
        .param
        .as_ref()
        .filter(|p| !p.is_null())
        .map(|p| js_string(Some(p)))
        .unwrap_or_default();
    let text = format!("{param} {}", e.message);
    bad && !ABOUT_HISTORY.is_match(&text) && ABOUT_EFFORT.is_match(&text)
}

/// Run `attempt` with the closest effort the server takes for `level`. `key` names the endpoint and
/// model; `thinking` `None` sends no effort at all.
pub async fn with_effort<T, F, Fut>(key: &str, thinking: Option<ThinkingLevel>, mut attempt: F) -> anyhow::Result<T>
where
    F: FnMut(Option<&'static str>) -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let Some(level) = thinking else {
        return attempt(None).await;
    };
    let list = candidates(level);
    let k = format!("{key} {}", level.as_str());
    let start = ACCEPTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&k)
        .copied()
        .unwrap_or(0);
    let mut i = start.min(list.len() - 1);
    loop {
        match attempt(list[i]).await {
            Ok(result) => {
                ACCEPTED.lock().unwrap_or_else(|e| e.into_inner()).insert(k, i);
                return Ok(result);
            }
            Err(e) => {
                if i >= list.len() - 1 || !is_effort_rejected(&e) {
                    return Err(e);
                }
            }
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn effort_rejections_are_told_apart() {
        let status =
            |s: u16, body: serde_json::Value| -> anyhow::Error { ApiError::openai_status(s, Some(body), None).into() };
        let stream = |error: serde_json::Value| -> anyhow::Error { ApiError::openai(None, Some(error), None).into() };
        assert!(is_effort_rejected(&status(
            400,
            json!({"error": {"message": "Unsupported value: 'reasoning_effort'"}})
        )));
        assert!(is_effort_rejected(&status(
            422,
            json!({"error": {"message": "effort must be one of low, high"}})
        )));
        assert!(!is_effort_rejected(&status(
            500,
            json!({"error": {"message": "reasoning_effort broke"}})
        )));
        // About the history, not the effort.
        assert!(!is_effort_rejected(&status(
            400,
            json!({"error": {"message": "messages[1].reasoning_content is missing for effort high"}})
        )));
        assert!(is_effort_rejected(&stream(
            json!({"message": "Unexpected reasoning effort high.", "code": 400})
        )));
        assert!(is_effort_rejected(&stream(
            json!({"message": "x does not support thinking", "type": "BadRequestError"})
        )));
        assert!(is_effort_rejected(&stream(
            json!({"message": "unsupported", "code": "400", "param": "reasoning.effort"})
        )));
        assert!(!is_effort_rejected(&stream(
            json!({"message": "reasoning effort high unsupported"})
        )));
        // "efforts" is not the word "effort".
        assert!(!is_effort_rejected(&status(
            400,
            json!({"error": {"message": "too many efforts"}})
        )));
        assert!(!is_effort_rejected(&anyhow::anyhow!("reasoning_effort")));
    }
}
