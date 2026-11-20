//! The values sasacode keeps out of every record (`[REDACTED]`), collected once at start: the keys
//! of all configured providers and the headers and environment of the MCP servers. Tokens read
//! later (a login, a refresh, a key from the keychain) are added where they are read.

use indexmap::IndexMap;
use serde_json::Value;

use sasacode_ai::ProviderConfig;
use sasacode_plugin_api::hide_secrets;

/// What a built-in client reads when the provider names no variable of its own.
const DEFAULT_KEY_VARIABLES: [&str; 3] = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "OPENAI_API_KEY"];

/// The key variables' values of every provider (the keychain is asked as keys are needed, not for
/// every provider here), the stored OpenAI Codex login, and the MCP servers' header and environment
/// values of a trusted `config`.
pub fn hide_configured(providers: &IndexMap<String, ProviderConfig>, config: &serde_json::Map<String, Value>) {
    hide_values(providers, config, |n| std::env::var(n).ok());
    // Reading the login registers its tokens.
    let _ = sasacode_bundled::openai_codex::read_codex_auth();
}

fn hide_values(
    providers: &IndexMap<String, ProviderConfig>,
    config: &serde_json::Map<String, Value>,
    var: impl Fn(&str) -> Option<String>,
) {
    let named = providers.values().filter_map(|p| p.api_key_env.as_deref());
    hide_secrets(named.chain(DEFAULT_KEY_VARIABLES).filter_map(&var));
    hide_mcp_values(config.get("mcpServers").unwrap_or(&Value::Null), var);
}

/// Header and `env` values of the `mcpServers` entries as the servers get them (`${VAR}` expanded,
/// an unset one empty), and the value of every variable they refer to.
fn hide_mcp_values(servers: &Value, var: impl Fn(&str) -> Option<String>) {
    let Some(servers) = servers.as_object() else { return };
    for entry in servers.values() {
        for field in ["headers", "env"] {
            let values = entry.get(field).and_then(Value::as_object).into_iter().flatten();
            for text in values.filter_map(|(_, v)| v.as_str()) {
                let (expanded, referred) = expand(text, &var);
                hide_secrets(referred.into_iter().chain([expanded]));
            }
        }
    }
}

/// `text` with each `${NAME}` replaced by its value, and those values on their own.
fn expand(text: &str, var: &impl Fn(&str) -> Option<String>) -> (String, Vec<String>) {
    let (mut out, mut referred, mut rest) = (String::new(), Vec::new(), text);
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let name_len = after
            .char_indices()
            .take_while(|(i, c)| c.is_ascii_alphabetic() || *c == '_' || (*i > 0 && c.is_ascii_digit()))
            .count();
        if name_len > 0 && after[name_len..].starts_with('}') {
            let value = var(&after[..name_len]).unwrap_or_default();
            out.push_str(&value);
            referred.push(value);
            rest = &after[name_len + 1..];
        } else {
            out.push_str("${");
            rest = after;
        }
    }
    out.push_str(rest);
    (out, referred)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sasacode_plugin_api::redact_secrets;
    use serde_json::json;

    #[test]
    fn mcp_headers_and_env_are_hidden_as_the_servers_get_them_and_short_values_stay() {
        let servers = json!({
            "docs": { "url": "https://x.example", "headers": { "Authorization": "Bearer ${REDACT_T_TOKEN}", "X-Short": "tiny" } },
            "tool": { "command": "t", "env": { "SECRET_LITERAL": "literal-value-redact-t", "UNSET": "${REDACT_T_NONE}", "A": "abc" } },
        });
        let var = |n: &str| (n == "REDACT_T_TOKEN").then(|| "tok-redact-t-0123456789".to_string());
        hide_mcp_values(&servers, var);
        let shown = redact_secrets(
            "Bearer tok-redact-t-0123456789 | tok-redact-t-0123456789 | literal-value-redact-t | tiny | abc",
        );
        assert_eq!(shown, "[REDACTED] | [REDACTED] | [REDACTED] | tiny | abc");
    }

    #[test]
    fn provider_keys_of_every_provider_are_hidden() {
        let cfg: ProviderConfig =
            serde_json::from_value(json!({ "api": "openai-chat", "apiKeyEnv": "REDACT_T_PROVIDER_KEY" })).unwrap();
        let var = |n: &str| match n {
            "REDACT_T_PROVIDER_KEY" => Some("provider-key-redact-t-123456".to_string()),
            "ANTHROPIC_AUTH_TOKEN" => Some("anthropic-token-redact-t-123456".to_string()),
            _ => None,
        };
        hide_values(&IndexMap::from([("other".to_string(), cfg)]), &Default::default(), var);
        assert_eq!(
            redact_secrets("k=provider-key-redact-t-123456 t=anthropic-token-redact-t-123456"),
            "k=[REDACTED] t=[REDACTED]"
        );
    }
}
