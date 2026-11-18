//! The write tool (`packages/tools/src/write.ts`).

use std::path::Path;

use async_trait::async_trait;
use serde_json::{Value, json};

use sasacode_plugin_api::{JsonObject, Tool, ToolContext, ToolKind, ToolResult};

use crate::util::{fs_error, js_string_arg, line_diff, resolve_path, split_lines, str_arg};

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }
    fn description(&self) -> String {
        "Create a file or overwrite it entirely. Parent directories are created as needed. Prefer edit for changes to existing files.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, absolute or relative to the working directory" },
                "content": { "type": "string", "description": "Full file contents" }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }
    fn always_load(&self) -> bool {
        true
    }
    fn paths(&self, args: &JsonObject, cwd: &Path) -> Option<Vec<String>> {
        Some(vec![resolve_path(str_arg(args, "path"), cwd)])
    }
    fn summary(&self, args: &JsonObject) -> Option<String> {
        Some(js_string_arg(args, "path"))
    }
    async fn execute(&self, args: JsonObject, ctx: ToolContext) -> anyhow::Result<ToolResult> {
        // Checked again after every wait: an interrupt must stop changes that have not started yet.
        let interrupted = || Ok(ToolResult::error("Interrupted by the user; nothing was written."));
        if ctx.signal.is_cancelled() {
            return interrupted();
        }
        let path = resolve_path(str_arg(&args, "path"), &ctx.cwd);
        let content = str_arg(&args, "content");
        // Read as UTF-8 text the way Node's readFile(path, "utf8") does: undecodable bytes become U+FFFD.
        let before = tokio::fs::read(&path)
            .await
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        if ctx.signal.is_cancelled() {
            return interrupted();
        }
        if before.as_deref() == Some(content) {
            let mut details = JsonObject::new();
            details.insert("path".into(), json!(path));
            details.insert("changed".into(), json!(false));
            return Ok(ToolResult::text(format!("Unchanged {path}; nothing was written.")).with_details(details));
        }
        if let Some(dir) = Path::new(&path).parent().filter(|d| !d.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(dir).await.map_err(|e| {
                // Windows says "already exists" for a file anywhere in the way; Node's mkdir
                // says ENOTDIR when it is above the folder.
                if cfg!(windows) && e.kind() == std::io::ErrorKind::AlreadyExists && !dir.exists() {
                    return anyhow::anyhow!("ENOTDIR: not a directory, mkdir '{}'", dir.display());
                }
                fs_error(&e, "mkdir", &dir.display().to_string())
            })?;
        }
        if ctx.signal.is_cancelled() {
            return interrupted();
        }
        tokio::fs::write(&path, content)
            .await
            .map_err(|e| fs_error(&e, "open", &path))?;
        // Counted as read counts them: a final newline ends the last line, it does not start one.
        let lines = if content.is_empty() {
            0
        } else {
            split_lines(content).len()
        };
        let verb = if before.is_none() { "Created" } else { "Overwrote" };
        let mut details = JsonObject::new();
        details.insert("path".into(), json!(path));
        details.insert("changed".into(), json!(true));
        details.insert("created".into(), json!(before.is_none()));
        if let Some(before) = &before {
            details.insert("diff".into(), json!(line_diff(before, content)));
        }
        Ok(ToolResult::text(format!("{verb} {path} ({lines} lines).")).with_details(details))
    }
}
