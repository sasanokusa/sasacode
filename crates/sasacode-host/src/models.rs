//! The providers and models the app knows (`packages/host/src/models.ts`).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use sasacode_ai::{LocalServer, ModelInfo, Price, ProviderConfig};

/// Order matters: with no model configured, the first provider whose key is set supplies the default.
pub fn builtin_providers() -> &'static IndexMap<String, ProviderConfig> {
    static P: LazyLock<IndexMap<String, ProviderConfig>> = LazyLock::new(|| {
        let p = |api: &str| ProviderConfig {
            api: api.to_string(),
            ..Default::default()
        };
        let s = |v: &str| Some(v.to_string());
        IndexMap::from([
            (
                "anthropic".to_string(),
                ProviderConfig {
                    api_key_env: s("ANTHROPIC_API_KEY"),
                    label: s("Anthropic"),
                    default_model: s("claude-opus-5"),
                    ..p("anthropic")
                },
            ),
            (
                "openai".to_string(),
                ProviderConfig {
                    api_key_env: s("OPENAI_API_KEY"),
                    label: s("OpenAI"),
                    default_model: s("gpt-5.5"),
                    ..p("openai-responses")
                },
            ),
            (
                "openai-chat".to_string(),
                ProviderConfig {
                    api_key_env: s("OPENAI_API_KEY"),
                    ..p("openai-chat")
                },
            ),
            (
                "openrouter".to_string(),
                ProviderConfig {
                    base_url: s("https://openrouter.ai/api/v1"),
                    api_key_env: s("OPENROUTER_API_KEY"),
                    label: s("OpenRouter"),
                    default_model: s("openrouter/auto"),
                    ..p("openai-chat")
                },
            ),
            (
                "ollama".to_string(),
                ProviderConfig {
                    base_url: s("http://localhost:11434/v1"),
                    ollama: Some(true),
                    ..p("openai-chat")
                },
            ),
            // Command Code serves all three formats on one key; Claude models answer on /messages only.
            (
                "commandcode".to_string(),
                ProviderConfig {
                    base_url: s("https://api.commandcode.ai/provider/v1"),
                    api_key_env: s("CMD_API_KEY"),
                    label: s("Command Code"),
                    default_model: s("deepseek/deepseek-v4-flash"),
                    ..p("openai-chat")
                },
            ),
            (
                "commandcode-responses".to_string(),
                ProviderConfig {
                    base_url: s("https://api.commandcode.ai/provider/v1"),
                    api_key_env: s("CMD_API_KEY"),
                    ..p("openai-responses")
                },
            ),
            (
                "commandcode-anthropic".to_string(),
                ProviderConfig {
                    base_url: s("https://api.commandcode.ai/provider"),
                    api_key_env: s("CMD_API_KEY"),
                    ..p("anthropic")
                },
            ),
            // A ChatGPT plan instead of an API key (`sasacode login`). The bundled openai-codex plugin
            // implements the api; without it (plugins.disabled) these models are simply unavailable.
            (
                "openai-codex".to_string(),
                ProviderConfig {
                    base_url: s("https://chatgpt.com/backend-api/codex"),
                    default_model: s("gpt-5.5"),
                    ..p("openai-codex")
                },
            ),
        ])
    });
    &P
}

/// What the table of known models says about one (`Omit<ModelInfo, "id" | "provider" | "api" | "baseUrl">`).
#[derive(Debug, Clone, PartialEq)]
pub struct KnownModel {
    pub context_window: u64,
    pub max_output: u64,
    pub price: Price,
    pub reasoning: bool,
    pub images: bool,
}

const fn price(input: f64, output: f64) -> Price {
    Price {
        input,
        output,
        cache_read: None,
        cache_write: None,
    }
}

const fn claude(input: f64, output: f64, context_window: u64) -> KnownModel {
    KnownModel {
        context_window,
        max_output: 64_000,
        price: price(input, output),
        reasoning: true,
        images: true,
    }
}

// OpenAI prices are list-price estimates in USD per 1M tokens, in distinct tiers (the newest
// generation at a premium, then flagship / smaller / mini). They only feed the cost line: a
// provider that reports actual spend (OpenRouter) wins, and modelOverrides.price can correct any model.
const fn gpt(input: f64, output: f64, context_window: u64) -> KnownModel {
    KnownModel {
        context_window,
        max_output: 128_000,
        price: price(input, output),
        reasoning: true,
        images: true,
    }
}

/// Known model metadata, in the TS table's order. Anything else gets conservative defaults and can
/// be overridden in config.
pub static KNOWN_MODELS: &[(&str, KnownModel)] = &[
    ("claude-fable-5-1", claude(10.0, 50.0, 1_000_000)),
    (
        "claude-opus-5-5",
        KnownModel {
            price: Price {
                input: 4.0,
                output: 20.0,
                cache_read: Some(0.2),
                cache_write: None,
            },
            ..claude(4.0, 20.0, 1_000_000)
        },
    ),
    ("claude-opus-5", claude(5.0, 25.0, 1_000_000)),
    ("claude-opus-4-8", claude(5.0, 25.0, 1_000_000)),
    ("claude-sonnet-5", claude(2.0, 10.0, 1_000_000)),
    ("claude-sonnet-4-6", claude(3.0, 15.0, 1_000_000)),
    (
        "claude-haiku-4-5",
        KnownModel {
            reasoning: false,
            ..claude(1.0, 5.0, 200_000)
        },
    ),
    // OpenAI's Models API does not report context windows; these come from what Command Code's
    // Models API reports for the same models.
    ("gpt-6-astra", gpt(2.5, 20.0, 1_050_000)),
    ("gpt-6-sol", gpt(2.5, 20.0, 1_050_000)),
    ("gpt-6-luna", gpt(2.5, 20.0, 1_050_000)),
    ("gpt-5.6-sol", gpt(1.25, 10.0, 1_050_000)),
    ("gpt-5.6-terra", gpt(1.25, 10.0, 1_050_000)),
    ("gpt-5.6-luna", gpt(1.25, 10.0, 1_050_000)),
    ("gpt-5.5", gpt(1.25, 10.0, 400_000)),
    ("gpt-5.4", gpt(1.25, 10.0, 400_000)),
    ("gpt-5.4-mini", gpt(0.25, 2.0, 400_000)),
    ("gpt-5.3-codex", gpt(0.5, 4.0, 400_000)),
];

