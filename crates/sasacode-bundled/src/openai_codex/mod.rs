//! (being ported)

pub mod auth;
pub mod login;
pub mod provider;

use sasacode_plugin_api::PluginApi;

pub fn plugin(_api: PluginApi) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async { Ok(()) })
}
