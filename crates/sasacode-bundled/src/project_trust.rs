//! Which projects the CLI trusts, for the shipped plugins that would otherwise run programs a
//! project names: git reads its repository's config. The CLI says so as it sets a project up (a
//! project with nothing that needs trust is trusted); one it has not spoken of is not.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static TRUSTED: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// The project at `cwd` (as the plugin host is given it) is trusted, or no longer.
pub fn set(cwd: &Path, trusted: bool) {
    let mut all = TRUSTED.lock().unwrap_or_else(|e| e.into_inner());
    if trusted {
        all.insert(cwd.to_path_buf());
    } else {
        all.remove(cwd);
    }
}

pub fn is_trusted(cwd: &Path) -> bool {
    TRUSTED.lock().unwrap_or_else(|e| e.into_inner()).contains(cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_is_trusted_once_said_so_and_not_before() {
        let cwd = Path::new("/nowhere/sasacode-trust-test");
        assert!(!is_trusted(cwd));
        set(cwd, true);
        assert!(is_trusted(cwd) && !is_trusted(Path::new("/nowhere")));
        set(cwd, false);
        assert!(!is_trusted(cwd));
    }
}