/// `KNOWN_MODELS[id]`.
pub fn known_model(id: &str) -> Option<&'static KnownModel> {
    KNOWN_MODELS.iter().find(|(k, _)| *k == id).map(|(_, m)| m)
}

pub const DEFAULT_MODEL: &str = "anthropic/claude-opus-5";

/// First provider with a non-empty key in the environment, as "<provider>/<model>".
pub fn default_model_for(providers: &IndexMap<String, ProviderConfig>) -> String {
    default_model_for_env(providers, |name| {
        std::env::var_os(name).map(|v| v.to_string_lossy().into_owned())
    })
}

/// [`default_model_for`] with the environment read through `env`.
pub fn default_model_for_env(
    providers: &IndexMap<String, ProviderConfig>,
    env: impl Fn(&str) -> Option<String>,
) -> String {
    for (name, p) in providers {
        let (Some(model), Some(key)) = (&p.default_model, &p.api_key_env) else {
            continue;
        };
        if env(key).is_some_and(|v| !v.trim().is_empty()) {
            return format!("{name}/{model}");
        }
    }
    DEFAULT_MODEL.to_string()
}

/// What a model neither the provider nor the table of known models has a size for plans with.
pub const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;

/// `Partial<ModelInfo>`: what config's `modelOverrides` (and the models a provider's Models API
/// reported) set for one model. Each field set replaces the resolved one whole, as the TS spread did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<LocalServer>,
}

impl ModelOverride {
    /// `{ ...self, ...other }`: fields set in `other` win.
    pub fn merged(&self, other: &ModelOverride) -> ModelOverride {
        let o = other.clone();
        let s = self.clone();
        ModelOverride {
            id: o.id.or(s.id),
            provider: o.provider.or(s.provider),
            api: o.api.or(s.api),
            base_url: o.base_url.or(s.base_url),
            context_window: o.context_window.or(s.context_window),
            max_output: o.max_output.or(s.max_output),
            price: o.price.or(s.price),
            reasoning: o.reasoning.or(s.reasoning),
            images: o.images.or(s.images),
            headers: o.headers.or(s.headers),
            server: o.server.or(s.server),
        }
    }

    /// `{ ...model, ...self }`.
    pub fn apply(&self, m: &mut ModelInfo) {
        let o = self.clone();
        if let Some(v) = o.id {
            m.id = v;
        }
        if let Some(v) = o.provider {
            m.provider = v;
        }
        if let Some(v) = o.api {
            m.api = v;
        }
        if o.base_url.is_some() {
            m.base_url = o.base_url;
        }
        if let Some(v) = o.context_window {
            m.context_window = v;
        }
        if let Some(v) = o.max_output {
            m.max_output = v;
        }
        if o.price.is_some() {
            m.price = o.price;
        }
        if o.reasoning.is_some() {
            m.reasoning = o.reasoning;
        }
        if o.images.is_some() {
            m.images = o.images;
        }
        if o.headers.is_some() {
            m.headers = o.headers;
        }
        if o.server.is_some() {
            m.server = o.server;
        }
    }
}

/// The model `spec` ("<provider>/<model>") names, with what the table of known models and
/// `overrides` (keyed by spec) say about it.
pub fn resolve_model(
    spec: &str,
    providers: &IndexMap<String, ProviderConfig>,
    overrides: &IndexMap<String, ModelOverride>,
) -> anyhow::Result<ModelInfo> {
    let Some((provider, id)) = spec.split_once('/') else {
        anyhow::bail!("model must be \"<provider>/<model>\", got \"{spec}\"");
    };
    let Some(p) = providers.get(provider) else {
        let known: Vec<&str> = providers.keys().map(String::as_str).collect();
        anyhow::bail!("unknown provider \"{provider}\" (known: {})", known.join(", "));
    };
    let known = known_model(id);
    // Which local server answers, so the adapter can send the fields that server reads (max_tokens and so on).
    let server = if p.ollama == Some(true) {
        Some(LocalServer::Ollama)
    } else if p.llamacpp == Some(true) {
        Some(LocalServer::Llamacpp)
    } else if p.lmstudio == Some(true) {
        Some(LocalServer::Lmstudio)
    } else {
        None
    };
    let mut m = ModelInfo {
        id: id.to_string(),
        provider: provider.to_string(),
        api: p.api.clone(),
        base_url: p.base_url.clone(),
        context_window: known.map_or(DEFAULT_CONTEXT_WINDOW, |k| k.context_window),
        max_output: known.map_or(32_000, |k| k.max_output),
        price: known.map(|k| k.price.clone()),
        // Unknown models are assumed to reason: the effort is sent, and dropped if the server refuses it.
        reasoning: Some(known.is_none_or(|k| k.reasoning)),
        images: Some(known.is_none_or(|k| k.images)),
        headers: p.headers.clone(),
        server,
    };
    if let Some(o) = overrides.get(spec) {
        o.apply(&mut m);
    }
    Ok(m)
}
