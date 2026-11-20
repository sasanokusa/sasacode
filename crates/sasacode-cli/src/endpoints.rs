//! Custom endpoints (a llama.cpp server, vLLM, LM Studio, a proxy …): configured with just a URL,
//! their API format is detected from the Models API and remembered (`packages/cli/src/endpoints.ts`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use indexmap::IndexMap;
use serde_json::Value;

use sasacode_ai::ProviderConfig;
use sasacode_host::{builtin_providers, t};

use crate::args::{boolean, parse_args, string};
use crate::catalog::GetKey;
use crate::config::{load_config, read_config_files_from, sasacode_home};
use crate::detect::{DetectedEndpoint, apply_detected, detect_endpoint, detected_entry};
use crate::json::{read_json, read_json_or, write_json};
use crate::list_models::{Deadline, list_models};

fn cache_file(home: &Path) -> PathBuf {
    home.join("endpoints.json")
}

/// Give every provider configured without `api` (an empty `api`) its detected format, using the
/// cached result when there is one (so startup does not wait on the network). Unreachable ones are
/// dropped with a warning.
pub async fn resolve_endpoints(
    providers: &mut IndexMap<String, ProviderConfig>,
    get_key: GetKey,
    warnings: &mut Vec<String>,
) {
    resolve_endpoints_in(&sasacode_home(), providers, get_key, warnings).await
}

/// [`resolve_endpoints`] with the cache in `home`.
pub async fn resolve_endpoints_in(
    home: &Path,
    providers: &mut IndexMap<String, ProviderConfig>,
    get_key: GetKey,
    warnings: &mut Vec<String>,
) {
    // Only a cache: anything unreadable is detected again.
    let mut cache: IndexMap<String, DetectedEndpoint> = read_json_or(&cache_file(home), IndexMap::new());
    let mut changed = false;
    let mut pending: Vec<(String, String)> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for (name, p) in providers.iter() {
        if !p.api.is_empty() {
            continue;
        }
        match p.base_url.as_deref().filter(|b| !b.is_empty()) {
            None => {
                warnings.push(t!(
                    "provider {name} has neither \"api\" nor \"baseUrl\"; ignored",
                    name = name
                ));
                dropped.push(name.clone());
            }
            Some(url) => pending.push((name.clone(), url.to_string())),
        }
    }
    let detections = futures::future::join_all(pending.iter().map(|(name, url)| {
        let cached = cache.get(url).cloned();
        let get_key = get_key.clone();
        async move {
            if let Some(found) = cached {
                return (Ok(found), false);
            }
            let key = get_key(name.clone()).await;
            let deadline = Deadline::after(Duration::from_millis(4000));
            (detect_endpoint(url, key.as_deref(), &deadline.token).await, true)
        }
    }))
    .await;
    let mut resolved: Vec<(String, DetectedEndpoint)> = Vec::new();
    for ((name, url), (result, fresh)) in pending.into_iter().zip(detections) {
        match result {
            Ok(found) => {
                if fresh {
                    cache.insert(url, found.clone());
                    changed = true;
                }
                resolved.push((name, found));
            }
            Err(e) => {
                warnings.push(t!(
                    "provider {name} ({url}): {error}; ignored",
                    name = name,
                    url = url,
                    error = e
                ));
                dropped.push(name);
            }
        }
    }
    for name in dropped {
        providers.shift_remove(&name);
    }
    for (name, found) in resolved {
        if let Some(p) = providers.get_mut(&name) {
            *p = apply_detected(p, &found);
        }
    }
    if changed {
        let _ = write_json(&cache_file(home), &cache);
    }
}

fn config_path(cwd: &Path, home: &Path, project: bool) -> PathBuf {
    if project {
        cwd.join(".sasacode").join("config.json")
    } else {
        home.join("config.json")
    }
}

/// `String(v)` for a config value printed in `endpoint list`.
fn display(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => sasacode_ai::json::stringify(other),
    }
}

/// `sasacode endpoint add|list|remove`. Errors the TS threw are `Err` (main prints `sasacode: …`).
pub async fn endpoint_command(args: &[String], cwd: &Path) -> anyhow::Result<i32> {
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    endpoint_command_in(args, cwd, &sasacode_home(), &mut out, &mut err).await
}

