//! Which replies may hand their opaque provider state (signed thinking, encrypted reasoning) back to a
//! model (`packages/ai/src/replay.ts`).

use sha2::{Digest, Sha256};

use crate::types::{AssistantMessage, ModelInfo};

/// Digest of API, provider, model and endpoint: never the URL or a credential itself (`replayScope`).
pub fn replay_scope(m: &ModelInfo) -> String {
    let default_base = if m.api == "anthropic" {
        "https://api.anthropic.com"
    } else {
        "https://api.openai.com/v1"
    };
    let base = m.base_url.as_deref().unwrap_or(default_base);
    let json = crate::json::stringify(&[m.api.as_str(), m.provider.as_str(), m.id.as_str(), base]);
    format!("{:x}", Sha256::digest(json.as_bytes()))
}

/// Was `m` produced by exactly this API, provider, model and endpoint (`canReplay`)?
pub fn can_replay(m: &AssistantMessage, target: &ModelInfo) -> bool {
    m.api == target.api
        && m.provider == target.provider
        && m.model == target.id
        && m.replay_scope.as_deref() == Some(replay_scope(target).as_str())
}

/// An empty reply from `model`, stamped with its replay scope (`newReply`).
pub fn new_reply(model: &ModelInfo) -> AssistantMessage {
    AssistantMessage {
        replay_scope: Some(replay_scope(model)),
        ..AssistantMessage::new(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, provider: &str, api: &str, base_url: Option<&str>) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            provider: provider.into(),
            api: api.into(),
            base_url: base_url.map(Into::into),
            context_window: 1,
            max_output: 1,
            price: None,
            reasoning: None,
            images: None,
            headers: None,
            server: None,
        }
    }

    /// Digests computed by the TS `replayScope`: a mismatch would silently stop replay on resumed sessions.
    #[test]
    fn digests_match_the_typescript() {
        let cases = [
            (
                model("claude-x", "anthropic", "anthropic", None),
                "8b5495cd5c73c1687cd65ccc4dc9b91342fc8047959ee95bbb89a8c86f0d877a",
            ),
            (
                model("gpt-5.5", "openai", "openai-responses", None),
                "c730144b89290932f95a5e0646b84e27d6d6243c89170356a05105627a532b42",
            ),
            (
                model(
                    "deepseek/deepseek-v4.1-flash",
                    "openrouter",
                    "openai-chat",
                    Some("https://openrouter.ai/api/v1"),
                ),
                "6b434426847af5de7d11773b673118f9925f8285bfe8beab6a91ebf4ad9c9dc1",
            ),
            (
                model("日本語\"モデル", "p\\q", "openai-chat", Some("http://x/v1")),
                "bbb018574cfee8ed0eab84fd0906aca618a32cc76725f295d63ffad8faabb4b0",
            ),
        ];
        for (m, want) in cases {
            assert_eq!(replay_scope(&m), want, "{}", m.id);
        }
    }

    #[test]
    fn replay_needs_the_same_scope() {
        let m = model("m", "p", "openai-chat", Some("http://x/v1"));
        let reply = new_reply(&m);
        assert!(can_replay(&reply, &m));
        assert!(!can_replay(
            &reply,
            &model("m", "p", "openai-chat", Some("http://y/v1"))
        ));
        assert!(!can_replay(&AssistantMessage::new(&m), &m));
    }
}
