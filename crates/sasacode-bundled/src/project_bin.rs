//! Programs a plugin starts on its own, without asking: never one that lives inside the project.
//! Anything the model can write there (a `bun` or `browsr-agent` in the working directory, a
//! `node_modules/.bin` entry) would run with the user's rights. A PATH with `.` or an empty entry
//! puts the working directory on the search path, so a lookup must check where the program is, not
//! trust the name.

use std::path::{Path, PathBuf};

use sasacode_plugin_api::win32_path::without_verbatim;

/// `p` with its links resolved, spelled as programs take it (no `\\?\` on Windows); `p` itself
/// when it cannot be resolved.
pub(crate) fn real_path(p: &Path) -> PathBuf {
    match std::fs::canonicalize(p) {
        Ok(real) => PathBuf::from(without_verbatim(&real.to_string_lossy())),
        Err(_) => p.to_path_buf(),
    }
}

/// The project: the working directory as given, and with its links resolved.
pub(crate) struct ProjectRoot {
    pub(crate) root: PathBuf,
    pub(crate) root_real: PathBuf,
}

impl ProjectRoot {
    pub(crate) fn new(root: &Path) -> Self {
        ProjectRoot {
            root: root.to_path_buf(),
            root_real: real_path(root),
        }
    }

    /// `found` with its links resolved, unless that is inside the project.
    pub(crate) fn outside(&self, found: &Path) -> Option<PathBuf> {
        let real = real_path(found);
        (!real.starts_with(&self.root_real) && !real.starts_with(&self.root)).then_some(real)
    }

    /// `found` as it is, when neither it nor what it links to is in the project: a version
    /// manager's shim goes by the name it is started under.
    pub(crate) fn as_found(&self, found: PathBuf) -> Option<PathBuf> {
        let dir_inside = found
            .parent()
            .is_some_and(|dir| real_path(dir).starts_with(&self.root_real));
        (!dir_inside && !found.starts_with(&self.root) && self.outside(&found).is_some()).then_some(found)
    }

    /// The first program called `name` on PATH that is not in the project, as an absolute path to
    /// start it by (so that starting it searches nothing again).
    pub(crate) fn find(&self, name: &str) -> Option<PathBuf> {
        self.first_outside(which::which_all(name).ok()?)
    }

    /// The first of `found` (what a PATH search turned up, relative entries included) that is not
    /// in the project: a program in the project is skipped, not taken as the answer.
    fn first_outside(&self, found: impl Iterator<Item = PathBuf>) -> Option<PathBuf> {
        found
            .map(|f| if f.is_absolute() { f } else { self.root.join(f) })
            .find_map(|f| self.as_found(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_in_the_project_is_not_found_even_when_path_names_the_project() {
        let dir = tempfile::Builder::new()
            .prefix("sasacode-project-bin-")
            .tempdir()
            .expect("tempdir");
        let root = dir.path();
        let p = ProjectRoot::new(root);
        let local = root.join("node_modules").join(".bin");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("bun"), "").unwrap();
        assert_eq!(p.outside(&local.join("bun")), None);
        assert_eq!(p.as_found(local.join("bun")), None);
        assert_eq!(p.outside(&root.join("missing")), None);
        assert!(p.outside(&std::env::temp_dir().join("sasacode-not-there")).is_some());

        // `.` or an empty entry in PATH: the relative hit is the project's, the next one is taken.
        let elsewhere = tempfile::Builder::new()
            .prefix("sasacode-project-bin-out-")
            .tempdir()
            .expect("tempdir");
        let good = elsewhere.path().join("tool");
        std::fs::write(&good, "").unwrap();
        std::fs::write(root.join("tool"), "").unwrap();
        let hits = || vec![PathBuf::from("tool"), root.join("tool"), good.clone()].into_iter();
        assert_eq!(p.first_outside(hits()), Some(good.clone()));
        assert_eq!(
            p.first_outside(vec![PathBuf::from("tool"), root.join("tool")].into_iter()),
            None
        );
    }
}