/// [`endpoint_command`] with the home directory and the output given.
pub async fn endpoint_command_in(
    args: &[String],
    cwd: &Path,
    home: &Path,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<i32> {
    let parsed = parse_args(args, &[boolean("project", None), string("key-env", None)]).map_err(anyhow::Error::msg)?;
    let project = parsed.flag("project");
    let key_env = parsed.string("key-env").filter(|k| !k.is_empty());
    // JS truthiness: an empty argument counts as absent.
    let mut positionals = parsed
        .positionals
        .iter()
        .map(|s| Some(s.as_str()).filter(|s| !s.is_empty()));
    let mut next = || positionals.next().flatten();
    let (sub, name, url) = (next(), next(), next());
    let path = config_path(cwd, home, project);

    if let (Some("add"), Some(name), Some(url)) = (sub, name, url) {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        {
            anyhow::bail!(t!("name may contain letters, digits, _ . - only"));
        }
        if builtin_providers().contains_key(name) {
            anyhow::bail!(t!(
                "\"{name}\" is a built-in provider; choose another name",
                name = name
            ));
        }
        let mut config = read_json(&path)?; // before detecting: a config that cannot be read stops here
        let key = key_env.and_then(|k| std::env::var(k).ok());
        let keyed = key.as_deref().filter(|k| !k.is_empty());
        if let Some(k) = key_env
            && keyed.is_none()
        {
            let _ = writeln!(
                err,
                "{}",
                t!("warning: {keyEnv} is not set; detecting without a key", keyEnv = k)
            );
        }
        let found = {
            let deadline = Deadline::after(Duration::from_millis(10_000));
            detect_endpoint(url, keyed, &deadline.token).await?
        };
        let entry = detected_entry(&found, key_env);
        let flavour = if found.llamacpp == Some(true) {
            " (llama.cpp)"
        } else if found.ollama == Some(true) {
            " (Ollama)"
        } else if found.lmstudio == Some(true) {
            " (LM Studio)"
        } else {
            ""
        };
        let api = if found.api == "anthropic" {
            "Anthropic Messages"
        } else {
            "OpenAI Chat Completions"
        };
        let _ = writeln!(
            out,
            "{}",
            t!(
                "{name}: {api}{flavour} at {url}",
                name = name,
                api = api,
                flavour = flavour,
                url = found.base_url
            )
        );
        let models = match serde_json::from_value::<ProviderConfig>(Value::Object(entry.clone())) {
            Ok(provider) => {
                let deadline = Deadline::after(Duration::from_millis(10_000));
                list_models(name, &provider, keyed, &deadline.token, None)
                    .await
                    .unwrap_or_default()
            }
            Err(_) => Vec::new(),
        };
        for m in models.iter().take(20) {
            let size = match (m.context_window.filter(|c| *c != 0), m.max_context.filter(|c| *c != 0)) {
                (Some(ctx), _) => format!("  {}", t!("({ctx} ctx)", ctx = ctx)),
                (None, Some(max)) => format!("  {}", t!("(max {max})", max = max)),
                (None, None) => String::new(),
            };
            let _ = writeln!(out, "  {name}/{}{size}", m.id);
        }
        if models.len() > 20 {
            let _ = writeln!(out, "  {}", t!("… {n} more (see /model)", n = models.len() - 20));
        }
        let Some(obj) = config.as_object_mut() else {
            anyhow::bail!(t!(
                "failed to read {path}: {error}",
                path = path.display(),
                error = "not a JSON object"
            ));
        };
        let mut providers = match obj.get("providers") {
            Some(Value::Object(p)) => p.clone(),
            _ => serde_json::Map::new(),
        };
        providers.insert(name.to_string(), Value::Object(entry));
        obj.insert("providers".to_string(), Value::Object(providers));
        write_json(&path, &config)?;
        let tip = match models.first() {
            Some(m) => format!(
                " — {}",
                t!("try: sasacode -m {spec}", spec = format!("{name}/{}", m.id))
            ),
            None => String::new(),
        };
        let _ = writeln!(out, "{}{tip}", t!("saved to {path}", path = path.display()));
        if project {
            let _ = writeln!(
                out,
                "{}",
                t!(
                    "note: endpoints in a project config are used once you trust the project (sasacode asks on the next start)"
                )
            );
        }
        return Ok(0);
    }
    if let (Some("remove"), Some(name)) = (sub, name) {
        let mut config = read_json(&path)?;
        let present = config
            .get("providers")
            .and_then(|p| p.get(name))
            .is_some_and(sasacode_ai::transport::js_truthy);
        if !present {
            let _ = writeln!(
                err,
                "{}",
                t!("{name} is not in {path}", name = name, path = path.display())
            );
            return Ok(1);
        }
        if let Some(providers) = config.get_mut("providers").and_then(Value::as_object_mut) {
            providers.shift_remove(name);
        }
        write_json(&path, &config)?;
        let _ = writeln!(
            out,
            "{}",
            t!("removed {name} from {path}", name = name, path = path.display())
        );
        return Ok(0);
    }
    if sub.is_none() || sub == Some("list") {
        let files = read_config_files_from(&home.join("config.json"), cwd)?;
        let config = load_config(cwd, false, Some(files))?.config;
        let custom: Vec<(&String, &Value)> = config
            .get("providers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(n, _)| !builtin_providers().contains_key(n.as_str()))
            .collect();
        if custom.is_empty() {
            let _ = writeln!(
                out,
                "{}",
                t!("no custom endpoints (add one: sasacode endpoint add <name> <url>)")
            );
        }
        for (n, p) in custom {
            let field = |k: &str| p.get(k).filter(|v| !v.is_null());
            let base_url = field("baseUrl").map_or_else(|| "-".to_string(), display);
            let api = field("api").map_or_else(|| "auto".to_string(), display);
            let key = match p.get("apiKeyEnv").filter(|v| sasacode_ai::transport::js_truthy(v)) {
                Some(env) => format!("  {}", t!("key: {env}", env = display(env))),
                None => String::new(),
            };
            let _ = writeln!(out, "{n}  {base_url}  {api}{key}");
        }
        return Ok(0);
    }
    let _ = writeln!(
        err,
        "{}",
        t!("usage: sasacode endpoint <add <name> <url> [--key-env VAR]|remove <name>|list> [--project]")
    );
    Ok(1)
}

#[cfg(test)]
#[path = "endpoints_tests.rs"]
mod tests;
