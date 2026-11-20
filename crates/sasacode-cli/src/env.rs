//! ~/.sasacode/.env: the API keys sasacode reads for itself (`packages/cli/src/env.ts`).

use std::path::Path;

use indexmap::IndexMap;
use regex::Regex;

use sasacode_ai::ProviderConfig;
use sasacode_host::t;

pub struct KeyVariable {
    pub name: String,
    pub label: String,
    pub example: Option<String>,
}

/// Key variables of the built-in providers, each once, in provider order.
pub fn key_variables(providers: &IndexMap<String, ProviderConfig>) -> Vec<KeyVariable> {
    let mut seen: IndexMap<String, KeyVariable> = IndexMap::new();
    for (provider, p) in providers {
        let (Some(env), Some(label)) = (&p.api_key_env, &p.label) else {
            continue;
        };
        if env.is_empty() || label.is_empty() || seen.contains_key(env) {
            continue;
        }
        let example = p
            .default_model
            .as_ref()
            .filter(|m| !m.is_empty())
            .map(|m| format!("{provider}/{m}"));
        seen.insert(
            env.clone(),
            KeyVariable {
                name: env.clone(),
                label: label.clone(),
                example,
            },
        );
    }
    seen.into_values().collect()
}

/// Create the env file with an empty line per provider key, unless it exists. Owner-only
/// permissions.
pub fn ensure_env_file(path: &Path, providers: &IndexMap<String, ProviderConfig>) -> std::io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    let mut lines = vec![
        format!(
            "# {}",
            t!("sasacode API keys. Fill in the providers you use; blank entries are ignored.")
        ),
        format!(
            "# {}",
            t!("Read on every start, from any directory. A project's ./.env is not read.")
        ),
        format!(
            "# {}",
            t!("Commands the agent runs (bash, background jobs) do not see what is set here.")
        ),
        String::new(),
    ];
    for k in key_variables(providers) {
        let example = match &k.example {
            Some(e) => format!(" {}", t!("(e.g. -m {example})", example = e)),
            None => String::new(),
        };
        lines.push(format!("# {}{example}", k.label));
        lines.push(format!("{}=", k.name));
        lines.push(String::new());
    }
    lines.push(format!(
        "# {}",
        t!("Ollama (ollama/<model>) runs locally and needs no key.")
    ));
    lines.push(String::new());
    // The directory holds keys and logins: owner-only from the start. One that exists keeps its mode.
    if let Some(dir) = path.parent() {
        sasacode_plugin_api::create_private_dir(dir)?;
    }
    sasacode_plugin_api::write_private(path, lines.join("\n").as_bytes())?;
    Ok(true)
}

/// KEY=value lines; blank values are skipped so an empty entry means "not configured". Existing
/// variables win (`is_set`). Returns what to set, in file order; the names are sasacode's own
/// secrets (kept from child processes). The caller sets them (setting the environment is only
/// sound before other threads run: see main).
pub fn parse_env_file(path: &Path, is_set: impl Fn(&str) -> bool) -> Vec<(String, String)> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    // INFALLIBLE: constant patterns.
    let line_re = Regex::new(r"^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$").unwrap();
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.split('\n') {
        let Some(m) = line_re.captures(line) else { continue };
        let name = m[1].to_string();
        let raw = &m[2];
        // JS `.` stops at \r, \u2028 and \u2029, so a value holding one did not match there.
        if raw.contains(['\r', '\u{2028}', '\u{2029}']) {
            continue;
        }
        let value = unquote(raw);
        if !value.is_empty() && !is_set(&name) && !out.iter().any(|(n, _)| *n == name) {
            out.push((name, value));
        }
    }
    out
}

/// `value.replace(/^(["'])(.*)\1$/, "$2")`.
fn unquote(v: &str) -> String {
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

/// Key variables that are set but blank (exported empty by a shell profile): they must not shadow
/// a real key, so the caller removes them.
pub fn blank_keys(providers: &IndexMap<String, ProviderConfig>, get: impl Fn(&str) -> Option<String>) -> Vec<String> {
    key_variables(providers)
        .into_iter()
        .filter(|k| get(&k.name).is_some_and(|v| v.trim().is_empty()))
        .map(|k| k.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_lines_parse_as_the_ts_read_them() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".env");
        std::fs::write(
            &p,
            "# c\nA_KEY=one\n export B_KEY = \"two\" \nC_KEY=\nD_KEY='three'\nSET_KEY=x\nbad line\nE_KEY=\"q'\nA_KEY=again\nF_KEY=x\r\nG_KEY=a\rb\n",
        )
        .unwrap();
        let got = parse_env_file(&p, |n| n == "SET_KEY");
        let want: Vec<(String, String)> = [
            ("A_KEY", "one"),
            ("B_KEY", "two"),
            ("D_KEY", "three"),
            ("E_KEY", "\"q'"),
            ("F_KEY", "x"),
        ]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        assert_eq!(got, want);
        assert!(parse_env_file(&dir.path().join("missing"), |_| false).is_empty());
    }
}

#[cfg(test)]
mod ts_tests {
    //! packages/cli/test/env.test.ts. The environment is read through closures here, not changed.
    use super::*;

    #[test]
    fn the_generated_env_lists_every_provider_key_blank_readable_only_by_the_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home").join(".env");
        let providers = sasacode_host::builtin_providers();
        assert!(ensure_env_file(&path, providers).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        for name in [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "OPENROUTER_API_KEY",
            "CMD_API_KEY",
        ] {
            assert!(text.contains(&format!("\n{name}=\n")), "{name}");
        }
        assert_eq!(text.matches("OPENAI_API_KEY=").count(), 1); // openai and openai-chat share one key
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            // The directory made for it is the owner's alone; one that was there keeps its mode.
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(path.parent().unwrap()), 0o700);
            let shared = dir.path().join("shared");
            std::fs::create_dir(&shared).unwrap();
            std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(ensure_env_file(&shared.join(".env"), providers).unwrap());
            assert_eq!(mode(&shared), 0o755);
        }
        std::fs::write(&path, "CMD_API_KEY=mine\n").unwrap();
        assert!(!ensure_env_file(&path, providers).unwrap()); // never overwritten
        let names: Vec<String> = key_variables(providers).into_iter().map(|k| k.name).collect();
        assert_eq!(
            names,
            [
                "ANTHROPIC_API_KEY",
                "OPENAI_API_KEY",
                "OPENROUTER_API_KEY",
                "CMD_API_KEY"
            ]
        );
    }

    #[test]
    fn blank_entries_mean_not_configured() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blank.env");
        std::fs::write(
            &path,
            "ANTHROPIC_API_KEY=\nOPENAI_API_KEY=\"\"\nCMD_API_KEY=abc\n# comment\n",
        )
        .unwrap();
        let providers = sasacode_host::builtin_providers();
        // e.g. from a project .env Bun loaded
        let env = |n: &str| (n == "OPENROUTER_API_KEY").then(|| "   ".to_string());
        assert_eq!(blank_keys(providers, env), ["OPENROUTER_API_KEY"]);
        let set = parse_env_file(&path, |_| false);
        assert_eq!(set, [("CMD_API_KEY".to_string(), "abc".to_string())]);
        let vars = |n: &str| (n == "CMD_API_KEY").then(|| "abc".to_string());
        assert_eq!(
            sasacode_host::default_model_for_env(providers, vars),
            "commandcode/deepseek/deepseek-v4-flash"
        );
    }
}
