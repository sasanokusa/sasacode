//! `sasacode login`, `sasacode web-search` and `sasacode gui` (`packages/cli/src/main.ts`).

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use sasacode_bundled::gui::{GuiTarget, open_gui, own_program};
use sasacode_bundled::openai_codex::{codex_login, codex_logout, codex_status};
use sasacode_bundled::rustxng_install::{InstallOptions, install_rustxng};
use sasacode_host::t;

use crate::config::{Settings, load_config};

/// `sasacode login [status|logout]`: a ChatGPT plan instead of an API key.
pub async fn login(sub: Option<&str>) -> anyhow::Result<i32> {
    match sub {
        Some("status") => {
            let s = codex_status();
            println!("{}", s.text);
            Ok(if s.ok { 0 } else { 1 })
        }
        Some("logout") => {
            println!("{}", codex_logout());
            Ok(0)
        }
        Some(sub) => anyhow::bail!(t!("unknown login subcommand \"{sub}\" (status, logout)", sub = sub)),
        None => {
            codex_login().await?;
            Ok(0)
        }
    }
}

/// `sasacode web-search install [tag]`: rustxng into ~/.sasacode/bin, for the web_search tool.
pub async fn web_search(args: &[String]) -> anyhow::Result<i32> {
    if args.first().map(String::as_str) != Some("install") {
        let sub = args.first().map(String::as_str).unwrap_or("");
        anyhow::bail!(t!("unknown web-search subcommand \"{sub}\" (install)", sub = sub));
    }
    let installed = install_rustxng(InstallOptions {
        tag: args.get(1).cloned(),
        log: Some(Arc::new(|m: &str| println!("{m}"))),
        ..Default::default()
    })
    .await?;
    println!(
        "{}",
        t!(
            "installed {version}: {path} (web_search uses it from the next request)",
            version = installed.version,
            path = installed.path
        )
    );
    Ok(0)
}

/// `sasacode gui [folder]`: the desktop window (sasacode-gui) on a folder, left running on its own.
pub async fn gui(args: &[String]) -> anyhow::Result<i32> {
    if args.len() > 1 || args.first().is_some_and(|a| a.starts_with('-')) {
        anyhow::bail!(t!("usage: sasacode gui [folder]"));
    }
    let cwd = resolve(args.first().map(String::as_str).unwrap_or("."));
    if !cwd.is_dir() {
        anyhow::bail!(t!("{path} is not a folder", path = cwd.display()));
    }
    // The program to start is the user's to name: a project's config is not asked (it is not trusted here).
    let config = load_config(&cwd, false, None)?.config;
    let command = Settings::of(&config)
        .plugin_settings("gui")
        .get("command")
        .and_then(|c| c.as_str())
        .map(str::to_string);
    let target = GuiTarget {
        cwd: cwd.to_string_lossy().into_owned(),
        session: None,
        program: own_program(),
    };
    open_gui(target, command, None).await?;
    Ok(0)
}

/// Node's `path.resolve(p)`: absolute, with `.` and `..` worked out from the text alone.
pub fn resolve(p: &str) -> PathBuf {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let joined = base.join(Path::new(p));
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                if out.parent().is_some() {
                    out.pop();
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::resolve;

    #[test]
    fn resolve_works_out_dots_from_the_text() {
        let here = std::env::current_dir().unwrap();
        assert_eq!(resolve("."), here);
        assert_eq!(resolve("a/./b/../c"), here.join("a/c"));
        // On Windows `/y` is on the working directory's drive.
        let root: std::path::PathBuf = here.components().take(if cfg!(windows) { 2 } else { 1 }).collect();
        assert_eq!(resolve("/x/../../y"), root.join("y"));
    }
}
