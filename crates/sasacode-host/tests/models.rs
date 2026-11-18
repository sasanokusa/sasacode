// packages/host/test/models.test.ts: the providers and models the app knows (moved out of the
// core's ai package): prices feed the cost line.
use indexmap::IndexMap;
use sasacode_ai::{LocalServer, Price, ProviderConfig, Usage, compute_cost};
use sasacode_host::{
    DEFAULT_CONTEXT_WINDOW, DEFAULT_MODEL, ModelOverride, builtin_providers, default_model_for_env, known_model,
    resolve_model,
};

fn provider(api: &str) -> ProviderConfig {
    ProviderConfig {
        api: api.to_string(),
        ..Default::default()
    }
}

fn providers(list: &[(&str, ProviderConfig)]) -> IndexMap<String, ProviderConfig> {
    list.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

/// `expect(a).toBeCloseTo(b)`: two decimal digits.
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.005
}

#[test]
fn a_priced_model_gets_a_cost_from_the_table() {
    let m = resolve_model(
        "openai/gpt-5.5",
        &providers(&[("openai", provider("openai-responses"))]),
        &IndexMap::new(),
    )
    .unwrap();
    let p = m.price.clone().unwrap();
    assert!(p.input > 0.0);
    let u = Usage {
        input: 1_000_000,
        output: 1_000_000,
        ..Default::default()
    };
    assert!(close(compute_cost(&m, &u), p.input + p.output));
    // Cache reads fall back to a tenth of the input price when the table lists none.
    let u = Usage {
        cache_read: 1_000_000,
        ..Default::default()
    };
    assert!(close(compute_cost(&m, &u), p.input * 0.1));
}

#[test]
fn a_providers_local_server_flag_reaches_the_model_whatever_the_provider_is_called() {
    // #58
    let ps = providers(&[
        (
            "myollama",
            ProviderConfig {
                base_url: Some("http://host:11434/v1".into()),
                ollama: Some(true),
                ..provider("openai-chat")
            },
        ),
        (
            "cpp",
            ProviderConfig {
                llamacpp: Some(true),
                ..provider("openai-chat")
            },
        ),
        (
            "studio",
            ProviderConfig {
                lmstudio: Some(true),
                ..provider("openai-chat")
            },
        ),
        (
            "cloud",
            ProviderConfig {
                base_url: Some("https://api.commandcode.ai/provider/v1".into()),
                ..provider("openai-chat")
            },
        ),
    ]);
    let none = IndexMap::new();
    assert_eq!(
        resolve_model("myollama/gemma4:e4b", &ps, &none).unwrap().server,
        Some(LocalServer::Ollama)
    );
    assert_eq!(
        resolve_model("cpp/qwen", &ps, &none).unwrap().server,
        Some(LocalServer::Llamacpp)
    );
    assert_eq!(
        resolve_model("studio/qwen", &ps, &none).unwrap().server,
        Some(LocalServer::Lmstudio)
    );
    let cloud = resolve_model("cloud/deepseek/deepseek-v4-flash", &ps, &none).unwrap();
    assert_eq!(cloud.server, None);
    assert_eq!(cloud.id, "deepseek/deepseek-v4-flash");
    assert!(!serde_json::to_string(&cloud).unwrap().contains("server"));
    let over = IndexMap::from([(
        "cloud/x".to_string(),
        ModelOverride {
            server: Some(LocalServer::Llamacpp),
            ..Default::default()
        },
    )]);
    assert_eq!(
        resolve_model("cloud/x", &ps, &over).unwrap().server,
        Some(LocalServer::Llamacpp)
    );
}

#[test]
fn a_model_override_supplies_the_price_resolve_model_reports() {
    let over: IndexMap<String, ModelOverride> =
        serde_json::from_str(r#"{"myproxy/some-model":{"price":{"input":1.25,"output":10}}}"#).unwrap();
    let m = resolve_model(
        "myproxy/some-model",
        &providers(&[("myproxy", provider("openai-chat"))]),
        &over,
    )
    .unwrap();
    assert_eq!(
        m.price,
        Some(Price {
            input: 1.25,
            output: 10.0,
            cache_read: None,
            cache_write: None
        })
    );
}

#[test]
fn unknown_models_get_defaults_and_bad_specs_name_the_problem() {
    let ps = providers(&[("a", provider("anthropic")), ("b", provider("openai-chat"))]);
    let m = resolve_model("a/some-new-model", &ps, &IndexMap::new()).unwrap();
    assert_eq!(
        (m.context_window, m.max_output, m.reasoning, m.images),
        (DEFAULT_CONTEXT_WINDOW, 32_000, Some(true), Some(true))
    );
    assert_eq!(m.price, None);
    let haiku = resolve_model("a/claude-haiku-4-5", &ps, &IndexMap::new()).unwrap();
    assert_eq!((haiku.context_window, haiku.reasoning), (200_000, Some(false)));
    assert_eq!(known_model("claude-opus-5-5").unwrap().price.cache_read, Some(0.2));
    assert_eq!(
        resolve_model("nomodel", &ps, &IndexMap::new()).unwrap_err().to_string(),
        r#"model must be "<provider>/<model>", got "nomodel""#
    );
    assert_eq!(
        resolve_model("c/m", &ps, &IndexMap::new()).unwrap_err().to_string(),
        r#"unknown provider "c" (known: a, b)"#
    );
}

#[test]
fn the_default_model_comes_from_the_first_provider_with_a_key() {
    let b = builtin_providers();
    let names: Vec<&str> = b.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "anthropic",
            "openai",
            "openai-chat",
            "openrouter",
            "ollama",
            "commandcode",
            "commandcode-responses",
            "commandcode-anthropic",
            "openai-codex"
        ]
    );
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    };
    assert_eq!(default_model_for_env(b, env(&[])), DEFAULT_MODEL);
    assert_eq!(
        default_model_for_env(b, env(&[("ANTHROPIC_API_KEY", "  ")])),
        DEFAULT_MODEL
    );
    assert_eq!(
        default_model_for_env(b, env(&[("CMD_API_KEY", "k")])),
        "commandcode/deepseek/deepseek-v4-flash"
    );
    assert_eq!(
        default_model_for_env(b, env(&[("CMD_API_KEY", "k"), ("OPENAI_API_KEY", "k")])),
        "openai/gpt-5.5"
    );
}
