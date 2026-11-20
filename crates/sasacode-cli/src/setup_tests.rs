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

fn config(v: serde_json::Value) -> Config {
    v.as_object().unwrap().clone()
}

#[test]
fn a_model_named_by_the_model_is_limited_to_the_providers_the_user_chose() {
    let resolve = limited_resolve(any_model(), providers(), vec!["anthropic".into(), "ollama".into()]);
    assert!(resolve("anthropic/claude-x").is_ok());
    assert!(resolve("ollama/qwen").is_ok());
    // A key that is set (for another tool, say) does not make it one the user chose.
    for spec in ["keyed/m", "keyless/m"] {
        let e = resolve(spec).unwrap_err().to_string();
        assert!(
            e.starts_with(&format!(
                "\"{spec}\": {} is not one of the providers you chose",
                &spec[..spec.find('/').unwrap()]
            )),
            "{e}"
        );
        let usable: Vec<&str> = e.split("one of: ").nth(1).unwrap().split(", ").collect();
        assert_eq!(usable, ["anthropic", "ollama"], "{e}");
    }
}

#[test]
fn the_providers_the_user_chose_come_from_their_own_config_and_the_model_flag() {
    let global = config(json!({
        "model": "anthropic/claude-x",
        "judgeModel": "judge/j",
        "models": ["listed/m"],
        "modelOverrides": { "overridden/m": { "contextWindow": 1000 } },
        "plugins": { "settings": { "p": { "model": "plugin/m" } } },
        "providers": { "only-set-up": { "baseUrl": "http://127.0.0.1:9/v1" } },
    }));
    assert_eq!(
        chosen_providers(&global, None),
        ["anthropic", "judge", "listed", "overridden", "plugin"]
    );
    // The model flag adds its provider; the default model is not one of them once a model is named.
    assert_eq!(
        chosen_providers(&config(json!({ "model": "ollama/q" })), Some("flagged/m")),
        ["flagged", "ollama"]
    );
    assert_eq!(chosen_providers(&Config::new(), Some("flagged/m")), ["flagged"]);
    // With nothing named, the model a run defaults to.
    let default = crate::config::user_default_model(&Config::new());
    assert_eq!(
        chosen_providers(&Config::new(), None),
        [crate::config::provider_of(&default)]
    );
}

#[test]
fn a_model_the_flag_names_is_one_a_subagent_can_use() {
    let picked = chosen_providers(&config(json!({ "model": "anthropic/claude-x" })), Some("keyed/m"));
    let resolve = limited_resolve(any_model(), providers(), picked);
    assert!(resolve("keyed/m").is_ok());
    assert!(resolve("anthropic/claude-x").is_ok());
    assert!(resolve("keyless/m").is_err());
}

#[test]
fn only_a_project_server_with_its_own_command_or_url_starts_in_the_project() {
    let global = config(json!({ "mcpServers": {
        "a": { "command": "g" }, "b": { "command": "g" }, "c": { "command": "g" }, "d": { "command": "g" },
    } }));
    let project = config(json!({ "mcpServers": {
        "a": { "args": ["x"], "env": { "K": "v" } },
        "b": { "url": "" },
        "c": { "command": "p" },
        "d": { "url": "https://mcp.example" },
    } }));
    let mut merged = crate::config::merge_config(&global, &project);
    project_servers_start_in_project(&mut merged, &project);
    let servers = &merged["mcpServers"];
    assert!(servers["a"].get("cwd").is_none());
    assert!(servers["b"].get("cwd").is_none());
    assert_eq!(servers["c"]["cwd"], sasacode_mcp::PROJECT_CWD);
    assert_eq!(servers["d"]["cwd"], sasacode_mcp::PROJECT_CWD);
}
