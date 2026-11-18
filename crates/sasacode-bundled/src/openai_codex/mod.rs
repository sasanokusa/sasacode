//! Models from a ChatGPT plan (openai-codex/…), signed in with `sasacode login` instead of an API
//! key (`packages/bundled/src/openai-codex`). The prompt cache is keyed by session, so a
//! conversation keeps hitting it.

pub mod auth;
pub mod login;
pub mod provider;

use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;

use sasacode_plugin_api::{PluginApi, hook};

pub use auth::{CodexTokens, codex_auth_path, codex_auth_path_in, read_codex_auth, read_codex_auth_at};
pub use login::{
    CodexStatus, LoginDeps, codex_login, codex_login_with, codex_logout, codex_logout_at, codex_status, codex_status_at,
};
pub use provider::{CodexProvider, ProviderDeps, codex_provider, codex_provider_with};

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let session_id = Arc::new(Mutex::new(uuid::Uuid::new_v4().to_string()));
        let set = session_id.clone();
        api.on::<hook::SessionStart, _, _>(move |e, _ctx| {
            if let Some(id) = e.session_id.filter(|id| !id.is_empty()) {
                *set.lock().unwrap_or_else(|p| p.into_inner()) = id;
            }
            async { Ok(None) }
        });
        let Some(config) = sasacode_host::builtin_providers().get("openai-codex").cloned() else {
            anyhow::bail!("no built-in provider openai-codex");
        };
        let current = move || session_id.lock().unwrap_or_else(|p| p.into_inner()).clone();
        api.register_provider(
            "openai-codex",
            config,
            Some(Arc::new(codex_provider(Arc::new(current)))),
        )?;
        Ok(())
    })
}
