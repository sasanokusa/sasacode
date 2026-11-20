//! The models a model may name itself (a subagent's `model`).

use serde_json::json;

use super::*;

fn providers() -> Providers {
    let mut list = builtin_providers().clone();
    let entry = |key_env: &str| {
        serde_json::from_value::<ProviderConfig>(
            json!({ "api": "openai-chat", "baseUrl": "http://127.0.0.1:9/v1", "apiKeyEnv": key_env }),
        )
        .unwrap()
    };
    // PATH is always set: a provider whose key is there counts as keyed.
    list.insert("keyed".into(), entry("PATH"));
    list.insert("keyless".into(), entry("SASACODE_TEST_NEVER_SET_6F1C"));
    Arc::new(RwLock::new(list))
}

fn any_model() -> Resolve {
    Arc::new(|spec: &str| {
        Ok(serde_json::from_value(json!({
            "id": spec, "provider": "p", "api": "openai-chat", "contextWindow": 1000, "maxOutput": 100
        }))?)
    })
}

#[test]
fn a_model_named_by_the_model_is_limited_to_providers_in_use_or_with_a_key() {
    let resolve = limited_resolve(any_model(), providers(), vec!["anthropic".into(), "ollama".into()]);
    assert!(resolve("anthropic/claude-x").is_ok());
    assert!(resolve("ollama/qwen").is_ok());
    assert!(resolve("keyed/m").is_ok());
    let e = resolve("keyless/m").unwrap_err().to_string();
    assert!(
        e.starts_with("\"keyless/m\": keyless is not a provider this session uses"),
        "{e}"
    );
    let usable = e.split("one of: ").nth(1).unwrap();
    let usable: Vec<&str> = usable.split(", ").collect();
    assert!(
        usable.contains(&"anthropic") && usable.contains(&"ollama") && usable.contains(&"keyed"),
        "{e}"
    );
    assert!(!usable.contains(&"keyless"), "{e}");
}
