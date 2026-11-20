//! API keys: the provider's environment variable, else the OS keychain (`packages/cli/src/keys.ts`).
//!
//! The keychain is asked exactly as the TS asked it (same command, same arguments, no input), once
//! per provider and process, so macOS shows no new prompt and nothing waits on one.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};

use sasacode_ai::ProviderConfig;

fn cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// API key from the provider's env var, else the OS keychain (service "sasacode", account =
/// provider).
pub async fn resolve_api_key(provider: &str, cfg: Option<&ProviderConfig>) -> Option<String> {
    if let Some(var) = cfg.and_then(|c| c.api_key_env.as_deref())
        && let Ok(v) = std::env::var(var)
        && !v.is_empty()
    {
        sasacode_plugin_api::hide_secrets([&v]);
        return Some(v);
    }
    if let Some(hit) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(provider) {
        return hit.clone();
    }
    let key = keychain(provider).await;
    sasacode_plugin_api::hide_secrets(&key);
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(provider.to_string(), key.clone());
    key
}

/// Whether the keychain was already asked for `provider` this process and had a key (never asks it).
pub fn cached_key(provider: &str) -> bool {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(provider)
        .is_some_and(Option::is_some)
}

async fn keychain(account: &str) -> Option<String> {
    let cmd: &[&str] = if cfg!(target_os = "macos") {
        &[
            "security",
            "find-generic-password",
            "-s",
            "sasacode",
            "-a",
            account,
            "-w",
        ]
    } else if cfg!(target_os = "linux") {
        &["secret-tool", "lookup", "service", "sasacode", "account", account]
    } else {
        return None;
    };
    let mut c = tokio::process::Command::new(cmd[0]);
    c.args(&cmd[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let out = c.output().await.ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}
