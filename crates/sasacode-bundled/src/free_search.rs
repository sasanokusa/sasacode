//! free-search: Local-first, no-API-key web search MCP server. Multi-engine web search, smart
//! fetching, document reading. Runs `uvx free-search-mcp` as the MCP server "free-search" (off by
//! default; uv must be installed).

use futures::future::BoxFuture;

use sasacode_mcp::{McpServerConfig, McpServers, mcp_plugin};
use sasacode_plugin_api::PluginApi;

/// The one server this plugin connects.
pub fn servers() -> McpServers {
    let mut servers = McpServers::new();
    servers.insert(
        "free-search".to_string(),
        McpServerConfig::stdio("uvx", vec!["free-search-mcp".into()]),
    );
    servers
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(mcp_plugin(api, servers()))
}
