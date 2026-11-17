//! The system prompt (`packages/agent/src/system-prompt.ts`): facts only (A1), who we are and
//! where we run. No instructions on how to think.

use std::path::Path;

/// Node's `process.platform` for this OS.
pub fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// Node's `process.arch` for this CPU.
pub fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        "x86" => "ia32",
        other => other,
    }
}

pub fn build_system_prompt(cwd: &str, append: &[String]) -> String {
    let mut lines: Vec<String> =
        vec!["You are sasacode, a coding agent running in the user's terminal. You act through the provided tools.".into()];
    for a in append {
        if !a.trim().is_empty() {
            lines.push(String::new());
            lines.push(a.trim().to_string());
        }
    }
    // Stable instructions precede session facts, so a different cwd does not cut their cache prefix.
    let git = if Path::new(cwd).join(".git").exists() { "yes" } else { "no" };
    lines.push(String::new());
    lines.push("Environment:".into());
    lines.push(format!("- Platform: {} ({})", node_platform(), node_arch()));
    lines.push(format!("- Git repository: {git}"));
    lines.push(format!("- Date: {}", chrono::Utc::now().format("%Y-%m-%d")));
    lines.push(format!("- Working directory: {cwd}"));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facts_only_with_appended_instructions_before_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().to_string();
        let p = build_system_prompt(&cwd, &["  Follow AGENTS.md  ".into(), "   ".into()]);
        let lines: Vec<&str> = p.lines().collect();
        assert!(lines[0].starts_with("You are sasacode"));
        assert_eq!(lines[2], "Follow AGENTS.md");
        assert_eq!(lines[4], "Environment:");
        assert!(p.contains("- Git repository: no"));
        assert!(p.ends_with(&format!("- Working directory: {cwd}")));
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert!(build_system_prompt(&cwd, &[]).contains("- Git repository: yes"));
    }
}
