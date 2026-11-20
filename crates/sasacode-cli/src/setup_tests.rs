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

fn no_current() -> CurrentModel {
    Arc::new(|| None)
}

#[test]
fn a_model_named_by_the_model_is_limited_to_the_providers_the_user_chose() {
    let picked = vec!["anthropic/claude-x".into(), "ollama/q".into()];
    let resolve = limited_resolve(any_model(), providers(), picked, no_current());
    assert!(resolve("anthropic/claude-x").is_ok());
    assert!(resolve("ollama/qwen").is_ok());
    // A key that is set (for another tool, say) does not make it one the user chose.
    for spec in ["keyed/m", "keyless/m"] {
        let e = resolve(spec).unwrap_err().to_string();
        assert!(
            e.starts_with(&format!("\"{spec}\" is not on one of the providers you chose")),
            "{e}"
        );
        let usable: Vec<&str> = e.split("one of: ").nth(1).unwrap().split(", ").collect();
        assert_eq!(usable, ["anthropic", "ollama"], "{e}");
    }
}

#[test]
fn the_models_the_user_chose_come_from_the_config_in_effect_and_the_starting_model() {
    let global = config(json!({
        "model": "anthropic/claude-x",
        "judgeModel": "judge/j",
        "models": ["listed/m"],
        "modelOverrides": { "overridden/m": { "contextWindow": 1000 } },
        "plugins": { "settings": { "p": { "model": "plugin/m" } } },
        "providers": { "only-set-up": { "baseUrl": "http://127.0.0.1:9/v1" } },
    }));
    assert_eq!(
        chosen_models(&global, "anthropic/claude-x"),
        ["anthropic/claude-x", "judge/j", "listed/m", "overridden/m", "plugin/m"]
    );
    // The model the run starts with (-m, or the default) is one of them.
    assert_eq!(
        chosen_models(&config(json!({ "model": "ollama/q" })), "flagged/m"),
        ["flagged/m", "ollama/q"]
    );
    assert_eq!(chosen_models(&Config::new(), "flagged/m"), ["flagged/m"]);
}

#[test]
fn a_model_the_flag_names_is_one_a_subagent_can_use() {
    let picked = chosen_models(&config(json!({ "model": "anthropic/claude-x" })), "keyed/m");
    let resolve = limited_resolve(any_model(), providers(), picked, no_current());
    assert!(resolve("keyed/m").is_ok());
    assert!(resolve("anthropic/claude-x").is_ok());
    assert!(resolve("keyless/m").is_err());
}

#[test]
fn a_trusted_projects_judge_and_plugin_models_resolve_at_run_time() {
    let global = config(json!({ "model": "anthropic/claude-x" }));
    let project = config(json!({
        "providers": { "keyed": {} },
        "judgeModel": "keyed/judge",
        "plugins": { "settings": { "compaction": { "model": "keyless/sum" } } },
    }));
    let (safe, elevated) =
        crate::config::split_project_config_with(&project, &global, None, || "anthropic/claude-x".into());
    // Trusted: the elevated part applies, and its models are ones the session may use.
    let trusted = crate::config::merge_config(&crate::config::merge_config(&global, &safe), &elevated);
    let resolve = limited_resolve(
        any_model(),
        providers(),
        chosen_models(&trusted, "anthropic/claude-x"),
        no_current(),
    );
    assert!(resolve("keyed/judge").is_ok());
    assert!(resolve("keyless/sum").is_ok());
    // Untrusted: only what the user's own config chose.
    let untrusted = crate::config::merge_config(&global, &safe);
    let resolve = limited_resolve(
        any_model(),
        providers(),
        chosen_models(&untrusted, "anthropic/claude-x"),
        no_current(),
    );
    assert!(resolve("keyed/judge").is_err());
    assert!(resolve("keyless/sum").is_err());
}

#[test]
fn the_sessions_current_model_is_one_the_user_chose() {
    let current = Arc::new(RwLock::new("anthropic/claude-x".to_string()));
    let read: CurrentModel = {
        let current = current.clone();
        Arc::new(move || Some(current.read().unwrap().clone()))
    };
    let resolve = limited_resolve(any_model(), providers(), vec!["anthropic/claude-x".into()], read);
    assert!(resolve("keyed/m").is_err());
    // /model or -r switched to another provider.
    *current.write().unwrap() = "keyed/other".into();
    assert!(resolve("keyed/m").is_ok());
}

#[test]
fn on_a_router_only_the_company_of_the_chosen_model_is_allowed() {
    let picked = vec!["openrouter/anthropic/claude-x".into(), "openrouter/auto".into()];
    let resolve = limited_resolve(any_model(), providers(), picked, no_current());
    assert!(resolve("openrouter/anthropic/claude-x").is_ok());
    assert!(resolve("openrouter/anthropic/claude-y").is_ok());
    assert!(resolve("openrouter/auto").is_ok());
    for spec in [
        "openrouter/openai/gpt-x",
        "openrouter/auto/x",
        "openrouter/anthropicx/y",
    ] {
        let e = resolve(spec).unwrap_err().to_string();
        assert!(e.ends_with("one of: openrouter/anthropic/*, openrouter/auto"), "{e}");
    }
    // The current model on a router admits its own company the same way.
    let current: CurrentModel = Arc::new(|| Some("openrouter/google/gemini-x".into()));
    let resolve = limited_resolve(any_model(), providers(), Vec::new(), current);
    assert!(resolve("openrouter/google/gemini-y").is_ok());
    assert!(resolve("openrouter/openai/gpt-x").is_err());
}

#[test]
fn only_a_project_server_with_its_own_command_starts_in_the_project() {
    let global = config(json!({ "mcpServers": {
        "a": { "command": "g" }, "b": { "command": "g" }, "c": { "command": "g" }, "d": { "command": "g" },
        "e": { "url": "https://global.example" }, "f": { "command": "g" },
    } }));
    let project = config(json!({ "mcpServers": {
        "a": { "args": ["x"], "env": { "K": "v" } },
        "b": { "url": "" },
        "c": { "command": "p" },
        // Laid over a global command server, a url replaces what it starts: it connects there.
        "d": { "url": "https://mcp.example" },
        "e": { "url": "https://project.example" },
        "f": { "disabled": false },
        "g": { "url": "https://new.example" },
        "h": { "command": "p" },
    } }));
    let mut merged = crate::config::merge_config(&global, &project);
    project_servers_start_in_project(&mut merged, &project);
    let servers = &merged["mcpServers"];
    for name in ["a", "b", "d", "e", "f", "g"] {
        assert!(servers[name].get("cwd").is_none(), "{name}: {}", servers[name]);
    }
    assert_eq!(servers["c"]["cwd"], sasacode_mcp::PROJECT_CWD);
    assert_eq!(servers["h"]["cwd"], sasacode_mcp::PROJECT_CWD);
    assert_eq!(servers["d"], json!({ "url": "https://mcp.example" }));
    assert_eq!(servers["a"]["command"], "g");
}
