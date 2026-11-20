//! agents-md (`packages/bundled/src/agents-md.ts`): project instructions (AGENTS.md; CLAUDE.md is
//! intentionally not read). Re-read when a file changes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use futures::future::BoxFuture;

use sasacode_plugin_api::{PluginApi, SystemPromptResult, hook, sasacode_home};

use crate::repair::js::js_trim;

/// AGENTS.md files that apply to `cwd`: the global one (in `home`, `sasacode_home()` in the
/// plugin), then from the repository root down to `cwd`.
pub fn agents_files(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    let mut chain: Vec<&Path> = Vec::new();
    let mut dir = cwd;
    loop {
        chain.insert(0, dir);
        if dir.join(".git").exists() {
            break;
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    let global = home.join("AGENTS.md");
    // A project's AGENTS.md is read only as a file of its own: a link there could put any file the
    // user can read into the prompt before anything is approved. The global one is the user's own.
    let own_file = |p: &PathBuf| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file());
    std::iter::once(global.clone())
        .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file()))
        .chain(chain.iter().map(|d| d.join("AGENTS.md")).filter(own_file))
        .collect()
}

fn mtime_ms(p: &Path) -> f64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    plugin_with(api, None)
}

/// [`plugin`] with the global AGENTS.md looked for in `home` instead of `sasacode_home()`.
pub fn plugin_with(api: PluginApi, home: Option<PathBuf>) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let cache: Arc<Mutex<(String, String)>> = Arc::default();
        let cwd = api.cwd();
        api.on::<hook::SystemPrompt, _, _>(move |ev, _c| {
            let home = home.clone().unwrap_or_else(sasacode_home);
            let files = agents_files(&cwd, &home);
            let out = (!files.is_empty()).then(|| {
                let key = files
                    .iter()
                    .map(|f| format!("{}@{}", f.display(), mtime_ms(f)))
                    .collect::<Vec<_>>()
                    .join("|");
                let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
                if key != c.0 {
                    let text = files
                        .iter()
                        .map(|f| {
                            let body = std::fs::read(f)
                                .map(|b| String::from_utf8_lossy(&b).into_owned())
                                .unwrap_or_default();
                            format!("Instructions from {}:\n{}", f.display(), js_trim(&body))
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    *c = (key, text);
                }
                let prompt = &ev.prompt;
                let prompt = match prompt.find("\n\nEnvironment:") {
                    None => format!("{prompt}\n\n{}", c.1),
                    Some(at) => format!("{}\n\n{}{}", &prompt[..at], c.1, &prompt[at..]),
                };
                SystemPromptResult { prompt: Some(prompt) }
            });
            async move { Ok(out) }
        });
        Ok(())
    })
}
