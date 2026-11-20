//! The options every git the shipped plugins run is given, so that what a repository's own
//! config names is not started by merely looking at it. `sasacode-gui` gives the same ones
//! (`crates/sasacode-gui/src/git.rs`), and its tests compare the two.

/// Before the subcommand: no file-system monitor started from the repository's config, and no
/// bare repository found in the folder unless one is named.
pub const GUARD: [&str; 4] = ["-c", "core.fsmonitor=false", "-c", "safe.bareRepository=explicit"];

/// After a subcommand that shows a diff (`diff`, `show`, `log -p`): no external diff program and
/// no textconv filter from the repository's config.
pub const NO_DIFF_HELPERS: [&str; 2] = ["--no-ext-diff", "--no-textconv"];

/// `git` with [`GUARD`] given, and nothing else set yet.
pub fn command() -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(GUARD);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_git_is_told_to_start_no_monitor_and_to_find_no_bare_repository() {
        let cmd = command();
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            ["-c", "core.fsmonitor=false", "-c", "safe.bareRepository=explicit"]
        );
        assert_eq!(cmd.as_std().get_program(), "git");
        assert_eq!(NO_DIFF_HELPERS, ["--no-ext-diff", "--no-textconv"]);
    }
}
