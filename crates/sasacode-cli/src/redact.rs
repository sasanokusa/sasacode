//! The values sasacode keeps out of every record (`[REDACTED]`), collected once at start: the keys
//! of all configured providers and the headers and environment of the MCP servers. Tokens read
//! later (a login, a refresh, a key from the keychain) are added where they are read.

use indexmap::IndexMap;
use serde_json::Value;

use sasacode_ai::ProviderConfig;
use sasacode_plugin_api::hide_secrets;

/// An environment variable whose name says it holds a secret.
fn secret_name(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    [
        "KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASS",
        "AUTH",
        "CREDENTIAL",
        "PRIVATE",
    ]
    .iter()
    .any(|w| up.contains(w))
        || up.split(['_', '-']).any(|w| w == "PAT")
}

/// What a built-in client reads when the provider names no variable of its own.
const DEFAULT_KEY_VARIABLES: [&str; 3] = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "OPENAI_API_KEY"];

/// The key variables' values of every provider (the keychain is asked as keys are needed, not for
/// every provider here), the stored OpenAI Codex login, and the MCP servers' header and environment
/// values of a trusted `config`.
pub fn hide_configured(providers: &IndexMap<String, ProviderConfig>, config: &serde_json::Map<String, Value>) {
    // What records hold as paths (the working directory, the home) is never hidden.
    let paths: Vec<String> = [std::env::current_dir().ok(), Some(sasacode_plugin_api::home_dir())]
        .into_iter()
        .flatten()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    hide_values(providers, config, &paths, |n| std::env::var(n).ok());
    // Reading the login registers its tokens.
    let _ = sasacode_bundled::openai_codex::read_codex_auth();
}

fn hide_values(
    providers: &IndexMap<String, ProviderConfig>,
    config: &serde_json::Map<String, Value>,
    paths: &[String],
    var: impl Fn(&str) -> Option<String>,
) {
    let named = providers.values().filter_map(|p| p.api_key_env.as_deref());
    hide_secrets(named.chain(DEFAULT_KEY_VARIABLES).filter_map(&var));
    hide_mcp_values(config.get("mcpServers").unwrap_or(&Value::Null), paths, var);
}

/// What the `mcpServers` entries give their servers: header values (`${VAR}` expanded, an unset one
/// empty) and the variables they refer to; `env` values, only where the variable's name says it is
/// a secret (a `${VAR}` in it is judged by VAR's name). A value that is, or starts, or lies under a
/// path in `paths` stays: hiding it would blank the paths a resumed session reads back.
fn hide_mcp_values(servers: &Value, paths: &[String], var: impl Fn(&str) -> Option<String>) {
    let Some(servers) = servers.as_object() else { return };
    let path_like = |v: &str| paths.iter().any(|p| p.starts_with(v) || v.starts_with(p.as_str()));
    for entry in servers.values() {
        for field in ["headers", "env"] {
            let values = entry.get(field).and_then(Value::as_object).into_iter().flatten();
            for (name, text) in values.filter_map(|(k, v)| Some((k, v.as_str()?))) {
                let (expanded, referred) = expand(text, &var);
                let whole = field == "headers" || secret_name(name);
                let kept = referred
                    .into_iter()
                    .filter(|(n, _)| field == "headers" || secret_name(n));
                let values = kept.map(|(_, v)| v).chain(whole.then_some(expanded));
                hide_secrets(values.filter(|v| !path_like(v)));
            }
        }
    }
}

/// `text` with each `${NAME}` replaced by its value, and those (name, value) pairs.
fn expand(text: &str, var: &impl Fn(&str) -> Option<String>) -> (String, Vec<(String, String)>) {
    let (mut out, mut referred, mut rest) = (String::new(), Vec::new(), text);
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let name_len = after
            .char_indices()
            .take_while(|(i, c)| c.is_ascii_alphabetic() || *c == '_' || (*i > 0 && c.is_ascii_digit()))
            .count();
        if name_len > 0 && after[name_len..].starts_with('}') {
            let name = &after[..name_len];
            let value = var(name).unwrap_or_default();
            out.push_str(&value);
            referred.push((name.to_string(), value));
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

    fn names_of(servers: Value, paths: &[&str], vars: &[(&str, &str)]) {
        let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        let var = |n: &str| vars.iter().find(|(k, _)| *k == n).map(|(_, v)| v.to_string());
        hide_mcp_values(&servers, &paths, var);
    }

    #[test]
    fn mcp_headers_and_secret_named_env_are_hidden_as_the_servers_get_them_and_short_values_stay() {
        names_of(
            json!({
                "docs": { "url": "https://x.example", "headers": { "Authorization": "Bearer ${REDACT_T_TOKEN}", "X-Short": "tiny" } },
                "tool": { "command": "t", "env": { "API_TOKEN": "literal-value-redact-t", "UNSET": "${REDACT_T_NONE}", "A": "abc" } },
            }),
            &[],
            &[("REDACT_T_TOKEN", "tok-redact-t-0123456789")],
        );
        let shown = redact_secrets(
            "Bearer tok-redact-t-0123456789 | tok-redact-t-0123456789 | literal-value-redact-t | tiny | abc",
        );
        assert_eq!(shown, "[REDACTED] | [REDACTED] | [REDACTED] | tiny | abc");
    }

    #[test]
    fn mcp_env_values_are_hidden_by_the_name_of_the_variable_and_paths_stay() {
        names_of(
            json!({ "t": { "env": {
                "PROJECT_DIR": "/redact-t/project-dir/sub",
                "GITHUB_TOKEN": "ghp_redact_t_0123456789",
                "MY_PAT": "pat-redact-t-0123456789",
                "NOTE": "plain-note-redact-t",
                "VIA": "${REDACT_T_PASSWORD}",
                "VIA_PLAIN": "${REDACT_T_DIR}",
                "WORKDIR_TOKEN": "/redact-t/cwd/inner",
            } } }),
            &["/redact-t/cwd"],
            &[
                ("REDACT_T_PASSWORD", "pw-redact-t-0123456789"),
                ("REDACT_T_DIR", "dir-value-redact-t"),
            ],
        );
        let shown = redact_secrets(
            "/redact-t/project-dir/sub ghp_redact_t_0123456789 pat-redact-t-0123456789 plain-note-redact-t pw-redact-t-0123456789 dir-value-redact-t /redact-t/cwd/inner",
        );
        assert_eq!(
            shown,
            "/redact-t/project-dir/sub [REDACTED] [REDACTED] plain-note-redact-t [REDACTED] dir-value-redact-t /redact-t/cwd/inner"
        );
    }

    #[test]
    fn a_header_value_that_is_a_path_of_the_session_is_not_hidden() {
        names_of(
            json!({ "t": { "headers": { "X-Dir": "${REDACT_T_HOME}", "X-Sub": "${REDACT_T_HOME}/sub", "X-Tok": "Bearer ${REDACT_T_HOME}" } } }),
            &["/redact-t/home/user"],
            &[("REDACT_T_HOME", "/redact-t/home/user")],
        );
        assert_eq!(
            redact_secrets("/redact-t/home/user /redact-t/home/user/sub Bearer /redact-t/home/user"),
            "/redact-t/home/user /redact-t/home/user/sub [REDACTED]"
        );
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
        hide_values(
            &IndexMap::from([("other".to_string(), cfg)]),
            &Default::default(),
            &[],
            var,
        );
        assert_eq!(
            redact_secrets("k=provider-key-redact-t-123456 t=anthropic-token-redact-t-123456"),
            "k=[REDACTED] t=[REDACTED]"
        );
    }
}
