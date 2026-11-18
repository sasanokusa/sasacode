//! Provider-neutral messages and the model API adapters (`packages/ai`).

pub mod anthropic;
pub mod effort;
pub mod format;
pub mod http;
pub mod json;
pub mod openai_chat;
pub mod openai_responses;
pub mod provider;
pub mod replay;
pub mod sse;
pub mod transport;
pub mod types;

pub use format::*;
pub use provider::*;
pub use replay::*;
pub use types::*;

/// Registers the three built-in adapters once (anthropic, openai-chat, openai-responses).
pub fn register_builtins() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Registered under the lock-free path: a plugin registering later replaces them.
        let builtins: [(&str, std::sync::Arc<dyn Provider>); 3] = [
            ("anthropic", std::sync::Arc::new(anthropic::AnthropicProvider)),
            ("openai-chat", std::sync::Arc::new(openai_chat::OpenAiChatProvider)),
            (
                "openai-responses",
                std::sync::Arc::new(openai_responses::OpenAiResponsesProvider),
            ),
        ];
        for (api, p) in builtins {
            provider::register_provider_if_absent(api, p);
        }
    });
}

/// Register an implementation for an api name (also used by tests to install a replay provider) (`registerApi`).
pub fn register_api(api: &str, p: std::sync::Arc<dyn Provider>) {
    register_builtins();
    provider::register_provider(api, p);
}
