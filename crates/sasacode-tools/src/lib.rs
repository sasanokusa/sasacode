//! The built-in tools: read, write, edit, bash (`packages/tools`). They register through the same
//! plugin API as any bundled plugin (P2), under the plugin name [`PLUGIN_NAME`].

pub mod bash;
pub mod edit;
pub mod read;
pub mod shell;
pub mod util;
pub mod write;

use std::sync::Arc;

use sasacode_plugin_api::{PluginApi, ToolRef};

pub use bash::BashTool;
pub use edit::EditTool;
pub use read::ReadTool;
pub use shell::{KillSignal, bash_path, git_bash_candidates, git_bash_candidates_with, kill_tree};
pub use util::{line_diff, line_diff_with_context, resolve_path, split_lines};
pub use write::WriteTool;

/// The name packages/cli loaded the built-in tools under (`host.load("builtin-tools", ...)`).
pub const PLUGIN_NAME: &str = "builtin-tools";

/// read, write, edit and bash, in the order they were registered.
pub fn builtin_tools() -> Vec<ToolRef> {
    vec![
        Arc::new(ReadTool),
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(BashTool),
    ]
}

/// The built-in tools as a plugin: registers each of them.
pub async fn plugin(api: PluginApi) -> anyhow::Result<()> {
    for t in builtin_tools() {
        api.register_tool(t);
    }
    Ok(())
}
