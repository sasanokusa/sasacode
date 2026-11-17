//! One trust decision per project covers everything in it that changes where your data goes or
//! runs code as you: the elevated part of .sasacode/config.json (endpoints, MCP servers, plugin
//! settings, allow rules, looser modes). Asked once, and again when any of it changes
//! (`packages/cli/src/trust.ts`).
//!
//! Project plugins (`.sasacode/plugins`) are no longer loaded (ADR-002), so they are no longer part
//! of the decision. The fingerprint is computed as before over `{elevated, manifests: []}`, so a
//! project trusted by the TS version (one without project plugins) stays trusted.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use sasacode_host::t;

use crate::config::{ConfigFiles, describe_elevated, load_config, read_config_files, sasacode_home};
use crate::json::{read_json_or, write_json};

pub struct ProjectTrust {
    /// What trusting would enable; empty when the project needs no trust.
    pub items: Vec<String>,
    pub fingerprint: String,
    /// False when what the decision covers could not all be read; a remembered decision then
    /// cannot vouch for it. Always true now that no plugin code is hashed.
    pub complete: bool,
    /// Read for this check and handed on to setup, so startup reads them once.
    pub files: ConfigFiles,
    pub warnings: Vec<String>,
}

fn trust_file_in(home: &Path) -> PathBuf {
    home.join("trust.json")
}

fn read_trust(home: &Path) -> IndexMap<String, String> {
    read_json_or(&trust_file_in(home), IndexMap::new())
}

pub fn assess_project(cwd: &Path) -> anyhow::Result<ProjectTrust> {
    Ok(assess_files(cwd, read_config_files(cwd)?))
}

/// [`assess_project`] with the config files already read.
pub fn assess_files(cwd: &Path, files: ConfigFiles) -> ProjectTrust {
    let elevated = match load_config(cwd, false, Some(files.clone())) {
        Ok(l) => l.elevated,
        Err(_) => Default::default(),
    };
    let items = describe_elevated(&elevated);
    let manifests: Vec<Value> = Vec::new();
    let digest = Sha256::digest(sasacode_ai::json::stringify(&json!({ "elevated": elevated, "manifests": manifests })));
    let fingerprint: String = digest.iter().map(|b| format!("{b:02x}")).collect::<String>()[..16].to_string();
    ProjectTrust { items, fingerprint, complete: true, files, warnings: Vec::new() }
}

pub fn is_trusted(cwd: &str, trust: &ProjectTrust) -> bool {
    is_trusted_in(&sasacode_home(), cwd, trust)
}

pub fn is_trusted_in(home: &Path, cwd: &str, trust: &ProjectTrust) -> bool {
    if trust.items.is_empty() {
        return true;
    }
    let listed = trust
        .files
        .global
        .get("trustedProjects")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|p| p.as_str() == Some(cwd)));
    listed || (trust.complete && read_trust(home).get(cwd) == Some(&trust.fingerprint))
}

pub fn save_trust(cwd: &str, trust: &ProjectTrust) -> std::io::Result<()> {
    save_trust_in(&sasacode_home(), cwd, trust)
}

pub fn save_trust_in(home: &Path, cwd: &str, trust: &ProjectTrust) -> std::io::Result<()> {
    let mut all = read_trust(home);
    all.insert(cwd.to_string(), trust.fingerprint.clone());
    write_json(&trust_file_in(home), &all)
}

/// Ask on the terminal before anything from the project is used.
pub async fn ask_trust(cwd: &str, trust: &ProjectTrust) -> bool {
    let mut text = format!(
        "\n{}\n{}\n{}\n",
        t!("This project wants to use:"),
        trust.items.iter().map(|i| format!("  - {i}")).collect::<Vec<_>>().join("\n"),
        t!("Plugins and MCP servers run code as you; endpoints receive your conversation and code.")
    );
    if !trust.complete {
        text.push_str(&t!("The plugins' code could not all be read (too many files, unreadable, or a link loop), so you will be asked every time."));
        text.push('\n');
    }
    text.push_str(&t!("Trust {cwd}? [y/N] ", cwd = cwd));
    eprint!("{text}");
    let answer = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        line
    })
    .await
    .unwrap_or_default();
    let answer = answer.trim().to_lowercase();
    answer == "y" || answer == "yes"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::read_config_files_from;

    fn assess(home: &Path, proj: &Path) -> ProjectTrust {
        assess_files(proj, read_config_files_from(&home.join("config.json"), proj).unwrap())
    }

    /// packages/cli/test/plugins.test.ts "one trust decision covers project plugins and elevated
    /// config; it is remembered until they change", without the plugin parts (ADR-002).
    #[test]
    fn one_trust_decision_is_remembered_until_the_elevated_config_changes() {
        let dir = tempfile::tempdir().unwrap();
        let (home, proj) = (dir.path().join("home"), dir.path().join("proj"));
        std::fs::create_dir_all(proj.join(".sasacode")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let cwd = proj.to_string_lossy().to_string();
        std::fs::write(proj.join(".sasacode/config.json"), r#"{"mcpServers":{"fx":{"command":"true"}}}"#).unwrap();
        let t = assess(&home, &proj);
        assert_eq!(t.items.len(), 1);
        assert!(t.items[0].ends_with("fx"));
        assert!(!is_trusted_in(&home, &cwd, &t));
        save_trust_in(&home, &cwd, &t).unwrap();
        assert!(is_trusted_in(&home, &cwd, &assess(&home, &proj)));
        // A change to what the project would turn on changes the fingerprint: asked again.
        std::fs::write(proj.join(".sasacode/config.json"), r#"{"mcpServers":{"fx":{"command":"false"}}}"#).unwrap();
        assert!(!is_trusted_in(&home, &cwd, &assess(&home, &proj)));
        // The legacy trustedProjects list in the global config still counts.
        std::fs::write(home.join("config.json"), serde_json::to_string(&json!({ "trustedProjects": [cwd] })).unwrap())
            .unwrap();
        assert!(is_trusted_in(&home, &cwd, &assess(&home, &proj)));
        // A project with nothing risky needs no trust.
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(assess(&home, &plain).items.is_empty());
    }

    /// The TS wrote this fingerprint for a project with `{"mcpServers":{"fx":{"command":"true"}}}`
    /// and no plugins (bun: sha256 of JSON.stringify({elevated, manifests: []}), first 16 hex).
    #[test]
    fn the_fingerprint_is_the_one_the_ts_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let (home, proj) = (dir.path().join("home"), dir.path().join("proj"));
        std::fs::create_dir_all(proj.join(".sasacode")).unwrap();
        std::fs::write(
            proj.join(".sasacode/config.json"),
            r#"{"mcpServers":{"fx":{"command":"true"}},"permissions":{"allow":["bash"],"mode":"auto"},"maxTurns":500}"#,
        )
        .unwrap();
        assert_eq!(assess(&home, &proj).fingerprint, TS_FINGERPRINT);
    }

    const TS_FINGERPRINT: &str = "79f418fd42aaabef";
}
